//! Loadouts: named snapshots of which mods are enabled and their order.
//! Stored as `<game>/.megamix/loadouts/<name>.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::error::{Error, Result};
use crate::mods::{DATA_DIR, Game};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Loadout {
    pub name: String,
    pub created: String,
    /// Full load order (mod folders), highest priority first.
    pub order: Vec<String>,
    /// Mod folders that are enabled.
    pub enabled: Vec<String>,
}

/// What applying a loadout changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ApplyReport {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    /// In the loadout but no longer installed.
    pub missing: Vec<String>,
    /// Installed but not in the loadout (disabled, put last).
    pub not_in_loadout: Vec<String>,
}

pub fn loadouts_dir(game_dir: &Path) -> PathBuf {
    game_dir.join(DATA_DIR).join("loadouts")
}

fn check_name(name: &str) -> Result<()> {
    let bad = name.trim().is_empty()
        || name.len() > 100
        || name.starts_with('.')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        });
    if bad {
        return Err(Error::Other(format!(
            "invalid loadout name `{name}` (avoid / \\ : * ? \" < > | and a leading dot)"
        )));
    }
    Ok(())
}

fn path_for(game_dir: &Path, name: &str) -> PathBuf {
    loadouts_dir(game_dir).join(format!("{name}.json"))
}

/// Snapshot the current state (on disk + unsaved changes) as `name`.
/// Overwrites an existing loadout with that name.
pub fn save(game: &Game, name: &str) -> Result<Loadout> {
    check_name(name)?;
    let ordered = game.ordered()?;
    let l = Loadout {
        name: name.to_owned(),
        created: chrono::Local::now().to_rfc3339(),
        order: ordered.iter().map(|m| m.folder.clone()).collect(),
        enabled: ordered
            .iter()
            .filter(|m| m.enabled())
            .map(|m| m.folder.clone())
            .collect(),
    };
    let dir = loadouts_dir(&game.dir);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let json = serde_json::to_vec_pretty(&l).expect("serialisable");
    atomic_write(&path_for(&game.dir, name), &json)?;
    Ok(l)
}

pub fn load(game_dir: &Path, name: &str) -> Result<Loadout> {
    check_name(name)?;
    let p = path_for(game_dir, name);
    let bytes = fs::read(&p).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Other(format!("no loadout named `{name}`"))
        } else {
            Error::io(&p, e)
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Other(format!("{}: {e}", p.display())))
}

pub fn list(game_dir: &Path) -> Result<Vec<Loadout>> {
    let dir = loadouts_dir(game_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&dir, e)),
    };
    let mut out: Vec<Loadout> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by_key(|l| l.name.to_lowercase());
    Ok(out)
}

pub fn delete(game_dir: &Path, name: &str) -> Result<()> {
    check_name(name)?;
    let p = path_for(game_dir, name);
    fs::remove_file(&p).map_err(|e| Error::io(&p, e))
}

/// Apply a loadout to `game` in memory; call `game.save(..)` afterwards.
pub fn apply(game: &mut Game, l: &Loadout) -> Result<ApplyReport> {
    let mut r = ApplyReport::default();
    let installed: Vec<String> = game.mods.iter().map(|m| m.folder.clone()).collect();

    let mut order: Vec<String> = l
        .order
        .iter()
        .filter(|f| installed.contains(f))
        .cloned()
        .collect();
    r.missing = l
        .order
        .iter()
        .filter(|f| !installed.contains(f))
        .cloned()
        .collect();
    for f in &installed {
        if !order.contains(f) {
            order.push(f.clone());
            r.not_in_loadout.push(f.clone());
        }
    }

    for f in &installed {
        let want = l.enabled.contains(f);
        let m = game
            .mods
            .iter()
            .find(|m| &m.folder == f)
            .expect("installed");
        if m.config.is_none() || m.enabled() == want {
            continue;
        }
        game.set_enabled(f, want)?;
        if want {
            r.enabled.push(f.clone());
        } else {
            r.disabled.push(f.clone());
        }
    }
    game.set_order(&order)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;

    #[test]
    fn save_and_apply_roundtrip() {
        let t = fake_game(
            &["A", "B", "C"],
            &[("A", ""), ("B", ""), ("C", "enabled = false\n")],
        );
        let mut g = Game::open(t.path()).unwrap();
        save(&g, "Normal").unwrap();

        g.set_enabled("A", false).unwrap();
        g.set_enabled("C", true).unwrap();
        g.set_order(&["C".into(), "B".into(), "A".into()]).unwrap();
        g.save("change").unwrap();

        // Installed after the loadout was made.
        let d = t.path().join("mods/D");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("config.toml"), "").unwrap();
        g.reload().unwrap();

        let l = load(&g.dir, "Normal").unwrap();
        let r = apply(&mut g, &l).unwrap();
        g.save("apply loadout").unwrap();
        assert_eq!(r.enabled, ["A"]);
        assert_eq!(r.disabled, ["C", "D"]);
        assert_eq!(r.not_in_loadout, ["D"]);
        assert_eq!(g.priority().unwrap(), ["A", "B", "C", "D"]);
        let en: Vec<_> = g
            .mods
            .iter()
            .map(|m| (m.folder.as_str(), m.enabled()))
            .collect();
        assert_eq!(en, [("A", true), ("B", true), ("C", false), ("D", false)]);
    }

    #[test]
    fn names_are_checked() {
        let t = fake_game(&[], &[]);
        let g = Game::open(t.path()).unwrap();
        for bad in ["", "../x", "a/b", ".hidden", "c:x"] {
            assert!(save(&g, bad).is_err(), "{bad}");
        }
        save(&g, "Songs ミク").unwrap();
        assert_eq!(list(&g.dir).unwrap()[0].name, "Songs ミク");
        delete(&g.dir, "Songs ミク").unwrap();
        assert!(list(&g.dir).unwrap().is_empty());
    }
}
