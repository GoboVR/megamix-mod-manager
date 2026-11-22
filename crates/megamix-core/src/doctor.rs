//! `doctor`: checks that the game, DivaModLoader and the mods are set up so
//! mods actually load, and explains how to fix what isn't.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::conflicts;
use crate::launch;
use crate::mods::Game;
use crate::steam::{self, GameInstall};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub level: Level,
    pub summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn new(id: &'static str, level: Level, summary: impl Into<String>) -> Self {
        Check {
            id,
            level,
            summary: summary.into(),
            details: Vec::new(),
            fix: None,
        }
    }
    fn details(mut self, d: Vec<String>) -> Self {
        self.details = d;
        self
    }
    fn fix(mut self, f: impl Into<String>) -> Self {
        self.fix = Some(f.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub game: Option<GameInstall>,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn worst(&self) -> Level {
        self.checks
            .iter()
            .map(|c| c.level)
            .max()
            .unwrap_or(Level::Ok)
    }
}

/// Mods known to hook rendering and crash under ARM64 Proton without the
/// x64 DXVK fix (folder or display name, case-insensitive).
pub const ARM_AFFECTED_MODS: &[&str] = &["LyricsPatch"];

/// Run all checks. `manual` overrides game detection.
pub fn run(manual: Option<&Path>) -> Report {
    match steam::find_game(manual) {
        Ok(install) => run_for(install, &steam::steam_roots()),
        Err(e) => Report {
            game: None,
            checks: vec![
                Check::new("game", Level::Error, e.to_string())
                    .fix("Pass the game folder with --game <path> (or set it in the GUI)."),
            ],
        },
    }
}

/// Run all checks for a known game folder. `steam_roots` are searched for
/// launch options.
pub fn run_for(install: GameInstall, steam_roots: &[PathBuf]) -> Report {
    let dir = install.game_dir.clone();
    let mut checks = vec![Check::new(
        "game",
        Level::Ok,
        format!("Game folder: {}", dir.display()),
    )];

    // DivaModLoader itself.
    if dir.join("dinput8.dll").is_file() {
        checks.push(Check::new(
            "loader",
            Level::Ok,
            "DivaModLoader (dinput8.dll) is installed",
        ));
    } else {
        checks.push(
            Check::new("loader", Level::Error, "DivaModLoader is not installed (no dinput8.dll)")
                .fix("Download DivaModLoader from https://github.com/blueskythlikesclouds/DivaModLoader/releases and extract it into the game folder."),
        );
    }

    let game =
        match Game::open(&dir) {
            Ok(g) => g,
            Err(e) => {
                checks.push(Check::new("loader_config", Level::Error, e.to_string()).fix(
                "Reinstall DivaModLoader, or restore a backup with `megamix backup restore`.",
            ));
                checks.extend(launch_checks(steam_roots));
                return Report {
                    game: Some(install),
                    checks,
                };
            }
        };
    checks.extend(loader_checks(&game));
    checks.extend(mod_checks(&game));
    checks.extend(launch_checks(steam_roots));
    checks.extend(dxvk_checks(&game));
    checks.push(running_check());
    Report {
        game: Some(install),
        checks,
    }
}

fn loader_checks(game: &Game) -> Vec<Check> {
    let mut out = Vec::new();
    match game.loader.enabled() {
        Ok(true) => out.push(Check::new(
            "loader_config",
            Level::Ok,
            "config.toml: mod loading is enabled",
        )),
        Ok(false) => out.push(
            Check::new(
                "loader_config",
                Level::Error,
                "config.toml: `enabled = false`, so no mods load",
            )
            .fix("Run `megamix loader enable`."),
        ),
        Err(e) => out.push(Check::new("loader_config", Level::Error, e.to_string())),
    }
    match game.mods_dir() {
        Ok(d) if d.is_dir() => {}
        Ok(d) => out.push(
            Check::new(
                "mods_dir",
                Level::Warn,
                format!("Mods folder {} does not exist", d.display()),
            )
            .fix("It will be created when you install a mod."),
        ),
        Err(e) => out.push(Check::new("mods_dir", Level::Error, e.to_string())),
    }
    out
}

fn mod_checks(game: &Game) -> Vec<Check> {
    let mut out = Vec::new();
    let Ok(priority) = game.priority() else {
        out.push(Check::new(
            "priority",
            Level::Error,
            "`priority` in config.toml is not a list of strings",
        ));
        return out;
    };

    let broken: Vec<String> = game
        .mods
        .iter()
        .filter_map(|m| m.error.as_ref().map(|e| format!("{}: {e}", m.folder)))
        .collect();
    if !broken.is_empty() {
        out.push(
            Check::new(
                "mod_configs",
                Level::Error,
                format!(
                    "{} mod(s) have a broken config.toml and won't load",
                    broken.len()
                ),
            )
            .details(broken),
        );
    }

    if !priority.is_empty() {
        let unlisted: Vec<String> = game
            .mods
            .iter()
            .filter(|m| m.enabled() && !priority.contains(&m.folder))
            .map(|m| m.folder.clone())
            .collect();
        if !unlisted.is_empty() {
            out.push(
                Check::new(
                    "priority",
                    Level::Error,
                    format!(
                        "{} enabled mod(s) are missing from `priority` and won't load",
                        unlisted.len()
                    ),
                )
                .details(unlisted)
                .fix("Run `megamix priority sync`."),
            );
        }
        let stale: Vec<String> = priority
            .iter()
            .filter(|p| !game.mods.iter().any(|m| &m.folder == *p))
            .cloned()
            .collect();
        if !stale.is_empty() {
            out.push(
                Check::new(
                    "priority_stale",
                    Level::Info,
                    "`priority` lists folders that don't exist (harmless)",
                )
                .details(stale)
                .fix("Run `megamix priority prune` to remove them."),
            );
        }
    }

    let missing_dlls: Vec<String> = game
        .mods
        .iter()
        .filter(|m| game.is_loaded(m, &priority))
        .flat_map(|m| {
            let dlls = m
                .config
                .as_ref()
                .and_then(|c| c.dll().ok())
                .unwrap_or_default();
            dlls.into_iter()
                .filter(|d| !m.dir.join(d).is_file())
                .map(|d| format!("{}: {d}", m.folder))
                .collect::<Vec<_>>()
        })
        .collect();
    if !missing_dlls.is_empty() {
        out.push(
            Check::new(
                "mod_dlls",
                Level::Warn,
                "Some mods list DLLs that aren't there",
            )
            .details(missing_dlls)
            .fix("Reinstall those mods."),
        );
    }

    let loaded = game
        .mods
        .iter()
        .filter(|m| game.is_loaded(m, &priority))
        .count();
    out.push(Check::new(
        "mods",
        Level::Ok,
        format!("{} mod(s) found, {loaded} will load", game.mods.len()),
    ));

    match conflicts::find(game) {
        Ok(c) if c.is_empty() => {}
        Ok(c) => {
            let details = conflicts::pairs(&c)
                .into_iter()
                .map(|p| format!("{} overrides {} ({} file(s))", p.winner, p.loser, p.files))
                .collect();
            out.push(
                Check::new(
                    "conflicts",
                    Level::Info,
                    format!("{} file conflict(s) between loaded mods", c.len()),
                )
                .details(details)
                .fix(
                    "Normal if intended. The mod higher in priority wins; see `megamix conflicts`.",
                ),
            );
        }
        Err(e) => out.push(Check::new("conflicts", Level::Warn, e.to_string())),
    }
    out
}

fn launch_checks(steam_roots: &[PathBuf]) -> Vec<Check> {
    if !launch::needs_dll_override() {
        return Vec::new();
    }
    let users: Vec<_> = steam_roots
        .iter()
        .flat_map(|r| launch::read_all(r))
        .collect();
    if users.is_empty() {
        return vec![
            Check::new("launch_options", Level::Warn, "Couldn't read the game's Steam launch options")
                .fix(format!(
                    "Make sure the game's launch options (Steam > Properties > Launch Options) are:\n  {}",
                    launch::suggest(None, false)
                )),
        ];
    }
    users
        .into_iter()
        .map(|u| {
            let opts = u.options.as_deref().unwrap_or("(empty)");
            if u.check.ok() {
                let mut c = Check::new(
                    "launch_options",
                    Level::Ok,
                    format!("Steam launch options load DivaModLoader (account {})", u.user_id),
                );
                if u.check.proton_log {
                    c.details.push(format!(
                        "PROTON_LOG is on: the log is written to ~/steam-{}.log",
                        steam::APP_ID
                    ));
                }
                c
            } else {
                let why = if u.check.dinput8_native && !u.check.has_command {
                    "the override is not followed by %command%, so Steam passes it to the game as arguments"
                } else {
                    "Proton will use its own dinput8.dll instead of DivaModLoader's"
                };
                Check::new(
                    "launch_options",
                    Level::Error,
                    format!("Steam launch options won't load DivaModLoader (account {}): {why}", u.user_id),
                )
                .details(vec![format!("Current: {opts}")])
                .fix(format!(
                    "In Steam: Mega Mix+ > Properties > Launch Options, set:\n  {}",
                    launch::suggest(u.options.as_deref(), u.check.proton_log)
                ))
            }
        })
        .collect()
}

pub fn is_arm64_linux() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "linux"))
}

fn dxvk_checks(game: &Game) -> Vec<Check> {
    let mut out = Vec::new();
    let files: Vec<String> = ["dxgi.dll", "d3d11.dll"]
        .iter()
        .filter_map(|f| {
            let p = game.dir.join(f);
            sha256_file(&p).ok().map(|h| format!("{f} (sha256 {h})"))
        })
        .collect();
    let has_dxvk = files.len() == 2;
    if !files.is_empty() {
        out.push(
            Check::new(
                "dxvk",
                Level::Info,
                "DirectX DLLs found in the game folder; the manager only changes them through the ARM fix",
            )
            .details(files),
        );
    }
    if is_arm64_linux() && !has_dxvk {
        let priority = game.priority().unwrap_or_default();
        let affected: Vec<String> = game
            .mods
            .iter()
            .filter(|m| game.is_loaded(m, &priority))
            .filter(|m| {
                ARM_AFFECTED_MODS
                    .iter()
                    .any(|a| m.folder.eq_ignore_ascii_case(a) || m.name().eq_ignore_ascii_case(a))
            })
            .map(|m| m.folder.clone())
            .collect();
        if !affected.is_empty() {
            out.push(
                Check::new(
                    "arm_dxvk",
                    Level::Warn,
                    "On ARM64 these mods crash the game unless x64 DXVK is in the game folder",
                )
                .details(affected)
                .fix("Run `megamix armfix apply`."),
            );
        }
    }
    out
}

fn running_check() -> Check {
    if game_running() {
        Check::new("running", Level::Warn, "The game is running")
            .fix("Changes take effect next time the game starts. Close it before installing or removing mods.")
    } else {
        Check::new("running", Level::Ok, "The game is not running")
    }
}

/// Whether `DivaMegaMix.exe` is running (natively or under Proton).
pub fn game_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let Ok(mut sys) = System::new() else {
        return false;
    };
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let exe = steam::GAME_EXE;
    sys.processes().values().any(|p| {
        p.name().to_string_lossy().eq_ignore_ascii_case(exe)
            || p.exe()
                .and_then(Path::file_name)
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(exe))
    })
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;
    use crate::steam::GameSource;

    fn install(dir: &Path) -> GameInstall {
        GameInstall {
            game_dir: dir.to_path_buf(),
            source: GameSource::Manual,
            library: None,
        }
    }

    fn level(r: &Report, id: &str) -> Option<Level> {
        r.checks.iter().find(|c| c.id == id).map(|c| c.level)
    }

    #[test]
    fn healthy_setup() {
        let t = fake_game(&["A"], &[("A", "")]);
        let r = run_for(install(t.path()), &[]);
        assert_eq!(level(&r, "loader"), Some(Level::Ok));
        assert_eq!(level(&r, "loader_config"), Some(Level::Ok));
        assert_eq!(level(&r, "priority"), None);
        assert_eq!(level(&r, "mod_configs"), None);
    }

    #[test]
    fn finds_problems() {
        let t = fake_game(
            &["A", "Gone"],
            &[
                ("A", "dll = [\"a.dll\"]\n"),
                ("B", ""),
                ("Bad", "enabled = [\n"),
            ],
        );
        fs::remove_file(t.path().join("dinput8.dll")).unwrap();
        let r = run_for(install(t.path()), &[]);
        assert_eq!(level(&r, "loader"), Some(Level::Error));
        assert_eq!(level(&r, "priority"), Some(Level::Error)); // B unlisted
        assert_eq!(level(&r, "priority_stale"), Some(Level::Info));
        assert_eq!(level(&r, "mod_configs"), Some(Level::Error));
        assert_eq!(level(&r, "mod_dlls"), Some(Level::Warn));
        assert_eq!(r.worst(), Level::Error);
    }

    #[test]
    fn missing_loader_config() {
        let t = fake_game(&[], &[]);
        fs::remove_file(t.path().join("config.toml")).unwrap();
        let r = run_for(install(t.path()), &[]);
        assert_eq!(level(&r, "loader_config"), Some(Level::Error));
    }

    #[test]
    fn dxvk_files_are_reported_with_hashes() {
        let t = fake_game(&[], &[]);
        fs::write(t.path().join("dxgi.dll"), b"abc").unwrap();
        fs::write(t.path().join("d3d11.dll"), b"abc").unwrap();
        let r = run_for(install(t.path()), &[]);
        let c = r.checks.iter().find(|c| c.id == "dxvk").unwrap();
        assert!(
            c.details[0]
                .contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }
}
