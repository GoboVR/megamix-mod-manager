//! Reading mod archives (.zip, .7z, .rar) with streaming extraction.
//!
//! - Archives are never loaded into memory; files are decoded straight to disk.
//! - Entry paths are made safe: `\` becomes `/`, absolute paths, drive
//!   letters and `..` are rejected (no writing outside the target folder).
//! - Zip names that aren't UTF-8 are decoded as Shift-JIS when possible
//!   (Japanese Windows zip tools), else CP437.
//! - Every written file's size is checked against the archive's header, and
//!   zip/7z CRCs are verified by the decoders.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Zip,
    SevenZ,
    Rar,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Zip => "ZIP",
            Kind::SevenZ => "7z",
            Kind::Rar => "RAR",
        }
    }
}

/// Supported archive file extensions (lowercase).
pub const EXTENSIONS: &[&str] = &["zip", "7z", "rar"];

pub fn has_archive_extension(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Detect the format from the file's first bytes (extensions lie).
pub fn detect(path: &Path) -> Result<Kind> {
    let mut head = [0u8; 8];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| Error::io(path, e))?;
    let head = &head[..n];
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        Ok(Kind::Zip)
    } else if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Ok(Kind::SevenZ)
    } else if head.starts_with(b"Rar!\x1A\x07") {
        Ok(Kind::Rar)
    } else {
        Err(Error::Archive(format!(
            "{} is not a .zip, .7z or .rar archive",
            path.display()
        )))
    }
}

/// A file or folder inside an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Safe relative path, `/`-separated, no leading/trailing `/`.
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Make an archive entry name safe, or `None` if it must be skipped
/// (escapes the target folder, absolute, empty).
pub fn safe_path(name: &str) -> Option<String> {
    if name.contains('\0') {
        return None;
    }
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for c in name.split('/') {
        match c {
            "" | "." => continue,
            ".." => return None,
            c if c.contains(':') => return None, // drive letters, ADS
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Junk that archivers add and that is never part of a mod.
pub fn is_junk(path: &str) -> bool {
    path.split('/')
        .any(|c| c == "__MACOSX" || c == ".DS_Store" || c.eq_ignore_ascii_case("thumbs.db"))
}

/// List all entries. Unsafe paths are dropped.
pub fn list(path: &Path) -> Result<(Kind, Vec<Entry>)> {
    let kind = detect(path)?;
    let entries = match kind {
        Kind::Zip => list_zip(path)?,
        Kind::SevenZ => list_7z(path)?,
        Kind::Rar => list_rar(path)?,
    };
    Ok((kind, entries))
}

/// Progress of an extraction.
#[derive(Debug, Clone, Copy)]
pub struct Progress<'a> {
    pub done: u64,
    pub total: u64,
    pub file: &'a str,
}

/// Extract the entries for which `dest` returns a target path. Directories
/// are created as needed. `total` is only used for progress reporting.
pub fn extract(
    path: &Path,
    kind: Kind,
    dest: &dyn Fn(&str) -> Option<PathBuf>,
    total: u64,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut ctx = Ctx {
        done: 0,
        total,
        progress,
        cancel,
    };
    match kind {
        Kind::Zip => extract_zip(path, dest, &mut ctx),
        Kind::SevenZ => extract_7z(path, dest, &mut ctx),
        Kind::Rar => extract_rar(path, dest, &mut ctx),
    }
}

struct Ctx<'a> {
    done: u64,
    total: u64,
    progress: &'a mut dyn FnMut(Progress),
    cancel: &'a AtomicBool,
}

impl Ctx<'_> {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Stream `r` into a new file at `target`, checking size and cancel.
    fn write_file(
        &mut self,
        r: &mut dyn Read,
        target: &Path,
        name: &str,
        expected: u64,
    ) -> Result<()> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let f = File::create(target).map_err(|e| Error::io(target, e))?;
        let mut w = BufWriter::with_capacity(1 << 20, f);
        let mut buf = vec![0u8; 1 << 16];
        let mut written = 0u64;
        let mut since_report = 0u64;
        loop {
            self.check_cancel()?;
            let n = match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(Error::Archive(format!(
                        "{name}: {e} (the archive may be damaged or incomplete)"
                    )));
                }
            };
            w.write_all(&buf[..n]).map_err(|e| Error::io(target, e))?;
            written += n as u64;
            self.done += n as u64;
            since_report += n as u64;
            if since_report >= 1 << 20 {
                since_report = 0;
                (self.progress)(Progress {
                    done: self.done,
                    total: self.total,
                    file: name,
                });
            }
        }
        w.flush().map_err(|e| Error::io(target, e))?;
        if written != expected {
            return Err(Error::Archive(format!(
                "{name}: unpacked {written} bytes but the archive says {expected} (size mismatch; the archive may be damaged, try downloading it again)"
            )));
        }
        (self.progress)(Progress {
            done: self.done,
            total: self.total,
            file: name,
        });
        Ok(())
    }
}

// ---- zip ----------------------------------------------------------------

fn open_zip(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>> {
    let f = File::open(path).map_err(|e| Error::io(path, e))?;
    zip::ZipArchive::new(BufReader::new(f))
        .map_err(|e| Error::Archive(format!("{}: {e}", path.display())))
}

/// Decode a zip entry name: UTF-8, else Shift-JIS, else the crate's CP437.
fn zip_name<R: Read>(f: &zip::read::ZipFile<'_, R>) -> String {
    let raw = f.name_raw();
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_owned();
    }
    let (sjis, had_errors) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(raw);
    if !had_errors {
        return sjis.into_owned();
    }
    match f.name() {
        Ok(n) => n.into_owned(),
        Err(_) => String::from_utf8_lossy(raw).into_owned(),
    }
}

fn list_zip(path: &Path) -> Result<Vec<Entry>> {
    let mut z = open_zip(path)?;
    let mut out = Vec::with_capacity(z.len());
    for i in 0..z.len() {
        let f = z
            .by_index_raw(i)
            .map_err(|e| Error::Archive(format!("{}: {e}", path.display())))?;
        if let Some(p) = safe_path(&zip_name(&f)) {
            out.push(Entry {
                path: p,
                size: f.size(),
                is_dir: f.is_dir(),
            });
        }
    }
    Ok(out)
}

fn extract_zip(path: &Path, dest: &dyn Fn(&str) -> Option<PathBuf>, ctx: &mut Ctx) -> Result<()> {
    let mut z = open_zip(path)?;
    for i in 0..z.len() {
        ctx.check_cancel()?;
        let mut f = z
            .by_index(i)
            .map_err(|e| Error::Archive(format!("{}: {e}", path.display())))?;
        if f.encrypted() {
            return Err(Error::Archive(
                "password-protected archives aren't supported".into(),
            ));
        }
        let Some(name) = safe_path(&zip_name(&f)) else {
            continue;
        };
        let Some(target) = dest(&name) else { continue };
        if f.is_dir() {
            fs::create_dir_all(&target).map_err(|e| Error::io(&target, e))?;
            continue;
        }
        let size = f.size();
        ctx.write_file(&mut f, &target, &name, size)?;
    }
    Ok(())
}

// ---- 7z -----------------------------------------------------------------

fn sz_err(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Archive(format!("{}: {e}", path.display()))
}

fn list_7z(path: &Path) -> Result<Vec<Entry>> {
    let r = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .map_err(|e| sz_err(path, e))?;
    Ok(r.archive()
        .files
        .iter()
        .filter(|e| !e.is_anti_item)
        .filter_map(|e| {
            safe_path(&e.name).map(|p| Entry {
                path: p,
                size: e.size,
                is_dir: e.is_directory,
            })
        })
        .collect())
}

fn extract_7z(path: &Path, dest: &dyn Fn(&str) -> Option<PathBuf>, ctx: &mut Ctx) -> Result<()> {
    use sevenz_rust2::EntryDecision;
    let mut r = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
        .map_err(|e| sz_err(path, e))?;
    // Directories (and empty files) have no stream; create them up front.
    for e in r.archive().files.iter().filter(|e| !e.has_stream) {
        if let Some(name) = safe_path(&e.name)
            && let Some(target) = dest(&name)
        {
            if e.is_directory {
                fs::create_dir_all(&target).map_err(|e| Error::io(&target, e))?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                }
                File::create(&target).map_err(|e| Error::io(&target, e))?;
            }
        }
    }
    // Solid archives must be decoded in order; unwanted entries are drained.
    let mut failure: Option<Error> = None;
    let res = r.for_each_entries_with_decision(|entry, reader| {
        if entry.is_directory || !entry.has_stream {
            return Ok(EntryDecision::Continue);
        }
        let target = safe_path(&entry.name).and_then(|n| dest(&n).map(|t| (n, t)));
        if let Some((name, target)) = target
            && let Err(e) = ctx.write_file(reader, &target, &name, entry.size)
        {
            failure = Some(e);
            return Ok(EntryDecision::Stop);
        }
        if ctx.cancel.load(Ordering::Relaxed) {
            failure = Some(Error::Cancelled);
            return Ok(EntryDecision::Stop);
        }
        Ok(EntryDecision::Continue)
    });
    if let Some(e) = failure {
        return Err(e);
    }
    res.map_err(|e| sz_err(path, e))
}

// ---- rar ----------------------------------------------------------------

#[cfg(feature = "rar")]
fn rar_name(h: &unrar::FileHeader) -> String {
    h.filename.to_string_lossy().replace('\\', "/")
}

#[cfg(feature = "rar")]
fn list_rar(path: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut a = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|e| sz_err(path, e))?;
    for h in &mut a {
        let h = h.map_err(|e| sz_err(path, e))?;
        if h.is_encrypted() {
            return Err(Error::Archive(
                "password-protected archives aren't supported".into(),
            ));
        }
        if h.is_split_before() {
            continue;
        }
        if let Some(p) = safe_path(&rar_name(&h)) {
            out.push(Entry {
                path: p,
                size: h.unpacked_size,
                is_dir: h.is_directory(),
            });
        }
    }
    Ok(out)
}

#[cfg(feature = "rar")]
fn extract_rar(path: &Path, dest: &dyn Fn(&str) -> Option<PathBuf>, ctx: &mut Ctx) -> Result<()> {
    let mut a = unrar::Archive::new(path)
        .open_for_processing()
        .map_err(|e| sz_err(path, e))?;
    while let Some(h) = a.read_header().map_err(|e| sz_err(path, e))? {
        ctx.check_cancel()?;
        let entry = h.entry();
        let name = safe_path(&rar_name(entry));
        let target = name.as_deref().and_then(dest);
        a = match (name, target) {
            (Some(name), Some(target)) if entry.is_file() => {
                let size = entry.unpacked_size;
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                }
                let next = h.extract_to(&target).map_err(|e| {
                    Error::Archive(format!("{name}: {e} (the archive may be damaged)"))
                })?;
                let written = fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
                if written != size {
                    return Err(Error::Archive(format!(
                        "{name}: unpacked {written} bytes but the archive says {size} (size mismatch; try downloading it again)"
                    )));
                }
                ctx.done += size;
                (ctx.progress)(Progress {
                    done: ctx.done,
                    total: ctx.total,
                    file: &name,
                });
                next
            }
            (Some(_), Some(target)) => {
                fs::create_dir_all(&target).map_err(|e| Error::io(&target, e))?;
                h.skip().map_err(|e| sz_err(path, e))?
            }
            _ => h.skip().map_err(|e| sz_err(path, e))?,
        };
    }
    Ok(())
}

#[cfg(not(feature = "rar"))]
fn list_rar(_: &Path) -> Result<Vec<Entry>> {
    Err(Error::Archive("this build has no RAR support".into()))
}

#[cfg(not(feature = "rar"))]
fn extract_rar(_: &Path, _: &dyn Fn(&str) -> Option<PathBuf>, _: &mut Ctx) -> Result<()> {
    Err(Error::Archive("this build has no RAR support".into()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn safe_paths() {
        assert_eq!(safe_path("a\\b/c.txt").as_deref(), Some("a/b/c.txt"));
        assert_eq!(safe_path("./a//b/").as_deref(), Some("a/b"));
        assert_eq!(safe_path("../evil"), None);
        assert_eq!(safe_path("a/../../evil"), None);
        assert_eq!(safe_path("/etc/passwd"), None);
        assert_eq!(safe_path("C:\\Windows\\x"), None);
        assert_eq!(safe_path("ok:stream"), None);
        assert_eq!(safe_path(""), None);
        assert_eq!(safe_path("ミク/曲.txt").as_deref(), Some("ミク/曲.txt"));
    }

    /// A minimal stored (uncompressed) zip with raw name bytes and no UTF-8
    /// flag, like old Japanese Windows zip tools produce.
    pub fn raw_zip(files: &[(&[u8], &[u8])]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        (c >> 1) ^ 0xEDB8_8320
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let off = out.len() as u32;
            let crc = crc32(data);
            let n = name.len() as u16;
            let sz = data.len() as u32;
            let mut hdr = Vec::new();
            hdr.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            hdr.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // ver, flags, method, time, date
            hdr.extend_from_slice(&crc.to_le_bytes());
            hdr.extend_from_slice(&sz.to_le_bytes());
            hdr.extend_from_slice(&sz.to_le_bytes());
            hdr.extend_from_slice(&n.to_le_bytes());
            hdr.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&hdr);
            out.extend_from_slice(name);
            out.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&sz.to_le_bytes());
            central.extend_from_slice(&sz.to_le_bytes());
            central.extend_from_slice(&n.to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name);
        }
        let cd_off = out.len() as u32;
        let cd_len = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        let count = files.len() as u16;
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_len.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn shift_jis_zip_names() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("old.zip");
        // "ミク/config.toml" in Shift-JIS
        let (name, _, _) = encoding_rs::SHIFT_JIS.encode("ミク/config.toml");
        fs::write(&p, raw_zip(&[(&name, b"name = \"x\"\n")])).unwrap();
        let (kind, entries) = list(&p).unwrap();
        assert_eq!(kind, Kind::Zip);
        assert_eq!(entries[0].path, "ミク/config.toml");

        let out = tmp.path().join("out");
        let o2 = out.clone();
        extract(
            &p,
            kind,
            &|n| Some(o2.join(n)),
            0,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(out.join("ミク/config.toml")).unwrap(),
            "name = \"x\"\n"
        );
    }

    #[test]
    fn zip_slip_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("evil.zip");
        fs::write(&p, raw_zip(&[(b"../../evil.txt", b"x"), (b"ok.txt", b"y")])).unwrap();
        let (_, entries) = list(&p).unwrap();
        assert_eq!(entries.len(), 1);
        let out = tmp.path().join("out");
        let o2 = out.clone();
        extract(
            &p,
            Kind::Zip,
            &|n| Some(o2.join(n)),
            0,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!tmp.path().join("evil.txt").exists());
        assert!(out.join("ok.txt").exists());
    }

    #[test]
    fn detects_by_content_not_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("really_a_zip.rar");
        fs::write(&p, raw_zip(&[(b"a.txt", b"a")])).unwrap();
        assert_eq!(detect(&p).unwrap(), Kind::Zip);
        let q = tmp.path().join("text.zip");
        fs::write(&q, b"hello").unwrap();
        assert!(matches!(detect(&q), Err(Error::Archive(_))));
    }

    #[test]
    fn sevenz_roundtrip_and_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src/Mod");
        fs::create_dir_all(src.join("rom")).unwrap();
        fs::write(src.join("config.toml"), "enabled = true\n").unwrap();
        fs::write(src.join("rom/big.bin"), vec![7u8; 3 << 20]).unwrap();
        let p = tmp.path().join("m.7z");
        sevenz_rust2::compress_to_path(tmp.path().join("src"), &p).unwrap();

        let (kind, entries) = list(&p).unwrap();
        assert_eq!(kind, Kind::SevenZ);
        assert!(
            entries
                .iter()
                .any(|e| e.path == "Mod/rom/big.bin" && e.size == 3 << 20)
        );

        let out = tmp.path().join("out");
        let o2 = out.clone();
        let mut reports = 0;
        extract(
            &p,
            kind,
            &|n| Some(o2.join(n)),
            3 << 20,
            &mut |_| reports += 1,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(out.join("Mod/rom/big.bin")).unwrap().len(),
            3 << 20
        );
        assert!(reports >= 3, "progress reported while streaming");

        let r = extract(
            &p,
            kind,
            &|n| Some(o2.join(n)),
            0,
            &mut |_| {},
            &AtomicBool::new(true),
        );
        assert!(matches!(r, Err(Error::Cancelled)));
    }
}
