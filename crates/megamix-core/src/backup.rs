//! Config backups taken before every change, with restore and multi-step undo.
//!
//! Layout: `<game>/.megamix/backups/<id>/` holds copies of the game's
//! `config.toml` and every `mods/<Mod>/config.toml` (same relative paths),
//! plus `backup.json`. Only configs are backed up; mod files are never
//! touched by restore.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::error::{Error, Result};
use crate::mods::{DATA_DIR, Game};

/// How many backups to keep.
pub const KEEP: usize = 100;
const MANIFEST: &str = "backup.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupInfo {
    pub id: String,
    /// RFC 3339 local time.
    pub created: String,
    pub reason: String,
    /// Files in the backup, relative to the game folder, `/`-separated.
    pub files: Vec<String>,
    /// Set on the safety backup taken when restoring backup `<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<String>,
    /// Mods removed right after this backup; restoring it brings them back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<crate::remove::RemovedMod>,
    #[serde(skip)]
    pub dir: PathBuf,
}

pub fn backups_dir(game_dir: &Path) -> PathBuf {
    game_dir.join(DATA_DIR).join("backups")
}

/// Back up all configs as they are on disk now.
pub fn create(game: &Game, reason: &str) -> Result<BackupInfo> {
    create_inner(game, reason, None, Vec::new())
}

/// A backup taken right before removing mods (see [`crate::remove`]).
pub(crate) fn create_with_removed(
    game: &Game,
    reason: &str,
    removed: Vec<crate::remove::RemovedMod>,
) -> Result<BackupInfo> {
    create_inner(game, reason, None, removed)
}

/// Delete one backup (used when the change it was taken for failed).
pub(crate) fn delete(game_dir: &Path, id: &str) -> Result<()> {
    let dir = backups_dir(game_dir).join(id);
    fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))
}

fn create_inner(
    game: &Game,
    reason: &str,
    restored_from: Option<String>,
    removed: Vec<crate::remove::RemovedMod>,
) -> Result<BackupInfo> {
    let root = backups_dir(&game.dir);
    fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;

    let now = chrono::Local::now();
    let base = now.format("%Y%m%d-%H%M%S-%3f").to_string();
    let (id, dir) = (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .map(|id| {
            let d = root.join(&id);
            (id, d)
        })
        .find(|(_, d)| !d.exists())
        .expect("unbounded");
    fs::create_dir(&dir).map_err(|e| Error::io(&dir, e))?;

    let mut sources = vec![game.loader.file.path().to_path_buf()];
    sources.extend(
        game.mods
            .iter()
            .map(|m| m.dir.join("config.toml"))
            .filter(|p| p.is_file()),
    );

    let mut files = Vec::new();
    let result = (|| {
        for src in &sources {
            let rel = src.strip_prefix(&game.dir).unwrap_or(src);
            let dst = dir.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            fs::copy(src, &dst).map_err(|e| Error::io(src, e))?;
            files.push(rel_string(rel));
        }
        let info = BackupInfo {
            id: id.clone(),
            created: now.to_rfc3339(),
            reason: reason.to_owned(),
            files: files.clone(),
            restored_from: restored_from.clone(),
            removed: removed.clone(),
            dir: dir.clone(),
        };
        let json = serde_json::to_vec_pretty(&info).expect("serialisable");
        atomic_write(&dir.join(MANIFEST), &json)?;
        Ok(info)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    let info = result?;
    prune(&game.dir, KEEP)?;
    Ok(info)
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// All backups, newest first. Folders without a readable manifest are skipped.
pub fn list(game_dir: &Path) -> Result<Vec<BackupInfo>> {
    let root = backups_dir(game_dir);
    let entries = match fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&root, e)),
    };
    let mut out: Vec<BackupInfo> = entries
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let bytes = fs::read(dir.join(MANIFEST)).ok()?;
            let mut info: BackupInfo = serde_json::from_slice(&bytes).ok()?;
            info.dir = dir;
            Some(info)
        })
        .collect();
    // ids sort chronologically; break ties on the `-n` suffix numerically.
    out.sort_by_key(|a| std::cmp::Reverse(id_key(&a.id)));
    Ok(out)
}

fn id_key(id: &str) -> (String, u32) {
    // "YYYYMMDD-HHMMSS-mmm[-n]"
    let parts: Vec<&str> = id.splitn(4, '-').collect();
    let n = parts.get(3).and_then(|n| n.parse().ok()).unwrap_or(1);
    (parts[..parts.len().min(3)].join("-"), n)
}

fn prune(game_dir: &Path, keep: usize) -> Result<()> {
    for old in list(game_dir)?.into_iter().skip(keep) {
        fs::remove_dir_all(&old.dir).map_err(|e| Error::io(&old.dir, e))?;
    }
    Ok(())
}

/// What a restore did.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub restored: BackupInfo,
    /// Backup of the state just before restoring (so the restore can be undone).
    pub safety_backup: BackupInfo,
    pub written: Vec<String>,
    /// Files skipped because their mod folder no longer exists.
    pub skipped: Vec<String>,
}

/// Restore a backup's configs. The current state is backed up first.
/// Mods installed after the backup keep their configs; removed mods are
/// not recreated. Reloads `game` afterwards.
pub fn restore(game: &mut Game, id: &str) -> Result<RestoreReport> {
    let all = list(&game.dir)?;
    let target = all
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| Error::BackupNotFound(id.to_owned()))?;
    let safety = create_inner(
        game,
        &format!("before restoring {id}"),
        Some(id.to_owned()),
        Vec::new(),
    )?;
    // Mod folders removed after this backup come back first, so their
    // configs can be restored too.
    crate::remove::bring_back(game, &target)?;

    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for rel in &target.files {
        let src = target.dir.join(rel);
        let dst = game.dir.join(rel);
        if !dst.parent().is_some_and(Path::is_dir) {
            skipped.push(rel.clone());
            continue;
        }
        let bytes = fs::read(&src).map_err(|e| Error::io(&src, e))?;
        if fs::read(&dst).ok().as_deref() == Some(bytes.as_slice()) {
            continue;
        }
        atomic_write(&dst, &bytes)?;
        written.push(rel.clone());
    }
    game.reload()?;
    Ok(RestoreReport {
        restored: target,
        safety_backup: safety,
        written,
        skipped,
    })
}

/// The backup `undo` would restore: the newest one that isn't a safety
/// backup and hasn't already been undone. Repeated undos step further back.
pub fn undo_target(game_dir: &Path) -> Result<Option<BackupInfo>> {
    let mut undone = Vec::new();
    for b in list(game_dir)? {
        if let Some(from) = &b.restored_from {
            undone.push(from.clone());
            continue;
        }
        if !undone.contains(&b.id) {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

/// Undo the last change (see [`undo_target`]).
pub fn undo(game: &mut Game) -> Result<Option<RestoreReport>> {
    match undo_target(&game.dir)? {
        Some(b) => restore(game, &b.id).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;

    fn enabled(g: &Game, folder: &str) -> bool {
        g.mods
            .iter()
            .find(|m| m.folder == folder)
            .unwrap()
            .enabled()
    }

    #[test]
    fn multi_step_undo_and_redo() {
        let t = fake_game(
            &["A", "B"],
            &[("A", "enabled = true\n"), ("B", "enabled = true\n")],
        );
        let mut g = Game::open(t.path()).unwrap();

        g.set_enabled("A", false).unwrap();
        g.save("disable A").unwrap();
        g.set_enabled("B", false).unwrap();
        g.save("disable B").unwrap();
        assert!(!enabled(&g, "A") && !enabled(&g, "B"));

        let r = undo(&mut g).unwrap().unwrap();
        assert_eq!(r.restored.reason, "disable B");
        assert!(!enabled(&g, "A") && enabled(&g, "B"));

        let r = undo(&mut g).unwrap().unwrap();
        assert_eq!(r.restored.reason, "disable A");
        assert!(enabled(&g, "A") && enabled(&g, "B"));

        assert!(undo(&mut g).unwrap().is_none(), "nothing left to undo");

        // Redo by restoring the first safety backup (state after both changes).
        let first_safety = list(&g.dir)
            .unwrap()
            .into_iter()
            .rfind(|b| b.restored_from.is_some())
            .unwrap();
        restore(&mut g, &first_safety.id).unwrap();
        assert!(!enabled(&g, "A") && !enabled(&g, "B"));
    }

    #[test]
    fn restore_skips_removed_mods_and_keeps_new_ones() {
        let t = fake_game(&["A"], &[("A", "enabled = true\n")]);
        let mut g = Game::open(t.path()).unwrap();
        g.set_enabled("A", false).unwrap();
        let b = g.save("x").unwrap().backup.unwrap();

        fs::remove_dir_all(t.path().join("mods/A")).unwrap();
        let newm = t.path().join("mods/New");
        fs::create_dir_all(&newm).unwrap();
        fs::write(newm.join("config.toml"), "enabled = true\n").unwrap();
        g.reload().unwrap();

        let r = restore(&mut g, &b.id).unwrap();
        assert_eq!(r.skipped, ["mods/A/config.toml"]);
        assert!(!t.path().join("mods/A").exists());
        assert!(enabled(&g, "New"));
    }

    #[test]
    fn prune_keeps_newest() {
        let t = fake_game(&[], &[("A", "")]);
        let g = Game::open(t.path()).unwrap();
        for i in 0..5 {
            create(&g, &format!("b{i}")).unwrap();
        }
        prune(&g.dir, 2).unwrap();
        let left: Vec<_> = list(&g.dir)
            .unwrap()
            .into_iter()
            .map(|b| b.reason)
            .collect();
        assert_eq!(left, ["b4", "b3"]);
    }

    #[test]
    fn unknown_backup() {
        let t = fake_game(&[], &[]);
        let mut g = Game::open(t.path()).unwrap();
        assert!(matches!(
            restore(&mut g, "nope"),
            Err(Error::BackupNotFound(_))
        ));
    }
}
