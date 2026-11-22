//! Format-preserving reading and writing of DivaModLoader `config.toml` files.
//!
//! Guarantees:
//! - Loading and saving a file without changes writes nothing (and if forced,
//!   produces byte-identical output).
//! - Comments, unknown keys, key order, a UTF-8 BOM and the file's line
//!   endings (LF or CRLF) are kept. Lines added by an edit use the file's
//!   existing line ending.
//! - Values are only rewritten when they are explicitly changed; e.g. the
//!   `include` line is never touched by enabling/disabling a mod.
//! - Saves are atomic (write to a sibling temp file, then rename).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Value};

use crate::error::{Error, Result};

const BOM: &str = "\u{feff}";

/// Line-ending style detected in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    /// Both styles appear. Kept byte-for-byte while unedited; once edited the
    /// file is written with whichever ending was more common.
    Mixed {
        crlf_majority: bool,
    },
}

impl LineEnding {
    fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count();
        match (crlf, lf) {
            (0, _) => LineEnding::Lf,
            (c, l) if c == l => LineEnding::CrLf,
            (c, l) => LineEnding::Mixed {
                crlf_majority: c * 2 > l,
            },
        }
    }
}

/// A TOML file loaded for format-preserving edits.
#[derive(Debug, Clone)]
pub struct TomlFile {
    path: PathBuf,
    doc: DocumentMut,
    bom: bool,
    line_ending: LineEnding,
    /// Bytes currently on disk (as last loaded or saved).
    original: Vec<u8>,
    /// `doc.to_string()` matching `original`, to detect edits.
    pristine: String,
}

impl TomlFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
        Self::from_bytes(path, bytes)
    }

    /// Parse `bytes` as if they were read from `path` (the path is used for
    /// error messages and `save`).
    pub fn from_bytes(path: impl Into<PathBuf>, bytes: Vec<u8>) -> Result<Self> {
        let path = path.into();
        let text =
            std::str::from_utf8(&bytes).map_err(|_| Error::NotUtf8 { path: path.clone() })?;
        let (bom, body) = match text.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let line_ending = LineEnding::detect(body);
        // Normalise to LF so that lines added by edits match; restored on write.
        let body = body.replace("\r\n", "\n");
        let doc = body.parse::<DocumentMut>().map_err(|e| Error::Toml {
            path: path.clone(),
            source: Box::new(e),
        })?;
        let pristine = doc.to_string();
        Ok(Self {
            path,
            doc,
            bom,
            line_ending,
            original: bytes,
            pristine,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn has_bom(&self) -> bool {
        self.bom
    }

    pub fn doc(&self) -> &DocumentMut {
        &self.doc
    }

    pub fn doc_mut(&mut self) -> &mut DocumentMut {
        &mut self.doc
    }

    /// Serialise with the original BOM and line endings. Unedited documents
    /// return the original bytes exactly.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = self.doc.to_string();
        if body == self.pristine {
            return self.original.clone();
        }
        let crlf = matches!(
            self.line_ending,
            LineEnding::CrLf
                | LineEnding::Mixed {
                    crlf_majority: true
                }
        );
        let mut out = String::with_capacity(body.len() + 64);
        if self.bom {
            out.push_str(BOM);
        }
        if crlf {
            out.push_str(&body.replace('\n', "\r\n"));
        } else {
            out.push_str(&body);
        }
        out.into_bytes()
    }

    /// True if saving would change the file's bytes.
    pub fn is_modified(&self) -> bool {
        self.doc.to_string() != self.pristine
    }

    /// Atomically write the file if it changed. Returns whether it was written.
    pub fn save(&mut self) -> Result<bool> {
        if !self.is_modified() {
            return Ok(false);
        }
        // Refuse to overwrite changes another program made since we loaded.
        match fs::read(&self.path) {
            Ok(on_disk) if on_disk != self.original => {
                return Err(Error::ChangedOnDisk {
                    path: self.path.clone(),
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&self.path, e)),
        }
        let bytes = self.to_bytes();
        atomic_write(&self.path, &bytes)?;
        self.original = bytes;
        self.pristine = self.doc.to_string();
        Ok(true)
    }

    // ---- typed accessors -------------------------------------------------

    fn wrong_type(&self, key: &str, expected: &'static str) -> Error {
        Error::WrongType {
            path: self.path.clone(),
            key: key.to_owned(),
            expected,
        }
    }

    pub fn get_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.doc.get(key) {
            None => Ok(None),
            Some(item) => item
                .as_bool()
                .map(Some)
                .ok_or_else(|| self.wrong_type(key, "a boolean")),
        }
    }

    pub fn get_str(&self, key: &str) -> Result<Option<&str>> {
        match self.doc.get(key) {
            None => Ok(None),
            Some(item) => item
                .as_str()
                .map(Some)
                .ok_or_else(|| self.wrong_type(key, "a string")),
        }
    }

    /// A string array. A single string is accepted as a one-element array.
    pub fn get_str_array(&self, key: &str) -> Result<Option<Vec<String>>> {
        let Some(item) = self.doc.get(key) else {
            return Ok(None);
        };
        if let Some(s) = item.as_str() {
            return Ok(Some(vec![s.to_owned()]));
        }
        let arr = item
            .as_array()
            .ok_or_else(|| self.wrong_type(key, "an array of strings"))?;
        arr.iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| self.wrong_type(key, "an array of strings"))
    }

    /// Set a root-level value, keeping the formatting around the old value.
    /// Does nothing if the value is already equal.
    pub fn set_value(&mut self, key: &str, mut value: Value) {
        if let Some(Item::Value(old)) = self.doc.get(key) {
            if values_equal(old, &value) {
                return;
            }
            *value.decor_mut() = old.decor().clone();
        }
        match self.doc.get_mut(key) {
            Some(item @ Item::Value(_)) => *item = Item::Value(value),
            _ => {
                self.doc.insert(key, Item::Value(value));
            }
        }
    }

    pub fn set_bool(&mut self, key: &str, v: bool) {
        self.set_value(key, Value::from(v));
    }

    /// Set a string array, reusing the old array's layout (single-line or one
    /// item per line, trailing comma, indentation).
    pub fn set_str_array(&mut self, key: &str, items: &[String]) {
        let new = match self.doc.get(key).and_then(Item::as_array) {
            Some(old) => restyle_array(old, items),
            None => items.iter().map(String::as_str).collect::<Array>(),
        };
        self.set_value(key, Value::Array(new));
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.value() == y.value(),
        (Value::Integer(x), Value::Integer(y)) => x.value() == y.value(),
        (Value::Float(x), Value::Float(y)) => x.value().to_bits() == y.value().to_bits(),
        (Value::Boolean(x), Value::Boolean(y)) => x.value() == y.value(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

fn restyle_array(old: &Array, items: &[String]) -> Array {
    let first_prefix = old
        .get(0)
        .and_then(|v| v.decor().prefix())
        .and_then(|r| r.as_str());
    let later_prefix = old
        .get(1)
        .and_then(|v| v.decor().prefix())
        .and_then(|r| r.as_str());
    let multiline = first_prefix.is_some_and(|p| p.contains('\n'));

    let mut new = Array::new();
    for (i, s) in items.iter().enumerate() {
        let mut v = Value::from(s.as_str());
        let prefix = match (i, multiline) {
            (0, _) => first_prefix.unwrap_or(""),
            (_, true) => later_prefix.or(first_prefix).unwrap_or("\n"),
            (_, false) => later_prefix.unwrap_or(" "),
        };
        v.decor_mut().set_prefix(prefix);
        v.decor_mut().set_suffix("");
        new.push_formatted(v);
    }
    new.set_trailing_comma(old.trailing_comma());
    new.set_trailing(old.trailing().as_str().unwrap_or(""));
    new
}

/// Write `bytes` to `path` via a sibling temp file and rename.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.megamix-tmp"));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| Error::io(path, e))
}

// ---- DivaModLoader-specific views ---------------------------------------

/// The game folder's `config.toml` (DivaModLoader's own settings).
#[derive(Debug, Clone)]
pub struct LoaderConfig {
    pub file: TomlFile,
}

impl LoaderConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            file: TomlFile::load(path)?,
        })
    }

    /// Missing means enabled.
    pub fn enabled(&self) -> Result<bool> {
        Ok(self.file.get_bool("enabled")?.unwrap_or(true))
    }

    pub fn console(&self) -> Result<bool> {
        Ok(self.file.get_bool("console")?.unwrap_or(false))
    }

    /// The mods folder name relative to the game folder (default `mods`).
    pub fn mods_dir(&self) -> Result<String> {
        Ok(self.file.get_str("mods")?.unwrap_or("mods").to_owned())
    }

    /// Load order; the first entry wins. If non-empty, mods not listed are
    /// not loaded by DivaModLoader.
    pub fn priority(&self) -> Result<Vec<String>> {
        Ok(self.file.get_str_array("priority")?.unwrap_or_default())
    }

    pub fn set_priority(&mut self, order: &[String]) {
        self.file.set_str_array("priority", order);
    }

    pub fn set_enabled(&mut self, v: bool) {
        self.file.set_bool("enabled", v);
    }

    pub fn set_console(&mut self, v: bool) {
        self.file.set_bool("console", v);
    }

    pub fn save(&mut self) -> Result<bool> {
        self.file.save()
    }
}

/// Keys with a fixed meaning in a mod's `config.toml`. Everything else is a
/// mod-specific setting.
pub const MOD_META_KEYS: &[&str] = &[
    "enabled",
    "include",
    "dll",
    "name",
    "description",
    "version",
    "date",
    "author",
];

/// A mod's `mods/<Mod>/config.toml`.
#[derive(Debug, Clone)]
pub struct ModConfig {
    pub file: TomlFile,
}

impl ModConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            file: TomlFile::load(path)?,
        })
    }

    /// Missing means enabled.
    pub fn enabled(&self) -> Result<bool> {
        Ok(self.file.get_bool("enabled")?.unwrap_or(true))
    }

    /// A missing `enabled` already means enabled, so enabling doesn't add it.
    pub fn set_enabled(&mut self, v: bool) {
        if v && self.file.doc().get("enabled").is_none() {
            return;
        }
        self.file.set_bool("enabled", v);
    }

    /// Folders whose files replace game files. `None` means no replacement.
    pub fn include(&self) -> Result<Option<Vec<String>>> {
        self.file.get_str_array("include")
    }

    pub fn dll(&self) -> Result<Vec<String>> {
        Ok(self.file.get_str_array("dll")?.unwrap_or_default())
    }

    pub fn name(&self) -> Result<Option<&str>> {
        self.file.get_str("name")
    }

    pub fn description(&self) -> Result<Option<&str>> {
        self.file.get_str("description")
    }

    pub fn version(&self) -> Result<Option<&str>> {
        self.file.get_str("version")
    }

    pub fn date(&self) -> Result<Option<&str>> {
        self.file.get_str("date")
    }

    pub fn author(&self) -> Result<Option<&str>> {
        self.file.get_str("author")
    }

    /// Mod-specific settings (root keys that aren't metadata), in file order.
    pub fn settings(&self) -> impl Iterator<Item = (&str, &Item)> {
        self.file
            .doc()
            .iter()
            .filter(|(k, _)| !MOD_META_KEYS.contains(k))
    }

    pub fn set_setting(&mut self, key: &str, value: Value) {
        self.file.set_value(key, value);
    }

    pub fn save(&mut self) -> Result<bool> {
        self.file.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(s: &str) -> TomlFile {
        TomlFile::from_bytes("test.toml", s.as_bytes().to_vec()).unwrap()
    }

    fn out(f: &TomlFile) -> String {
        String::from_utf8(f.to_bytes()).unwrap()
    }

    #[test]
    fn detects_line_endings() {
        assert_eq!(LineEnding::detect("a\nb\n"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::CrLf);
        assert_eq!(
            LineEnding::detect("a\r\nb\n"),
            LineEnding::Mixed {
                crlf_majority: false
            }
        );
        assert_eq!(
            LineEnding::detect("a\r\nb\r\nc\n"),
            LineEnding::Mixed {
                crlf_majority: true
            }
        );
        assert_eq!(LineEnding::detect("a"), LineEnding::Lf);
    }

    #[test]
    fn crlf_edit_keeps_crlf_everywhere() {
        let mut f = parse("enabled = true\r\n#include = [\"roms\"]\r\ndll = [\"a.dll\"]\r\n");
        f.set_bool("enabled", false);
        f.set_bool("newkey", true);
        assert_eq!(
            out(&f),
            "enabled = false\r\n#include = [\"roms\"]\r\ndll = [\"a.dll\"]\r\nnewkey = true\r\n"
        );
    }

    #[test]
    fn bom_is_kept() {
        let mut f = parse("\u{feff}enabled = true\n");
        assert!(f.has_bom());
        assert!(!f.is_modified());
        f.set_bool("enabled", false);
        assert_eq!(out(&f), "\u{feff}enabled = false\n");
    }

    #[test]
    fn mixed_line_endings_untouched() {
        let src = "a = 1\r\nb = 2\r\nc = 3\n";
        let mut f = parse(src);
        assert_eq!(
            f.line_ending(),
            LineEnding::Mixed {
                crlf_majority: true
            }
        );
        assert!(!f.is_modified());
        assert_eq!(out(&f), src);
        f.set_bool("d", true);
        assert_eq!(out(&f), "a = 1\r\nb = 2\r\nc = 3\r\nd = true\r\n");
    }

    #[test]
    fn setting_same_value_is_noop() {
        let mut f = parse("enabled   =   true # keep\n");
        f.set_bool("enabled", true);
        assert!(!f.is_modified());
    }

    #[test]
    fn set_value_keeps_comment_and_spacing() {
        let mut f = parse("# head\nenabled   =   true # keep\n");
        f.set_bool("enabled", false);
        assert_eq!(out(&f), "# head\nenabled   =   false # keep\n");
    }

    #[test]
    fn priority_single_line_style_kept() {
        let mut f = parse("priority = [\"A\", \"B\"] # order\n");
        f.set_str_array("priority", &["B".into(), "A".into(), "C".into()]);
        assert_eq!(out(&f), "priority = [\"B\", \"A\", \"C\"] # order\n");
    }

    #[test]
    fn priority_multiline_style_kept() {
        let src = "priority = [\n    \"A\",\n    \"B\",\n]\n";
        let mut f = parse(src);
        f.set_str_array("priority", &["B".into(), "C".into(), "A".into()]);
        assert_eq!(
            out(&f),
            "priority = [\n    \"B\",\n    \"C\",\n    \"A\",\n]\n"
        );
    }

    #[test]
    fn single_string_include_is_read_as_array() {
        let f = parse("include = \".\"\n");
        assert_eq!(
            f.get_str_array("include").unwrap(),
            Some(vec![".".to_owned()])
        );
    }

    #[test]
    fn wrong_type_is_an_error() {
        let f = parse("enabled = \"yes\"\n");
        assert!(matches!(
            f.get_bool("enabled"),
            Err(Error::WrongType { .. })
        ));
    }

    #[test]
    fn non_utf8_is_an_error() {
        let r = TomlFile::from_bytes("x.toml", vec![0x82, 0xa0, b'\n']);
        assert!(matches!(r, Err(Error::NotUtf8 { .. })));
    }

    #[test]
    fn mod_enabled_defaults_true_and_include_untouched() {
        let mut m = ModConfig {
            file: parse("include = [ \".\" ,\"rom\" ]\nname = \"X\"\n"),
        };
        assert!(m.enabled().unwrap());
        m.set_enabled(false);
        assert_eq!(
            out(&m.file),
            "include = [ \".\" ,\"rom\" ]\nname = \"X\"\nenabled = false\n"
        );
    }

    #[test]
    fn atomic_save_writes_only_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "enabled = true\r\n").unwrap();
        let mut f = TomlFile::load(&p).unwrap();
        assert!(!f.save().unwrap());
        f.set_bool("enabled", false);
        assert!(f.save().unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), "enabled = false\r\n");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temp file left behind"
        );
        // Another program edits the file: our next save must refuse.
        fs::write(&p, "enabled = true\r\n# edited elsewhere\r\n").unwrap();
        f.set_bool("enabled", true);
        assert!(matches!(f.save(), Err(Error::ChangedOnDisk { .. })));
    }
}
