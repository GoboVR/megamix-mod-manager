//! The game folder, its mods and DivaModLoader's load order.
//!
//! DivaModLoader rules this follows (v0.0.16):
//! - `priority` lists mod **folder names**; the first entry wins conflicts.
//! - If `priority` is non-empty, mods not listed are **not loaded**. So every
//!   change made here keeps all mods listed (see [`Game::sync_priority`]).
//! - A mod without `enabled` is enabled.
//! - Mods are the direct subfolders of the mods folder with a `config.toml`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::backup::{self, BackupInfo};
use crate::config::{LoaderConfig, ModConfig};
use crate::error::{Error, Result};

/// Folder (inside the game folder) where the manager keeps its own data:
/// backups, download staging, install records.
pub const DATA_DIR: &str = ".megamix";

/// A mod folder.
#[derive(Debug, Clone)]
pub struct Mod {
    /// Folder name; this is what `priority` refers to.
    pub folder: String,
    pub dir: PathBuf,
    /// `None` if `config.toml` is missing or broken (see `error`).
    pub config: Option<ModConfig>,
    pub error: Option<String>,
}

impl Mod {
    fn load(dir: PathBuf) -> Self {
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cfg_path = dir.join("config.toml");
        let (config, error) = if !cfg_path.is_file() {
            (None, Some("no config.toml".to_owned()))
        } else {
            match ModConfig::load(&cfg_path) {
                Ok(c) => match validate(&c) {
                    Ok(()) => (Some(c), None),
                    Err(e) => (Some(c), Some(e.to_string())),
                },
                Err(e) => (None, Some(e.to_string())),
            }
        };
        Mod {
            folder,
            dir,
            config,
            error,
        }
    }

    /// Enabled in its own config. Broken mods count as disabled.
    pub fn enabled(&self) -> bool {
        self.error.is_none()
            && self
                .config
                .as_ref()
                .is_some_and(|c| c.enabled().unwrap_or(false))
    }

    /// Display name: `name` from the config, else the folder name.
    pub fn name(&self) -> &str {
        self.config
            .as_ref()
            .and_then(|c| c.name().ok().flatten())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&self.folder)
    }
}

/// Check that the fields DivaModLoader reads have the right types.
fn validate(c: &ModConfig) -> Result<()> {
    c.enabled()?;
    c.include()?;
    c.dll()?;
    Ok(())
}

/// Summary of one mod, for listing (CLI `list`, GUI rows, `--json`).
#[derive(Debug, Clone, Serialize)]
pub struct ModInfo {
    pub folder: String,
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub enabled: bool,
    /// 1-based position in `priority` (1 = highest), if listed.
    pub priority: Option<usize>,
    /// Will DivaModLoader actually load it right now.
    pub loaded: bool,
    pub include: Option<Vec<String>>,
    pub dll: Vec<String>,
    pub error: Option<String>,
}

/// Where to put mods that are missing from `priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewModPlacement {
    /// Highest priority (wins conflicts). Default: a freshly installed mod
    /// is usually meant to take effect.
    #[default]
    Top,
    Bottom,
}

/// A move in the load order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up(usize),
    Down(usize),
    Top,
    Bottom,
    /// 1-based position.
    To(usize),
}

/// What [`Game::save`] did.
#[derive(Debug, Clone, Serialize)]
pub struct SaveReport {
    /// Files written, relative to the game folder.
    pub written: Vec<PathBuf>,
    /// Backup taken before writing (None if nothing changed).
    pub backup: Option<BackupInfo>,
}

/// A game folder with DivaModLoader.
#[derive(Debug, Clone)]
pub struct Game {
    pub dir: PathBuf,
    pub loader: LoaderConfig,
    pub mods: Vec<Mod>,
}

impl Game {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let cfg = dir.join("config.toml");
        if !cfg.is_file() {
            return Err(Error::LoaderConfigMissing(cfg));
        }
        let loader = LoaderConfig::load(&cfg)?;
        let mut game = Game {
            dir,
            loader,
            mods: Vec::new(),
        };
        game.mods = game.scan_mods()?;
        Ok(game)
    }

    /// Reload everything from disk (drops unsaved changes).
    pub fn reload(&mut self) -> Result<()> {
        *self = Game::open(self.dir.clone())?;
        Ok(())
    }

    pub fn mods_dir(&self) -> Result<PathBuf> {
        Ok(self.dir.join(self.loader.mods_dir()?))
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.join(DATA_DIR)
    }

    fn scan_mods(&self) -> Result<Vec<Mod>> {
        let mods_dir = self.mods_dir()?;
        let entries = match fs::read_dir(&mods_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&mods_dir, e)),
        };
        let mut mods: Vec<Mod> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| Mod::load(e.path()))
            .collect();
        mods.sort_by_key(|m| m.folder.to_lowercase());
        Ok(mods)
    }

    pub fn priority(&self) -> Result<Vec<String>> {
        self.loader.priority()
    }

    /// Would DivaModLoader load this mod with the current files.
    pub fn is_loaded(&self, m: &Mod, priority: &[String]) -> bool {
        m.enabled() && (priority.is_empty() || priority.iter().any(|p| p == &m.folder))
    }

    /// Mods in effective load order: those in `priority` (in order), then the
    /// rest alphabetically.
    pub fn ordered(&self) -> Result<Vec<&Mod>> {
        let priority = self.priority()?;
        let mut out: Vec<&Mod> = priority
            .iter()
            .filter_map(|p| self.mods.iter().find(|m| &m.folder == p))
            .collect();
        for m in &self.mods {
            if !priority.contains(&m.folder) {
                out.push(m);
            }
        }
        Ok(out)
    }

    pub fn infos(&self) -> Result<Vec<ModInfo>> {
        let priority = self.priority()?;
        Ok(self
            .ordered()?
            .into_iter()
            .map(|m| {
                let c = m.config.as_ref();
                let s = |f: fn(&ModConfig) -> Result<Option<&str>>| {
                    c.and_then(|c| f(c).ok().flatten()).map(str::to_owned)
                };
                ModInfo {
                    folder: m.folder.clone(),
                    name: m.name().to_owned(),
                    version: s(ModConfig::version),
                    author: s(ModConfig::author),
                    description: s(ModConfig::description),
                    enabled: m.enabled(),
                    priority: priority.iter().position(|p| p == &m.folder).map(|i| i + 1),
                    loaded: self.is_loaded(m, &priority),
                    include: c.and_then(|c| c.include().ok().flatten()),
                    dll: c.and_then(|c| c.dll().ok()).unwrap_or_default(),
                    error: m.error.clone(),
                }
            })
            .collect())
    }

    /// Find a mod by folder name, then case-insensitive folder name, then
    /// display name.
    pub fn find(&self, query: &str) -> Result<usize> {
        if let Some(i) = self.mods.iter().position(|m| m.folder == query) {
            return Ok(i);
        }
        let tries: [&dyn Fn(&Mod) -> bool; 2] = [
            &|m: &Mod| m.folder.eq_ignore_ascii_case(query),
            &|m: &Mod| m.name().eq_ignore_ascii_case(query),
        ];
        for t in tries {
            let hits: Vec<usize> = (0..self.mods.len()).filter(|&i| t(&self.mods[i])).collect();
            match hits.len() {
                0 => continue,
                1 => return Ok(hits[0]),
                _ => {
                    return Err(Error::AmbiguousMod {
                        query: query.to_owned(),
                        matches: hits.iter().map(|&i| self.mods[i].folder.clone()).collect(),
                    });
                }
            }
        }
        Err(Error::ModNotFound(query.to_owned()))
    }

    // ---- changes (in memory until `save`) -----------------------------

    pub fn set_enabled(&mut self, query: &str, enabled: bool) -> Result<()> {
        let i = self.find(query)?;
        let m = &mut self.mods[i];
        let Some(cfg) = m.config.as_mut() else {
            return Err(Error::Other(format!(
                "{}: {}",
                m.folder,
                m.error.as_deref().unwrap_or("config.toml can't be read")
            )));
        };
        cfg.set_enabled(enabled);
        if enabled {
            self.sync_priority(NewModPlacement::Top)?;
        }
        Ok(())
    }

    /// Make sure every mod folder is in `priority` (so DivaModLoader loads
    /// it). Stale entries for missing folders are kept; `prune_priority`
    /// removes them. Returns the folders that were added.
    pub fn sync_priority(&mut self, placement: NewModPlacement) -> Result<Vec<String>> {
        let mut order = self.priority()?;
        let missing: Vec<String> = self
            .mods
            .iter()
            .filter(|m| !order.contains(&m.folder))
            .map(|m| m.folder.clone())
            .collect();
        if missing.is_empty() {
            return Ok(missing);
        }
        match placement {
            NewModPlacement::Top => {
                order.splice(0..0, missing.iter().cloned());
            }
            NewModPlacement::Bottom => order.extend(missing.iter().cloned()),
        }
        self.loader.set_priority(&order);
        Ok(missing)
    }

    /// Remove `priority` entries whose folder doesn't exist. Returns them.
    pub fn prune_priority(&mut self) -> Result<Vec<String>> {
        let order = self.priority()?;
        let (keep, gone): (Vec<String>, Vec<String>) = order
            .into_iter()
            .partition(|p| self.mods.iter().any(|m| &m.folder == p));
        if !gone.is_empty() {
            self.loader.set_priority(&keep);
        }
        Ok(gone)
    }

    /// Move a mod in the load order. Returns its new 1-based position.
    pub fn move_mod(&mut self, query: &str, mv: Move) -> Result<usize> {
        let folder = self.mods[self.find(query)?].folder.clone();
        self.sync_priority(NewModPlacement::Top)?;
        let mut order = self.priority()?;
        let from = order
            .iter()
            .position(|p| p == &folder)
            .expect("synced priority contains every mod");
        let last = order.len() - 1;
        let to = match mv {
            Move::Up(n) => from.saturating_sub(n),
            Move::Down(n) => from.saturating_add(n).min(last),
            Move::Top => 0,
            Move::Bottom => last,
            Move::To(pos) => pos.saturating_sub(1).min(last),
        };
        let item = order.remove(from);
        order.insert(to, item);
        self.loader.set_priority(&order);
        Ok(to + 1)
    }

    /// Replace the whole order (e.g. after a drag and drop). Must contain
    /// every mod folder exactly once; stale entries may be kept.
    pub fn set_order(&mut self, order: &[String]) -> Result<()> {
        for m in &self.mods {
            let n = order.iter().filter(|o| *o == &m.folder).count();
            if n != 1 {
                return Err(Error::Other(format!(
                    "new order must list `{}` exactly once (found {n})",
                    m.folder
                )));
            }
        }
        self.loader.set_priority(order);
        Ok(())
    }

    /// Files with unsaved changes, relative to the game folder.
    pub fn modified_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if self.loader.file.is_modified() {
            out.push(self.rel(self.loader.file.path()));
        }
        for m in &self.mods {
            if let Some(c) = &m.config
                && c.file.is_modified()
            {
                out.push(self.rel(c.file.path()));
            }
        }
        out
    }

    fn rel(&self, p: &Path) -> PathBuf {
        p.strip_prefix(&self.dir).unwrap_or(p).to_path_buf()
    }

    /// Back up all configs, then write the changed ones.
    pub fn save(&mut self, reason: &str) -> Result<SaveReport> {
        let written = self.modified_files();
        if written.is_empty() {
            return Ok(SaveReport {
                written,
                backup: None,
            });
        }
        let info = backup::create(self, reason)?;
        self.loader.save()?;
        for m in &mut self.mods {
            if let Some(c) = &mut m.config {
                c.save()?;
            }
        }
        Ok(SaveReport {
            written,
            backup: Some(info),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::steam::GAME_EXE;

    /// A fake game folder: `mods` = (folder, config.toml text).
    /// A temp folder on a real disk (under `target/`), not `/tmp`, which is
    /// RAM-backed on many Linux systems and refused for installs.
    pub fn disk_tempdir() -> tempfile::TempDir {
        // MEGAMIX_TEST_TMP lets cross-compiled tests run on other machines.
        let base = std::env::var_os("MEGAMIX_TEST_TMP")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp")
            });
        fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    pub fn fake_game(priority: &[&str], mods: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = disk_tempdir();
        let d = tmp.path();
        fs::write(d.join(GAME_EXE), b"").unwrap();
        fs::write(d.join("dinput8.dll"), b"").unwrap();
        let list = priority
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            d.join("config.toml"),
            format!("enabled = true\nconsole = false\nmods = \"mods\"\npriority = [{list}]\n"),
        )
        .unwrap();
        for (folder, cfg) in mods {
            let md = d.join("mods").join(folder);
            fs::create_dir_all(&md).unwrap();
            fs::write(md.join("config.toml"), cfg).unwrap();
        }
        tmp
    }

    fn folders(g: &Game) -> Vec<String> {
        g.priority().unwrap()
    }

    #[test]
    fn lists_in_priority_order_and_load_state() {
        let t = fake_game(
            &["B", "Gone"],
            &[
                ("A", "enabled = true\n"),
                ("B", "name = \"Bee\"\n"),
                ("C", "enabled = false\n"),
                ("D", "enabled = \"yes\"\n"),
            ],
        );
        fs::create_dir_all(t.path().join("mods/.megamix-staging")).unwrap();
        let g = Game::open(t.path()).unwrap();
        let infos = g.infos().unwrap();
        let rows: Vec<_> = infos
            .iter()
            .map(|i| (i.folder.as_str(), i.enabled, i.priority, i.loaded))
            .collect();
        assert_eq!(
            rows,
            [
                ("B", true, Some(1), true),
                ("A", true, None, false), // not in non-empty priority
                ("C", false, None, false),
                ("D", false, None, false), // broken type
            ]
        );
        assert_eq!(infos[0].name, "Bee");
        assert!(infos[3].error.is_some());
    }

    #[test]
    fn empty_priority_loads_all_enabled() {
        let t = fake_game(&[], &[("A", ""), ("B", "enabled = false\n")]);
        let g = Game::open(t.path()).unwrap();
        let loaded: Vec<_> = g.infos().unwrap().iter().map(|i| i.loaded).collect();
        assert_eq!(loaded, [true, false]);
    }

    #[test]
    fn enabling_adds_to_priority_and_save_backs_up() {
        let t = fake_game(&["B"], &[("A", "enabled = false\n"), ("B", "")]);
        let mut g = Game::open(t.path()).unwrap();
        g.set_enabled("a", true).unwrap(); // case-insensitive lookup
        assert_eq!(folders(&g), ["A", "B"]);
        let r = g.save("enable A").unwrap();
        assert_eq!(
            r.written,
            [
                PathBuf::from("config.toml"),
                PathBuf::from("mods/A/config.toml")
            ]
        );
        let b = r.backup.unwrap();
        let g2 = Game::open(t.path()).unwrap();
        assert!(g2.infos().unwrap().iter().all(|i| i.loaded));
        // The backup has the old files.
        let old = fs::read_to_string(b.dir.join("mods/A/config.toml")).unwrap();
        assert_eq!(old, "enabled = false\n");
    }

    #[test]
    fn save_without_changes_writes_nothing() {
        let t = fake_game(&["A"], &[("A", "")]);
        let mut g = Game::open(t.path()).unwrap();
        g.set_enabled("A", true).unwrap(); // already enabled (missing = true)
        let r = g.save("noop").unwrap();
        assert!(r.written.is_empty() && r.backup.is_none());
        assert!(!t.path().join(DATA_DIR).exists());
    }

    #[test]
    fn moves() {
        let t = fake_game(
            &["A", "B", "C", "D"],
            &[("A", ""), ("B", ""), ("C", ""), ("D", "")],
        );
        let mut g = Game::open(t.path()).unwrap();
        assert_eq!(g.move_mod("D", Move::Up(1)).unwrap(), 3);
        assert_eq!(folders(&g), ["A", "B", "D", "C"]);
        assert_eq!(g.move_mod("A", Move::Bottom).unwrap(), 4);
        assert_eq!(g.move_mod("C", Move::Top).unwrap(), 1);
        assert_eq!(folders(&g), ["C", "B", "D", "A"]);
        assert_eq!(g.move_mod("C", Move::Down(99)).unwrap(), 4);
        assert_eq!(g.move_mod("C", Move::To(2)).unwrap(), 2);
        assert_eq!(folders(&g), ["B", "C", "D", "A"]);
        assert_eq!(g.move_mod("B", Move::Up(5)).unwrap(), 1);
    }

    #[test]
    fn moving_an_unlisted_mod_lists_it_first() {
        let t = fake_game(&["A"], &[("A", ""), ("New", "")]);
        let mut g = Game::open(t.path()).unwrap();
        g.move_mod("A", Move::Top).unwrap();
        assert_eq!(folders(&g), ["A", "New"]);
    }

    #[test]
    fn sync_and_prune() {
        let t = fake_game(&["Gone", "A"], &[("A", ""), ("B", ""), ("C", "")]);
        let mut g = Game::open(t.path()).unwrap();
        assert_eq!(
            g.sync_priority(NewModPlacement::Bottom).unwrap(),
            ["B", "C"]
        );
        assert_eq!(folders(&g), ["Gone", "A", "B", "C"]);
        assert_eq!(g.prune_priority().unwrap(), ["Gone"]);
        assert_eq!(folders(&g), ["A", "B", "C"]);
    }

    #[test]
    fn set_order_requires_every_mod_once() {
        let t = fake_game(&["A", "B"], &[("A", ""), ("B", "")]);
        let mut g = Game::open(t.path()).unwrap();
        assert!(g.set_order(&["A".into()]).is_err());
        assert!(g.set_order(&["A".into(), "B".into(), "A".into()]).is_err());
        g.set_order(&["B".into(), "A".into()]).unwrap();
        assert_eq!(folders(&g), ["B", "A"]);
    }

    #[test]
    fn find_by_name_and_ambiguity() {
        let t = fake_game(
            &[],
            &[
                ("x1", "name = \"Same\"\n"),
                ("x2", "name = \"Same\"\n"),
                ("y", "name = \"Why\"\n"),
            ],
        );
        let g = Game::open(t.path()).unwrap();
        assert_eq!(g.mods[g.find("why").unwrap()].folder, "y");
        assert!(matches!(g.find("same"), Err(Error::AmbiguousMod { .. })));
        assert!(matches!(g.find("nope"), Err(Error::ModNotFound(_))));
    }

    #[test]
    fn missing_loader_config() {
        let t = tempfile::tempdir().unwrap();
        assert!(matches!(
            Game::open(t.path()),
            Err(Error::LoaderConfigMissing(_))
        ));
    }
}
