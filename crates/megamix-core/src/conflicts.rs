//! Conflicts: two loaded mods providing the same game file.
//!
//! Each `include` entry is a folder mirroring the game's file layout (e.g.
//! `"."` with `rom/...` inside). Only paths under a top-level `rom*` folder
//! count as game files; readmes, previews and the mod's own config don't.
//! Files whose name starts with `mod_` (`mod_pv_db.txt`, `mod_spr_db.bin`,
//! ...) are merged by DivaModLoader, so they never conflict. Paths compare
//! case-insensitively, as the game runs on Windows file semantics.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use walkdir::WalkDir;

use crate::error::Result;
use crate::mods::{Game, Mod};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    /// Game path, `/`-separated, as spelled by the winning mod.
    pub path: String,
    /// Mod folder whose file is used (highest priority).
    pub winner: String,
    /// Mod folders whose copy is ignored, highest priority first.
    pub overridden: Vec<String>,
}

/// Conflict count between a winning and a losing mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictPair {
    pub winner: String,
    pub loser: String,
    pub files: usize,
}

/// Conflicts among the mods DivaModLoader would load now, sorted by path.
pub fn find(game: &Game) -> Result<Vec<Conflict>> {
    let priority = game.priority()?;
    let loaded: Vec<&Mod> = game
        .ordered()?
        .into_iter()
        .filter(|m| game.is_loaded(m, &priority))
        .collect();

    // lowercased path -> (display path, providers in load order)
    let mut map: HashMap<String, (String, Vec<String>)> = HashMap::new();
    for m in loaded {
        for path in game_files(m) {
            let key = path.to_lowercase();
            let entry = map.entry(key).or_insert_with(|| (path.clone(), Vec::new()));
            if !entry.1.contains(&m.folder) {
                entry.1.push(m.folder.clone());
            }
        }
    }
    let mut out: Vec<Conflict> = map
        .into_values()
        .filter(|(_, who)| who.len() > 1)
        .map(|(path, mut who)| {
            let winner = who.remove(0);
            Conflict {
                path,
                winner,
                overridden: who,
            }
        })
        .collect();
    out.sort_by_key(|a| a.path.to_lowercase());
    Ok(out)
}

/// Group conflicts by (winner, loser) pair, most files first.
pub fn pairs(conflicts: &[Conflict]) -> Vec<ConflictPair> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for c in conflicts {
        for l in &c.overridden {
            *counts.entry((c.winner.clone(), l.clone())).or_default() += 1;
        }
    }
    let mut out: Vec<ConflictPair> = counts
        .into_iter()
        .map(|((winner, loser), files)| ConflictPair {
            winner,
            loser,
            files,
        })
        .collect();
    out.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.winner.cmp(&b.winner))
            .then_with(|| a.loser.cmp(&b.loser))
    });
    out
}

/// Game files a mod replaces (`/`-separated, relative to its include roots).
pub fn game_files(m: &Mod) -> Vec<String> {
    let Some(include) = m.config.as_ref().and_then(|c| c.include().ok().flatten()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for inc in include {
        let root = m.dir.join(&inc);
        // Never follow an include outside the mod folder.
        if !is_inside(&root, &m.dir) {
            continue;
        }
        for e in WalkDir::new(&root).follow_links(true).into_iter().flatten() {
            if !e.file_type().is_file() {
                continue;
            }
            let Ok(rel) = e.path().strip_prefix(&root) else {
                continue;
            };
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let is_game_file = parts.len() > 1
                && parts[0].to_lowercase().starts_with("rom")
                && !parts
                    .last()
                    .is_some_and(|f| f.to_lowercase().starts_with("mod_"));
            if is_game_file {
                out.push(parts.join("/"));
            }
        }
    }
    out
}

fn is_inside(p: &Path, dir: &Path) -> bool {
    match (crate::steam::canonical(p), crate::steam::canonical(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;
    use std::fs;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn finds_conflicts_by_priority() {
        let t = fake_game(
            &["High", "Low", "Off"],
            &[
                ("High", "include = [\".\"]\n"),
                ("Low", "include = [\".\", \"extra\"]\n"),
                ("Off", "enabled = false\ninclude = [\".\"]\n"),
                ("NoInc", "dll = [\"x.dll\"]\n"),
            ],
        );
        let m = t.path().join("mods");
        touch(&m.join("High"), "rom/2d/spr_sel_pv001.farc");
        touch(&m.join("High"), "rom/mod_pv_db.txt");
        touch(&m.join("High"), "README.txt");
        touch(&m.join("Low"), "rom/2d/SPR_SEL_PV001.farc"); // different case
        touch(&m.join("Low"), "rom/mod_pv_db.txt"); // merged, not a conflict
        touch(&m.join("Low"), "extra/rom/sound/a.ogg");
        touch(&m.join("Low"), "README.txt");
        touch(&m.join("Off"), "rom/sound/a.ogg"); // disabled
        touch(&m.join("NoInc"), "rom/sound/a.ogg"); // no include: no files

        let g = Game::open(t.path()).unwrap();
        let c = find(&g).unwrap();
        assert_eq!(
            c,
            [Conflict {
                path: "rom/2d/spr_sel_pv001.farc".into(),
                winner: "High".into(),
                overridden: vec!["Low".into()],
            }]
        );
        assert_eq!(
            pairs(&c),
            [ConflictPair {
                winner: "High".into(),
                loser: "Low".into(),
                files: 1
            }]
        );
    }

    #[test]
    fn include_outside_mod_is_ignored() {
        let t = fake_game(&[], &[("Evil", "include = [\"../..\"]\n")]);
        let g = Game::open(t.path()).unwrap();
        assert!(game_files(&g.mods[0]).is_empty());
    }
}
