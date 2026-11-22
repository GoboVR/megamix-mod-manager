//! Finding the game install through Steam (all library folders, including
//! extra drives and SD cards), with a manual override.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::vdf::{self, Vdf};

pub const APP_ID: u32 = 1761390;
pub const DEFAULT_INSTALL_DIR: &str = "Hatsune Miku Project DIVA Mega Mix Plus";
pub const GAME_EXE: &str = "DivaMegaMix.exe";

/// How the game folder was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameSource {
    Manual,
    Steam,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameInstall {
    pub game_dir: PathBuf,
    pub source: GameSource,
    /// Steam library that contains the game (`None` for a manual path).
    pub library: Option<PathBuf>,
}

fn not_found_message(checked: &[PathBuf]) -> String {
    if checked.is_empty() {
        "Steam wasn't found on this computer, so the game couldn't be found either. \
         Set the game folder manually."
            .into()
    } else {
        format!(
            "could not find Project DIVA Mega Mix+ in any Steam library (checked: {}). \
             Set the game folder manually.",
            checked
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FindError {
    #[error("{0} does not look like the game folder ({GAME_EXE} not found)")]
    NotAGameDir(PathBuf),
    #[error("{}", not_found_message(.checked))]
    NotFound { checked: Vec<PathBuf> },
}

/// True if `dir` contains the game executable.
pub fn is_game_dir(dir: &Path) -> bool {
    dir.join(GAME_EXE).is_file()
}

/// Find the game. A manual path, if given, is used as-is (after checking it).
pub fn find_game(manual: Option<&Path>) -> Result<GameInstall, FindError> {
    if let Some(dir) = manual {
        return if is_game_dir(dir) {
            Ok(GameInstall {
                game_dir: dir.to_path_buf(),
                source: GameSource::Manual,
                library: None,
            })
        } else {
            Err(FindError::NotAGameDir(dir.to_path_buf()))
        };
    }
    find_in_roots(&steam_roots())
}

/// Search the given Steam roots (and every library they list) for the game.
pub fn find_in_roots(roots: &[PathBuf]) -> Result<GameInstall, FindError> {
    let mut checked = Vec::new();
    for root in roots {
        for lib in library_folders(root) {
            if checked.contains(&lib) {
                continue;
            }
            if let Some(dir) = game_dir_in_library(&lib) {
                return Ok(GameInstall {
                    game_dir: dir,
                    source: GameSource::Steam,
                    library: Some(lib),
                });
            }
            checked.push(lib);
        }
    }
    Err(FindError::NotFound { checked })
}

/// Look for the game in one library folder: the app manifest's `installdir`
/// first, then the default folder name (for half-broken manifests).
pub fn game_dir_in_library(lib: &Path) -> Option<PathBuf> {
    let steamapps = steamapps_dir(lib);
    let manifest = steamapps.join(format!("appmanifest_{APP_ID}.acf"));
    let from_manifest = fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| vdf::parse(&t).ok())
        .and_then(|v| {
            v.path(&["AppState", "installdir"])
                .and_then(Vdf::as_str)
                .map(str::to_owned)
        });
    [from_manifest.as_deref(), Some(DEFAULT_INSTALL_DIR)]
        .into_iter()
        .flatten()
        .map(|d| steamapps.join("common").join(d))
        .find(|d| is_game_dir(d))
}

/// `steamapps` inside a library. Older Steam on Windows used `SteamApps`,
/// which matters on case-sensitive file systems.
fn steamapps_dir(lib: &Path) -> PathBuf {
    let lower = lib.join("steamapps");
    if !lower.exists() {
        let upper = lib.join("SteamApps");
        if upper.exists() {
            return upper;
        }
    }
    lower
}

/// All library folders of a Steam root: the root itself first, then every
/// path in `libraryfolders.vdf`.
pub fn library_folders(root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![root.to_path_buf()];
    let vdf_path = steamapps_dir(root).join("libraryfolders.vdf");
    if let Ok(text) = fs::read_to_string(&vdf_path) {
        for p in parse_library_folders(&text) {
            if !libs.iter().any(|l| same_path(l, &p)) {
                libs.push(p);
            }
        }
    }
    libs
}

/// Extract library paths from `libraryfolders.vdf`. Supports the current
/// format (`"0" { "path" "..." }`) and the old one (`"1" "D:\\Lib"`).
pub fn parse_library_folders(text: &str) -> Vec<PathBuf> {
    let Ok(doc) = vdf::parse(text) else {
        return Vec::new();
    };
    let Some(root) = doc
        .get("libraryfolders")
        .or_else(|| doc.get("LibraryFolders"))
    else {
        return Vec::new();
    };
    root.entries()
        .iter()
        .filter(|(k, _)| k.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|(_, v)| match v {
            Vdf::Str(s) => Some(s.as_str()),
            Vdf::Obj(_) => v.get("path").and_then(Vdf::as_str),
        })
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => {
            cfg!(windows)
                && a.to_string_lossy()
                    .eq_ignore_ascii_case(&b.to_string_lossy())
        }
    }
}

/// `fs::canonicalize`, but without Windows' `\\?\` prefix for ordinary
/// drive paths (it confuses users and some tools).
pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    let c = fs::canonicalize(p)?;
    Ok(strip_verbatim(c))
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Candidate Steam install roots on this system, existing ones only,
/// without duplicates (symlinks such as `~/.steam/steam` are resolved).
pub fn steam_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for c in candidate_roots() {
        if !c.is_dir() {
            continue;
        }
        let real = canonical(&c).unwrap_or(c);
        if !out.iter().any(|o| same_path(o, &real)) {
            out.push(real);
        }
    }
    out
}

#[cfg(not(windows))]
fn candidate_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    vec![
        data.join("Steam"),
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".steam/debian-installation"),
        // Flatpak Steam
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.steam/steam"),
        // Snap Steam
        home.join("snap/steam/common/.local/share/Steam"),
    ]
}

#[cfg(windows)]
fn candidate_roots() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let mut out = Vec::new();
    let read = |hive, key: &str, value: &str| -> Option<PathBuf> {
        RegKey::predef(hive)
            .open_subkey(key)
            .ok()?
            .get_value::<String, _>(value)
            .ok()
            .map(|s| PathBuf::from(s.replace('/', "\\")))
    };
    out.extend(read(
        HKEY_CURRENT_USER,
        r"Software\Valve\Steam",
        "SteamPath",
    ));
    out.extend(read(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\WOW6432Node\Valve\Steam",
        "InstallPath",
    ));
    out.extend(read(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Valve\Steam",
        "InstallPath",
    ));
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(pf) = std::env::var_os(var) {
            out.push(PathBuf::from(pf).join("Steam"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW_FORMAT: &str = r#""libraryfolders"
{
	"0"
	{
		"path"		"/home/deck/.local/share/Steam"
		"label"		""
		"apps"
		{
			"228980"		"25674515"
		}
	}
	"1"
	{
		"path"		"/run/media/mmcblk0p1"
		"label"		"SD"
		"apps"
		{
			"1761390"		"35211287592"
		}
	}
}
"#;

    const OLD_FORMAT: &str = r#""LibraryFolders"
{
	"TimeNextStatsReport"		"1234"
	"ContentStatsID"		"-123"
	"1"		"D:\\SteamLibrary"
	"2"		"E:\\Games\\Steam"
}
"#;

    #[test]
    fn strips_verbatim_drive_prefix_only() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Steam")),
            PathBuf::from(r"C:\Steam")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share")),
            PathBuf::from(r"\\?\UNC\srv\share")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from("/home/x")),
            PathBuf::from("/home/x")
        );
    }

    #[test]
    fn parses_new_library_format() {
        assert_eq!(
            parse_library_folders(NEW_FORMAT),
            [
                PathBuf::from("/home/deck/.local/share/Steam"),
                PathBuf::from("/run/media/mmcblk0p1")
            ]
        );
    }

    #[test]
    fn parses_old_library_format() {
        assert_eq!(
            parse_library_folders(OLD_FORMAT),
            [
                PathBuf::from("D:\\SteamLibrary"),
                PathBuf::from("E:\\Games\\Steam")
            ]
        );
    }

    #[test]
    fn garbage_gives_no_libraries() {
        assert!(parse_library_folders("not { vdf").is_empty());
    }

    fn make_game(lib: &Path, installdir: &str, manifest: bool) -> PathBuf {
        let steamapps = lib.join("steamapps");
        let game = steamapps.join("common").join(installdir);
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join(GAME_EXE), b"").unwrap();
        if manifest {
            fs::write(
                steamapps.join(format!("appmanifest_{APP_ID}.acf")),
                format!(
                    "\"AppState\"\n{{\n\t\"appid\"\t\t\"{APP_ID}\"\n\t\"installdir\"\t\t\"{installdir}\"\n}}\n"
                ),
            )
            .unwrap();
        }
        game
    }

    #[test]
    fn finds_game_on_second_library() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let sd = tmp.path().join("SD card");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}",
                root.display().to_string().replace('\\', "\\\\"),
                sd.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let game = make_game(&sd, "Custom Folder ミク", true);

        let found = find_in_roots(&[root]).unwrap();
        assert_eq!(found.game_dir, game);
        assert_eq!(found.library.as_deref(), Some(sd.as_path()));
        assert_eq!(found.source, GameSource::Steam);
    }

    #[test]
    fn falls_back_to_default_dir_without_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let game = make_game(tmp.path(), DEFAULT_INSTALL_DIR, false);
        assert_eq!(game_dir_in_library(tmp.path()), Some(game));
    }

    #[test]
    fn not_found_lists_checked_libraries() {
        let tmp = tempfile::tempdir().unwrap();
        let err = find_in_roots(&[tmp.path().to_path_buf()]).unwrap_err();
        assert!(matches!(err, FindError::NotFound { ref checked } if checked.len() == 1));
    }

    #[test]
    fn manual_override_is_validated() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            find_game(Some(tmp.path())),
            Err(FindError::NotAGameDir(_))
        ));
        fs::write(tmp.path().join(GAME_EXE), b"").unwrap();
        let g = find_game(Some(tmp.path())).unwrap();
        assert_eq!(g.source, GameSource::Manual);
    }
}
