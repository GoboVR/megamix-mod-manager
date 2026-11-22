//! A mod's own settings: the keys in its `config.toml` other than
//! DivaModLoader's (`enabled`, `include`, `name`, …), at the top level or in
//! `[sections]`. Each comes with the comment written above it, which is how
//! mods document their options.
//!
//! Editing keeps the file's formatting: only the value changes, its type is
//! kept, and comments stay where they are.

use serde::Serialize;
use toml_edit::{Item, Table, Value};

use crate::config::{MOD_META_KEYS, ModConfig};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bool,
    Int,
    Float,
    Text,
    /// Arrays, inline tables, dates: shown, not edited here.
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Setting {
    /// `[section]` the key is in (None = top level).
    pub section: Option<String>,
    pub key: String,
    pub kind: Kind,
    /// Current value as text (strings without quotes).
    pub value: String,
    /// Comment lines above the key (and after the value), without `#`.
    pub comment: String,
}

impl Setting {
    /// "section.key" or "key", for display and the CLI.
    pub fn path(&self) -> String {
        match &self.section {
            Some(s) => format!("{s}.{}", self.key),
            None => self.key.clone(),
        }
    }
}

fn kind_of(v: &Value) -> Kind {
    match v {
        Value::Boolean(_) => Kind::Bool,
        Value::Integer(_) => Kind::Int,
        Value::Float(_) => Kind::Float,
        Value::String(_) => Kind::Text,
        _ => Kind::Other,
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.value().clone(),
        Value::Boolean(b) => b.value().to_string(),
        Value::Integer(i) => i.value().to_string(),
        Value::Float(f) => f.value().to_string(),
        other => other.to_string().trim().to_owned(),
    }
}

/// Comment text from a decor string ("# a\n# b\n" → "a\nb").
fn comment_text(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix('#'))
        .map(|l| l.strip_prefix(' ').unwrap_or(l).trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn collect(table: &Table, section: Option<&str>, table_comment: &str, out: &mut Vec<Setting>) {
    let mut first = true;
    for (k, item) in table.iter() {
        if section.is_none() && MOD_META_KEYS.contains(&k) {
            continue;
        }
        match item {
            Item::Value(v) => {
                let above = table
                    .key(k)
                    .and_then(|key| key.leaf_decor().prefix())
                    .and_then(|r| r.as_str())
                    .map(comment_text)
                    .unwrap_or_default();
                let after = v
                    .decor()
                    .suffix()
                    .and_then(|r| r.as_str())
                    .map(comment_text)
                    .unwrap_or_default();
                let mut comment = [above, after]
                    .into_iter()
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                // The section's own comment belongs with its first key.
                if first && !table_comment.is_empty() {
                    comment = if comment.is_empty() {
                        table_comment.to_owned()
                    } else {
                        format!("{table_comment}\n{comment}")
                    };
                }
                first = false;
                out.push(Setting {
                    section: section.map(str::to_owned),
                    key: k.to_owned(),
                    kind: kind_of(v),
                    value: text_of(v),
                    comment,
                });
            }
            Item::Table(t) if section.is_none() => {
                let c = t
                    .decor()
                    .prefix()
                    .and_then(|r| r.as_str())
                    .map(comment_text)
                    .unwrap_or_default();
                collect(t, Some(k), &c, out);
            }
            _ => {}
        }
    }
}

/// The mod's settings, in file order.
pub fn list(cfg: &ModConfig) -> Vec<Setting> {
    let mut out = Vec::new();
    collect(cfg.file.doc().as_table(), None, "", &mut out);
    out
}

/// Parse `text` as a new value for a setting of `kind`.
pub fn parse_value(kind: Kind, key: &str, text: &str) -> Result<Value> {
    let t = text.trim();
    let bad = |what: &str| Error::Other(format!("`{key}` needs {what}, not “{t}”"));
    Ok(match kind {
        Kind::Bool => match t.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Value::from(true),
            "false" | "off" | "no" | "0" => Value::from(false),
            _ => return Err(bad("true or false")),
        },
        Kind::Int => Value::from(t.parse::<i64>().map_err(|_| bad("a whole number"))?),
        Kind::Float => {
            let f = t.parse::<f64>().map_err(|_| bad("a number"))?;
            if !f.is_finite() {
                return Err(bad("a number"));
            }
            Value::from(f)
        }
        Kind::Text => Value::from(text),
        Kind::Other => {
            return Err(Error::Other(format!(
                "`{key}` is a list or table; edit it in the mod's config.toml"
            )));
        }
    })
}

/// Change one setting (`path` = "key" or "section.key"), keeping its type
/// and the formatting around it. Call `Game::save` afterwards (backup).
pub fn set(cfg: &mut ModConfig, path: &str, text: &str) -> Result<()> {
    let s = list(cfg)
        .into_iter()
        .find(|s| s.path() == path)
        .ok_or_else(|| Error::Other(format!("this mod has no setting `{path}`")))?;
    let mut value = parse_value(s.kind, &s.key, text)?;
    let doc = cfg.file.doc_mut();
    let table = match &s.section {
        Some(sec) => doc
            .get_mut(sec)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| Error::Other(format!("section [{sec}] is gone")))?,
        None => doc.as_table_mut(),
    };
    // Replace the value in place: the key (and the comment above it) stays.
    let Some(item @ Item::Value(_)) = table.get_mut(&s.key) else {
        return Err(Error::Other(format!("`{path}` is gone")));
    };
    let old = item.as_value().expect("value");
    if text_of(old) == text_of(&value) && kind_of(old) == kind_of(&value) {
        return Ok(());
    }
    *value.decor_mut() = old.decor().clone();
    *item = Item::Value(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TomlFile;

    fn cfg(text: &str) -> ModConfig {
        ModConfig {
            file: TomlFile::from_bytes("config.toml", text.as_bytes().to_vec()).unwrap(),
        }
    }

    const EXPATCH: &str = "enabled = true\r\ninclude = [\".\"]\r\ndll = [\"ExPatch.dll\"]\r\n\r\nname = \"ExPatch\"\r\nversion = \"2.2\"\r\n\r\n# Saves unlock info to your save data permanently.\r\n# Cannot be undone automatically.\r\npermanent = false\r\n\r\n# Section comment\r\n[graphics]\r\nscale = 1.5 # render scale\r\nlabel = \"hi\"\r\nlevels = [1, 2]\r\n";

    #[test]
    fn lists_settings_with_comments() {
        let s = list(&cfg(EXPATCH));
        let paths: Vec<_> = s.iter().map(Setting::path).collect();
        assert_eq!(
            paths,
            [
                "permanent",
                "graphics.scale",
                "graphics.label",
                "graphics.levels"
            ]
        );
        assert_eq!(s[0].kind, Kind::Bool);
        assert_eq!(
            s[0].comment,
            "Saves unlock info to your save data permanently.\nCannot be undone automatically."
        );
        assert_eq!(s[1].comment, "Section comment\nrender scale");
        assert_eq!(s[2].value, "hi");
        assert_eq!(s[3].kind, Kind::Other);
    }

    #[test]
    fn edits_keep_formatting_and_types() {
        let mut c = cfg(EXPATCH);
        set(&mut c, "permanent", "true").unwrap();
        set(&mut c, "graphics.scale", "2").unwrap();
        set(&mut c, "graphics.label", "ミク \"quoted\"").unwrap();
        let out = String::from_utf8(c.file.to_bytes()).unwrap();
        assert!(
            out.contains("# Cannot be undone automatically.\r\npermanent = true\r\n"),
            "{out}"
        );
        assert!(out.contains("scale = 2.0 # render scale\r\n"), "{out}");
        assert!(out.contains("label = 'ミク \"quoted\"'\r\n"), "{out}");
        let back = list(&cfg(&out));
        assert_eq!(back[2].value, "ミク \"quoted\"");
        assert!(set(&mut c, "graphics.levels", "1").is_err());
        assert!(set(&mut c, "permanent", "maybe").is_err());
        assert!(set(&mut c, "nope", "1").is_err());
        assert!(
            set(&mut c, "enabled", "false").is_err(),
            "loader keys aren't settings"
        );
        // Same value: nothing changes.
        let mut d = cfg(EXPATCH);
        set(&mut d, "permanent", "false").unwrap();
        assert!(!d.file.is_modified());
    }
}
