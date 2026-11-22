//! Removing mods, with undo.
//!
//! A removed mod's folder is moved (renamed, so it's instant even for
//! multi-GB song packs) to `<mods>/.megamix-removed/<backup id>/<folder>`,
//! on the same drive. The config backup taken first lists it, so "undo" (or
//! restoring that backup) moves it back. Removed mods are deleted for good
//! after [`KEEP_DAYS`] days, or with [`purge`].

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::backup::{self, BackupInfo};
use crate::error::{Error, Result};
use crate::mods::{Game, SaveReport};
use crate::records;

pub const REMOVED_DIR: &str = ".megamix-removed";
pub const KEEP_DAYS: u64 = 30;

/// A mod folder moved away by a removal (stored in the backup).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedMod {
    pub folder: String,
    /// Its install record (where it came from), to put back on undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<records::Record>,
}

fn stash_dir(mods_dir: &Path, backup_id: &str) -> PathBuf {
    mods_dir.join(REMOVED_DIR).join(backup_id)
}

/// Remove mods: back up, move their folders away, drop them from the load
/// order and forget where they came from.
pub fn remove(game: &mut Game, folders: &[String]) -> Result<SaveReport> {
    game.reload()?;
    let mods_dir = game.mods_dir()?;
    let mut list = Vec::new();
    let recs = records::load(&game.dir);
    for f in folders {
        let i = game.find(f)?;
        let folder = game.mods[i].folder.clone();
        if !list.iter().any(|r: &RemovedMod| r.folder == folder) {
            list.push(RemovedMod {
                record: recs.get(&folder).cloned(),
                folder,
            });
        }
    }
    let names: Vec<&str> = list.iter().map(|r| r.folder.as_str()).collect();
    let info =
        backup::create_with_removed(game, &format!("remove {}", names.join(", ")), list.clone())?;

    let stash = stash_dir(&mods_dir, &info.id);
    fs::create_dir_all(&stash).map_err(|e| Error::io(&stash, e))?;
    let mut moved = Vec::new();
    for r in &list {
        let from = mods_dir.join(&r.folder);
        let to = stash.join(&r.folder);
        if let Err(e) = fs::rename(&from, &to) {
            // Put back what was already moved, then report (e.g. a file
            // is open in the game on Windows).
            for m in moved.iter().rev() {
                let _ = fs::rename(stash.join(m), mods_dir.join(m));
            }
            let _ = fs::remove_dir(&stash);
            let _ = backup::delete(&game.dir, &info.id);
            return Err(Error::Other(format!(
                "couldn't remove {}: {e}{}",
                r.folder,
                if crate::doctor::game_running() {
                    " (close the game first)"
                } else {
                    ""
                }
            )));
        }
        moved.push(r.folder.clone());
    }

    // Load order and records.
    let order: Vec<String> = game
        .priority()?
        .into_iter()
        .filter(|p| !moved.contains(p))
        .collect();
    game.loader.set_priority(&order);
    let mut written = Vec::new();
    if game.loader.save()? {
        written.push(PathBuf::from("config.toml"));
    }
    for f in &moved {
        records::remove(&game.dir, f)?;
    }
    game.reload()?;
    Ok(SaveReport {
        written,
        backup: Some(info),
    })
}

/// Called by backup restore before configs are written: move the backup's
/// removed mods back into the mods folder. Returns the folders restored.
pub(crate) fn bring_back(game: &Game, info: &BackupInfo) -> Result<Vec<String>> {
    if info.removed.is_empty() {
        return Ok(Vec::new());
    }
    let mods_dir = game.mods_dir()?;
    let stash = stash_dir(&mods_dir, &info.id);
    let mut back = Vec::new();
    let mut recs = Vec::new();
    for r in &info.removed {
        let from = stash.join(&r.folder);
        let to = mods_dir.join(&r.folder);
        if !from.is_dir() {
            continue; // already restored, or purged
        }
        if to.exists() {
            return Err(Error::Other(format!(
                "can't bring back {}: a mod folder with that name exists again",
                r.folder
            )));
        }
        fs::rename(&from, &to).map_err(|e| Error::io(&from, e))?;
        if let Some(rec) = &r.record {
            recs.push((r.folder.clone(), rec.clone()));
        }
        back.push(r.folder.clone());
    }
    if !recs.is_empty() {
        records::set(&game.dir, recs)?;
    }
    let _ = fs::remove_dir(&stash);
    let _ = fs::remove_dir(mods_dir.join(REMOVED_DIR));
    Ok(back)
}

/// Removed mods still kept: (backup id, folders, size in bytes, age).
#[derive(Debug, Clone, Serialize)]
pub struct Kept {
    pub id: String,
    pub folders: Vec<String>,
    pub size: u64,
}

pub fn kept(mods_dir: &Path) -> Vec<Kept> {
    let Ok(rd) = fs::read_dir(mods_dir.join(REMOVED_DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<Kept> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let folders = fs::read_dir(e.path())
                .map(|r| {
                    r.flatten()
                        .map(|x| x.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            let size = walkdir::WalkDir::new(e.path())
                .into_iter()
                .flatten()
                .filter_map(|x| x.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum();
            Kept {
                id: e.file_name().to_string_lossy().into_owned(),
                folders,
                size,
            }
        })
        .collect();
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// Delete removed mods for good: all of them (`older_than` = None) or those
/// removed more than that long ago. Returns bytes freed.
pub fn purge(mods_dir: &Path, older_than: Option<Duration>) -> u64 {
    let root = mods_dir.join(REMOVED_DIR);
    let mut freed = 0;
    for k in kept(mods_dir) {
        let dir = root.join(&k.id);
        let old = match older_than {
            None => true,
            Some(limit) => fs::metadata(&dir)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|a| a > limit),
        };
        if old && fs::remove_dir_all(&dir).is_ok() {
            freed += k.size;
        }
    }
    let _ = fs::remove_dir(&root);
    freed
}

/// [`purge`] mods removed more than [`KEEP_DAYS`] ago.
pub fn purge_old(mods_dir: &Path) -> u64 {
    purge(mods_dir, Some(Duration::from_secs(KEEP_DAYS * 24 * 3600)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;

    #[test]
    fn remove_and_undo() {
        let t = fake_game(
            &["A", "B", "C"],
            &[
                ("A", "enabled = true\nx = 1\n"),
                ("B", "enabled = true\n"),
                ("C", ""),
            ],
        );
        fs::write(t.path().join("mods/B/big.bin"), vec![7u8; 1000]).unwrap();
        records::set(
            t.path(),
            [(
                "B".to_owned(),
                records::Record {
                    source: records::Source::GameBanana { id: 9 },
                    installed_at: "x".into(),
                    version: None,
                    file: None,
                },
            )],
        )
        .unwrap();
        let mut g = Game::open(t.path()).unwrap();
        let r = remove(&mut g, &["b".into()]).unwrap();
        assert!(r.backup.is_some());
        assert!(!t.path().join("mods/B").exists());
        assert_eq!(g.priority().unwrap(), ["A", "C"]);
        assert!(g.mods.iter().all(|m| m.folder != "B"));
        assert!(!records::load(t.path()).contains_key("B"));
        let k = kept(&t.path().join("mods"));
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].folders, ["B"]);
        assert!(k[0].size >= 1000);

        // Undo brings the folder, its place in the order and its record back.
        backup::undo(&mut g).unwrap().unwrap();
        assert_eq!(
            fs::read(t.path().join("mods/B/big.bin")).unwrap().len(),
            1000
        );
        assert_eq!(g.priority().unwrap(), ["A", "B", "C"]);
        assert!(records::load(t.path()).contains_key("B"));
        assert!(kept(&t.path().join("mods")).is_empty());
        assert!(!t.path().join("mods").join(REMOVED_DIR).exists());
    }

    #[test]
    fn purge_deletes_for_good() {
        let t = fake_game(&["A"], &[("A", "")]);
        let mut g = Game::open(t.path()).unwrap();
        remove(&mut g, &["A".into()]).unwrap();
        let mods = t.path().join("mods");
        assert_eq!(purge_old(&mods), 0, "too new to purge");
        assert_eq!(kept(&mods).len(), 1);
        purge(&mods, None);
        assert!(kept(&mods).is_empty());
        // Undo now can't bring it back, but doesn't fail.
        backup::undo(&mut g).unwrap();
        assert!(!mods.join("A").exists());
    }
}
