//! Installing mods from archive files.
//!
//! 1. [`plan`] lists the archive and finds the mods in it: every folder with
//!    a `config.toml` (nested mods inside a mod are ignored). An archive
//!    with loose files at the top is one mod named after the archive. An
//!    archive that only wraps another archive is unpacked one level.
//! 2. [`install`] unpacks into `<mods>/.megamix-staging/<job>/` — on the
//!    same drive as the mods, never `/tmp` (RAM on SteamOS/PB-OS) — after
//!    checking free space, then moves each mod into place with a rename.
//!    Replacing a mod keeps its `enabled` state and its settings. The staging
//!    folder is removed on success, failure and cancel.
//! 3. [`register`] adds the new mods to the load order (with a config backup)
//!    and records where they came from.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use crate::archive::{self, Kind};
use crate::config::{MOD_META_KEYS, ModConfig};
use crate::error::{Error, Result};
use crate::mods::{Game, NewModPlacement, SaveReport};
use crate::records::{self, Record, Source};

pub const STAGING_DIR: &str = ".megamix-staging";

/// Free space kept in reserve beyond the unpacked size.
const MIN_MARGIN: u64 = 64 << 20;

/// A mod found in an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedMod {
    /// Folder inside the archive that holds `config.toml` ("" = top level).
    pub root: String,
    /// Folder name it will get in the mods folder.
    pub folder: String,
    pub files: usize,
    pub size: u64,
    /// A mod folder with this name already exists.
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub archive: PathBuf,
    pub kind: Kind,
    pub mods: Vec<PlannedMod>,
    /// Unpacked size of everything that will be extracted.
    pub total_size: u64,
    /// The archive only wraps another archive (path inside); its mods are
    /// found after unpacking it, so `mods` is empty.
    pub nested: Option<String>,
}

/// What to do when a mod folder already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IfExists {
    /// Replace it, keeping its on/off state and settings (an update).
    Replace,
    /// Install next to it as "Name (2)".
    KeepBoth,
    /// Leave it alone.
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    pub folder: String,
    pub replaced: bool,
    /// Settings carried over from the replaced version.
    pub kept_settings: Vec<String>,
}

/// Progress of [`install`].
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    Checking,
    Unpacking(archive::Progress<'a>),
    Finishing,
}

// ---- planning -------------------------------------------------------------

pub fn plan(mods_dir: &Path, archive_path: &Path) -> Result<Plan> {
    let (kind, entries) = archive::list(archive_path)?;
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|e| !archive::is_junk(&e.path))
        .collect();

    let mut roots: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter(|e| last(&e.path).eq_ignore_ascii_case("config.toml"))
        .map(|e| parent(&e.path).to_owned())
        .collect();
    roots.sort_by_key(|r| (depth(r), r.clone()));
    let mut kept: Vec<String> = Vec::new();
    for r in roots {
        if !kept.iter().any(|k| is_under(&r, k)) {
            kept.push(r);
        }
    }

    if kept.is_empty() {
        let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
        let inner: Vec<_> = files
            .iter()
            .filter(|e| archive::has_archive_extension(Path::new(&e.path)))
            .collect();
        if inner.len() == 1 {
            return Ok(Plan {
                archive: archive_path.to_path_buf(),
                kind,
                mods: Vec::new(),
                total_size: inner[0].size,
                nested: Some(inner[0].path.clone()),
            });
        }
        return Err(Error::Archive(format!(
            "no DivaModLoader mod found in {} (no config.toml inside)",
            file_name(archive_path)
        )));
    }

    let stem = archive_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Mod".into());
    let mut mods: Vec<PlannedMod> = Vec::new();
    for root in kept {
        let base = sanitize_folder(if root.is_empty() { &stem } else { last(&root) });
        let mut folder = base.clone();
        let mut n = 2;
        while mods.iter().any(|m| m.folder.eq_ignore_ascii_case(&folder)) {
            folder = format!("{base} ({n})");
            n += 1;
        }
        let inside: Vec<_> = entries
            .iter()
            .filter(|e| !e.is_dir && is_under(&e.path, &root))
            .collect();
        mods.push(PlannedMod {
            exists: mods_dir.join(&folder).exists(),
            files: inside.len(),
            size: inside.iter().map(|e| e.size).sum(),
            root,
            folder,
        });
    }
    let total_size = mods.iter().map(|m| m.size).sum();
    Ok(Plan {
        archive: archive_path.to_path_buf(),
        kind,
        mods,
        total_size,
        nested: None,
    })
}

fn last(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn parent(p: &str) -> &str {
    p.rfind('/').map(|i| &p[..i]).unwrap_or("")
}

fn depth(p: &str) -> usize {
    if p.is_empty() {
        0
    } else {
        p.matches('/').count() + 1
    }
}

/// `p` is `root` itself or inside it ("" contains everything).
fn is_under(p: &str, root: &str) -> bool {
    root.is_empty()
        || p == root
        || (p.starts_with(root) && p.as_bytes().get(root.len()) == Some(&b'/'))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A folder name that works on Windows and Linux.
pub fn sanitize_folder(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .trim();
    let upper = trimmed.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    match (trimmed.is_empty(), reserved) {
        (true, _) => "Mod".into(),
        (_, true) => format!("{trimmed}_"),
        _ => trimmed.to_owned(),
    }
}

// ---- disk checks -------------------------------------------------------

/// Is `path` on a RAM-backed file system (tmpfs/ramfs)? Linux only.
pub fn is_ram_backed(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        const TMPFS_MAGIC: i64 = 0x0102_1994;
        const RAMFS_MAGIC: i64 = 0x8584_58f6;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: valid C string and out-pointer.
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return false;
        }
        #[allow(clippy::unnecessary_cast)]
        let t = st.f_type as i64;
        t == TMPFS_MAGIC || t == RAMFS_MAGIC
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// Check a staging folder: not in RAM, and `needed` bytes (+ margin) free.
pub fn check_staging(dir: &Path, needed: u64) -> Result<()> {
    if is_ram_backed(dir) {
        return Err(Error::RamBacked(dir.to_path_buf()));
    }
    let available = fs4::available_space(dir).map_err(|e| Error::io(dir, e))?;
    let wanted = needed + (needed / 50).max(MIN_MARGIN);
    if available < wanted {
        return Err(Error::NotEnoughSpace {
            path: dir.to_path_buf(),
            needed_mb: wanted.div_ceil(1 << 20),
            available_mb: available >> 20,
        });
    }
    Ok(())
}

/// Remove staging folders left behind by an install that was killed (its
/// process no longer runs). Folders are named `<time>-<pid>-<n>`.
fn cleanup_stale_staging(staging_root: &Path) {
    let Ok(rd) = fs::read_dir(staging_root) else {
        return;
    };
    let me = std::process::id();
    let mut sys: Option<sysinfo::System> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid: Option<u32> = name.split('-').nth(2).and_then(|p| p.parse().ok());
        let alive = match pid {
            Some(p) if p == me => true,
            Some(p) => {
                let sys = sys.get_or_insert_with(|| {
                    let mut s = sysinfo::System::new()
                        .unwrap_or_else(|_| sysinfo::System::new_all().expect("sysinfo"));
                    s.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                    s
                });
                sys.process(sysinfo::Pid::from_u32(p)).is_some()
            }
            None => false,
        };
        if !alive {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// Removes the job's staging folder when dropped (success, error, cancel).
struct StagingGuard {
    dir: PathBuf,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        if let Some(parent) = self.dir.parent() {
            let _ = fs::remove_dir(parent); // only if empty
        }
    }
}

// ---- installing --------------------------------------------------------

/// Unpack the planned mods into the mods folder. Doesn't touch the load
/// order; call [`register`] afterwards.
pub fn install(
    mods_dir: &Path,
    plan: &Plan,
    if_exists: IfExists,
    progress: &mut dyn FnMut(Step),
    cancel: &AtomicBool,
) -> Result<Vec<Installed>> {
    progress(Step::Checking);
    fs::create_dir_all(mods_dir).map_err(|e| Error::io(mods_dir, e))?;
    let staging_root = mods_dir.join(STAGING_DIR);
    cleanup_stale_staging(&staging_root);
    fs::create_dir_all(&staging_root).map_err(|e| Error::io(&staging_root, e))?;
    static JOBS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let job = staging_root.join(format!(
        "{}-{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        std::process::id(),
        JOBS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&job).map_err(|e| Error::io(&job, e))?;
    let _guard = StagingGuard { dir: job.clone() };
    install_in(mods_dir, &job, plan, if_exists, progress, cancel, 0)
}

#[allow(clippy::too_many_arguments)]
fn install_in(
    mods_dir: &Path,
    job: &Path,
    plan: &Plan,
    if_exists: IfExists,
    progress: &mut dyn FnMut(Step),
    cancel: &AtomicBool,
    level: u8,
) -> Result<Vec<Installed>> {
    if let Some(inner) = &plan.nested {
        if level > 0 {
            return Err(Error::Archive(
                "archives nested more than one level deep aren't supported".into(),
            ));
        }
        check_staging(job, plan.total_size)?;
        let target = job
            .join("nested")
            .join(archive::safe_path(last(inner)).unwrap_or_else(|| "inner".into()));
        let want = inner.clone();
        let t2 = target.clone();
        archive::extract(
            &plan.archive,
            plan.kind,
            &|p| (p == want).then(|| t2.clone()),
            plan.total_size,
            &mut |p| progress(Step::Unpacking(p)),
            cancel,
        )?;
        let inner_plan = plan_from(mods_dir, &target)?;
        return install_in(
            mods_dir,
            job,
            &inner_plan,
            if_exists,
            progress,
            cancel,
            level + 1,
        );
    }

    // Which mods, under which final names.
    let mut chosen: Vec<(usize, String, bool)> = Vec::new(); // (plan index, folder, replace)
    for (i, m) in plan.mods.iter().enumerate() {
        let exists = mods_dir.join(&m.folder).exists();
        match (exists, if_exists) {
            (false, _) => chosen.push((i, m.folder.clone(), false)),
            (true, IfExists::Skip) => {}
            (true, IfExists::Replace) => chosen.push((i, m.folder.clone(), true)),
            (true, IfExists::KeepBoth) => {
                chosen.push((i, unique_folder(mods_dir, &m.folder), false))
            }
        }
    }
    if chosen.is_empty() {
        return Ok(Vec::new());
    }
    let total: u64 = chosen.iter().map(|(i, _, _)| plan.mods[*i].size).sum();
    check_staging(job, total)?;

    // Extract: entry under mod i's root -> job/<i>/<relative path>.
    let roots: Vec<(usize, String)> = chosen
        .iter()
        .map(|(i, _, _)| (*i, plan.mods[*i].root.clone()))
        .collect();
    let job_dir = job.to_path_buf();
    let dest = move |p: &str| -> Option<PathBuf> {
        if archive::is_junk(p) {
            return None;
        }
        roots.iter().find(|(_, r)| is_under(p, r)).map(|(i, r)| {
            let rel = if r.is_empty() {
                p
            } else {
                p[r.len()..].trim_start_matches('/')
            };
            let base = job_dir.join(i.to_string());
            if rel.is_empty() { base } else { base.join(rel) }
        })
    };
    archive::extract(
        &plan.archive,
        plan.kind,
        &dest,
        total,
        &mut |p| progress(Step::Unpacking(p)),
        cancel,
    )?;

    progress(Step::Finishing);
    let mut out = Vec::new();
    for (i, folder, replace) in chosen {
        let staged = job.join(i.to_string());
        normalize_config_name(&staged)?;
        let target = mods_dir.join(&folder);
        let mut kept = Vec::new();
        if replace && target.exists() {
            kept = carry_over_settings(&target, &staged)?;
            let old = job.join(format!("old-{i}"));
            rename(&target, &old)?;
            if let Err(e) = rename(&staged, &target) {
                let _ = fs::rename(&old, &target);
                return Err(e);
            }
            // The old copy is deleted with the staging folder.
        } else {
            rename(&staged, &target)?;
        }
        out.push(Installed {
            folder,
            replaced: replace,
            kept_settings: kept,
        });
    }
    Ok(out)
}

fn plan_from(mods_dir: &Path, archive_path: &Path) -> Result<Plan> {
    plan(mods_dir, archive_path)
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|e| {
        let hint = if cfg!(windows) {
            " (is the game or another program using the mod's files?)"
        } else {
            ""
        };
        Error::Other(format!(
            "couldn't move {} to {}: {e}{hint}",
            from.display(),
            to.display()
        ))
    })
}

fn unique_folder(mods_dir: &Path, base: &str) -> String {
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|f| !mods_dir.join(f).exists())
        .expect("unbounded")
}

/// DivaModLoader on Windows finds `Config.toml` too; make it `config.toml`
/// so it also works on case-sensitive Linux file systems.
fn normalize_config_name(dir: &Path) -> Result<()> {
    if dir.join("config.toml").is_file() {
        return Ok(());
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name();
            if n.to_string_lossy().eq_ignore_ascii_case("config.toml") {
                return rename(&e.path(), &dir.join("config.toml"));
            }
        }
    }
    Ok(())
}

/// Copy `enabled` and the mod's own settings from the installed version's
/// config into the new one (keys that exist in both). Returns their names.
fn carry_over_settings(old_dir: &Path, new_dir: &Path) -> Result<Vec<String>> {
    let (Ok(old), Ok(mut new)) = (
        ModConfig::load(old_dir.join("config.toml")),
        ModConfig::load(new_dir.join("config.toml")),
    ) else {
        return Ok(Vec::new());
    };
    let mut kept = Vec::new();
    if let Ok(enabled) = old.enabled()
        && new.enabled().ok() != Some(enabled)
    {
        new.file.set_bool("enabled", enabled);
        kept.push("enabled".to_owned());
    }
    let settings: Vec<(String, toml_edit::Value)> = old
        .settings()
        .filter_map(|(k, item)| item.as_value().map(|v| (k.to_owned(), v.clone())))
        .collect();
    for (k, v) in settings {
        if MOD_META_KEYS.contains(&k.as_str()) {
            continue;
        }
        let same_type = new
            .file
            .doc()
            .get(&k)
            .and_then(|i| i.as_value())
            .is_some_and(|nv| std::mem::discriminant(nv) == std::mem::discriminant(&v));
        if same_type {
            let mut v = v;
            v.decor_mut().clear();
            new.set_setting(&k, v);
            kept.push(k);
        }
    }
    new.save()?;
    Ok(kept)
}

/// Add installed mods to the load order, record their source and save
/// (with a config backup). New mods go to `placement`; replaced mods keep
/// their position. If `priority` is empty (DivaModLoader loads everything),
/// it stays empty.
pub fn register(
    game: &mut Game,
    installed: &[Installed],
    archive_path: &Path,
    placement: NewModPlacement,
) -> Result<SaveReport> {
    let size = fs::metadata(archive_path).map(|m| m.len()).unwrap_or(0);
    let source = Source::File {
        file_name: file_name(archive_path),
        size,
    };
    register_with(game, installed, source, None, None, placement)
}

/// [`register`] with an explicit source (downloads). `version` is used
/// when the mod's `config.toml` has none.
pub fn register_with(
    game: &mut Game,
    installed: &[Installed],
    source: Source,
    file: Option<records::RemoteFileInfo>,
    version: Option<String>,
    placement: NewModPlacement,
) -> Result<SaveReport> {
    game.reload()?;
    let mut order = game.priority()?;
    if !order.is_empty() {
        let new: Vec<String> = installed
            .iter()
            .map(|m| m.folder.clone())
            .filter(|f| !order.contains(f))
            .collect();
        match placement {
            NewModPlacement::Top => {
                order.splice(0..0, new);
            }
            NewModPlacement::Bottom => order.extend(new),
        }
        game.loader.set_priority(&order);
    }
    let now = chrono::Local::now().to_rfc3339();
    let recs: Vec<(String, Record)> = installed
        .iter()
        .map(|m| {
            let config_version = game
                .mods
                .iter()
                .find(|x| x.folder == m.folder)
                .and_then(|x| x.config.as_ref())
                .and_then(|c| c.version().ok().flatten())
                .map(str::to_owned);
            (
                m.folder.clone(),
                Record {
                    source: source.clone(),
                    installed_at: now.clone(),
                    version: config_version.or_else(|| version.clone()),
                    file: file.clone(),
                },
            )
        })
        .collect();
    records::set(&game.dir, recs)?;
    let names: Vec<&str> = installed.iter().map(|m| m.folder.as_str()).collect();
    game.save(&format!("install {}", names.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::raw_zip;
    use crate::mods::tests::fake_game;
    use std::io::Write;

    fn zip_with(path: &Path, files: &[(&str, &[u8])]) {
        let f = fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in files {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    fn run(game: &mut Game, archive: &Path, if_exists: IfExists) -> Result<Vec<Installed>> {
        let mods_dir = game.mods_dir().unwrap();
        let p = plan(&mods_dir, archive)?;
        let r = install(
            &mods_dir,
            &p,
            if_exists,
            &mut |_| {},
            &AtomicBool::new(false),
        )?;
        register(game, &r, archive, NewModPlacement::Top)?;
        Ok(r)
    }

    #[test]
    fn plans_single_folder_loose_files_and_bundles() {
        let t = tempfile::tempdir().unwrap();
        let mods = t.path().join("mods");
        fs::create_dir_all(mods.join("Existing")).unwrap();

        let a = t.path().join("one.zip");
        zip_with(
            &a,
            &[
                ("Cool Mod/config.toml", b"x=1"),
                ("Cool Mod/rom/a.bin", b"aaaa"),
                ("readme.txt", b"hi"),
            ],
        );
        let p = plan(&mods, &a).unwrap();
        assert_eq!(p.mods.len(), 1);
        assert_eq!(
            (
                p.mods[0].root.as_str(),
                p.mods[0].folder.as_str(),
                p.mods[0].files
            ),
            ("Cool Mod", "Cool Mod", 2)
        );

        let b = t.path().join("Existing.zip");
        zip_with(&b, &[("config.toml", b"x=1"), ("rom/b.bin", b"b")]);
        let p = plan(&mods, &b).unwrap();
        assert_eq!(
            (
                p.mods[0].root.as_str(),
                p.mods[0].folder.as_str(),
                p.mods[0].exists
            ),
            ("", "Existing", true)
        );

        let c = t.path().join("pack.zip");
        zip_with(
            &c,
            &[
                ("Pack/Song A/config.toml", b""),
                ("Pack/Song A/sub/config.toml", b""), // nested: part of Song A
                ("Pack/Song B/config.toml", b""),
                ("__MACOSX/Pack/Song C/config.toml", b""),
            ],
        );
        let p = plan(&mods, &c).unwrap();
        let folders: Vec<_> = p.mods.iter().map(|m| m.folder.as_str()).collect();
        assert_eq!(folders, ["Song A", "Song B"]);
        assert_eq!(p.mods[0].files, 2);

        let d = t.path().join("nothing.zip");
        zip_with(&d, &[("readme.txt", b"no mod")]);
        assert!(matches!(plan(&mods, &d), Err(Error::Archive(_))));
    }

    #[test]
    fn installs_and_registers() {
        let t = fake_game(&["A"], &[("A", "")]);
        let mut g = Game::open(t.path()).unwrap();
        let a = t.path().join("dl.zip");
        zip_with(
            &a,
            &[
                ("New/config.toml", b"name = \"New\"\nversion = \"1.0\"\n"),
                ("New/rom/x.bin", b"xx"),
            ],
        );
        let r = run(&mut g, &a, IfExists::Replace).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(g.priority().unwrap(), ["New", "A"]);
        assert!(t.path().join("mods/New/rom/x.bin").is_file());
        assert!(
            !t.path().join("mods").join(STAGING_DIR).exists(),
            "staging cleaned up"
        );
        let rec = records::load(t.path());
        assert_eq!(rec["New"].version.as_deref(), Some("1.0"));
        assert!(
            matches!(rec["New"].source, Source::File { ref file_name, .. } if file_name == "dl.zip")
        );
    }

    #[test]
    fn replace_keeps_settings_and_state() {
        let t = fake_game(
            &["M"],
            &[(
                "M",
                "enabled = false\nversion = \"1\"\nquality = 3\nold_only = true\n",
            )],
        );
        fs::write(t.path().join("mods/M/old.bin"), b"old").unwrap();
        let mut g = Game::open(t.path()).unwrap();
        let a = t.path().join("M.zip");
        zip_with(
            &a,
            &[(
                "M/config.toml",
                b"enabled = true\nversion = \"2\"\nquality = 1 # 1-5\nnew_only = 1\n",
            )],
        );
        let r = run(&mut g, &a, IfExists::Replace).unwrap();
        assert!(r[0].replaced);
        assert_eq!(r[0].kept_settings, ["enabled", "quality"]);
        assert_eq!(
            fs::read_to_string(t.path().join("mods/M/config.toml")).unwrap(),
            "enabled = false\nversion = \"2\"\nquality = 3 # 1-5\nnew_only = 1\n"
        );
        assert!(
            !t.path().join("mods/M/old.bin").exists(),
            "old files replaced"
        );
        assert_eq!(g.priority().unwrap(), ["M"], "position kept");
    }

    #[test]
    fn keep_both_and_skip() {
        let t = fake_game(&["M"], &[("M", "")]);
        let mut g = Game::open(t.path()).unwrap();
        let a = t.path().join("M.zip");
        zip_with(&a, &[("M/config.toml", b"version = \"2\"\n")]);
        assert!(run(&mut g, &a, IfExists::Skip).unwrap().is_empty());
        let r = run(&mut g, &a, IfExists::KeepBoth).unwrap();
        assert_eq!(r[0].folder, "M (2)");
        assert_eq!(g.priority().unwrap(), ["M (2)", "M"]);
    }

    #[test]
    fn nested_archive_and_config_case() {
        let t = fake_game(&[], &[]);
        let mut g = Game::open(t.path()).unwrap();
        let inner = t.path().join("inner.zip");
        zip_with(&inner, &[("Inner Mod/Config.toml", b"name = \"I\"\n")]);
        let outer = t.path().join("outer.zip");
        zip_with(
            &outer,
            &[
                ("inner.zip", &fs::read(&inner).unwrap()),
                ("readme.txt", b""),
            ],
        );
        let p = plan(&t.path().join("mods"), &outer).unwrap();
        assert_eq!(p.nested.as_deref(), Some("inner.zip"));
        run(&mut g, &outer, IfExists::Replace).unwrap();
        assert!(
            t.path().join("mods/Inner Mod/config.toml").is_file(),
            "Config.toml renamed"
        );
        assert!(
            g.priority().unwrap().is_empty(),
            "empty priority stays empty"
        );
    }

    #[test]
    fn shift_jis_mod_folder() {
        let t = fake_game(&[], &[]);
        let mut g = Game::open(t.path()).unwrap();
        let (name, _, _) = encoding_rs::SHIFT_JIS.encode("初音ミク曲/config.toml");
        let a = t.path().join("jp.zip");
        fs::write(&a, raw_zip(&[(&name, b"")])).unwrap();
        let r = run(&mut g, &a, IfExists::Replace).unwrap();
        assert_eq!(r[0].folder, "初音ミク曲");
        assert!(t.path().join("mods/初音ミク曲/config.toml").is_file());
    }

    #[test]
    fn cancel_and_failure_clean_up() {
        let t = fake_game(&[], &[]);
        let mods = t.path().join("mods");
        fs::create_dir_all(&mods).unwrap();
        let a = t.path().join("m.zip");
        zip_with(
            &a,
            &[
                ("M/config.toml", b""),
                ("M/rom/big.bin", &vec![1u8; 4 << 20]),
            ],
        );
        let p = plan(&mods, &a).unwrap();
        let r = install(
            &mods,
            &p,
            IfExists::Replace,
            &mut |_| {},
            &AtomicBool::new(true),
        );
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(!mods.join("M").exists());
        assert!(!mods.join(STAGING_DIR).exists());

        // Damaged archive: truncate it.
        let bytes = fs::read(&a).unwrap();
        fs::write(&a, &bytes[..bytes.len() / 2]).unwrap();
        assert!(plan(&mods, &a).is_err());
        assert!(!mods.join(STAGING_DIR).exists());
    }

    #[test]
    fn folder_names_are_sanitized() {
        assert_eq!(sanitize_folder("a:b*c?"), "a_b_c_");
        assert_eq!(sanitize_folder("  .hidden. "), "hidden");
        assert_eq!(sanitize_folder("CON"), "CON_");
        assert_eq!(sanitize_folder("..."), "Mod");
        assert_eq!(sanitize_folder("ミク"), "ミク");
    }

    #[test]
    fn tmpfs_staging_is_refused() {
        #[cfg(target_os = "linux")]
        {
            let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
            let shm_is_tmpfs = mounts.lines().any(|l| {
                let f: Vec<_> = l.split_whitespace().collect();
                f.len() > 2 && f[1] == "/dev/shm" && f[2] == "tmpfs"
            });
            if shm_is_tmpfs {
                assert!(is_ram_backed(Path::new("/dev/shm")));
                assert!(matches!(
                    check_staging(Path::new("/dev/shm"), 1),
                    Err(Error::RamBacked(_))
                ));
            }
        }
        // A real disk folder is fine (and has more than 1 byte free).
        let d = crate::mods::tests::disk_tempdir();
        assert!(!is_ram_backed(d.path()));
        check_staging(d.path(), 1).unwrap();
        assert!(matches!(
            check_staging(d.path(), u64::MAX / 4),
            Err(Error::NotEnoughSpace { .. })
        ));
    }
}
