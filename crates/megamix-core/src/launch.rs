//! Steam launch options for the game.
//!
//! Under Proton (Linux), Wine uses its own built-in `dinput8.dll` unless told
//! otherwise, so DivaModLoader never loads. The game needs
//! `WINEDLLOVERRIDES='dinput8.dll=n,b' %command%` (native first, then
//! built-in). `PROTON_LOG=1` before it writes `~/steam-1761390.log`.
//! On Windows the game folder's `dinput8.dll` loads by itself.
//!
//! This module only reads Steam's `localconfig.vdf`. It never writes it:
//! Steam rewrites that file on exit, so changes made while it runs are lost.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::steam::APP_ID;
use crate::vdf::{self, Vdf};

pub const DLL_OVERRIDE: &str = "WINEDLLOVERRIDES='dinput8.dll=n,b'";

/// Whether the launch options matter on this platform.
pub fn needs_dll_override() -> bool {
    !cfg!(windows)
}

/// Launch options of one Steam account for the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserLaunchOptions {
    /// Steam account folder (`userdata/<id>`).
    pub user_id: String,
    pub config_path: PathBuf,
    /// `None` if the account has no launch options for the game.
    pub options: Option<String>,
    pub check: LaunchCheck,
}

/// What a launch-options string does for DivaModLoader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LaunchCheck {
    /// `dinput8` is overridden native-first (`n` or `n,b`).
    pub dinput8_native: bool,
    /// Contains `%command%`. Without it, Steam passes the text to the game as
    /// arguments, so environment variables are not set.
    pub has_command: bool,
    pub proton_log: bool,
}

impl LaunchCheck {
    /// DivaModLoader will be loaded with these options.
    pub fn ok(&self) -> bool {
        self.dinput8_native && self.has_command
    }
}

/// Analyse a launch-options string.
pub fn check(options: &str) -> LaunchCheck {
    let (env, _) = split_env(options);
    let mut c = LaunchCheck {
        dinput8_native: false,
        has_command: options.contains("%command%"),
        proton_log: false,
    };
    for (k, v) in env {
        match k.as_str() {
            "WINEDLLOVERRIDES" => c.dinput8_native = override_is_native(&v, "dinput8"),
            "PROTON_LOG" => c.proton_log = !v.is_empty() && v != "0",
            _ => {}
        }
    }
    c
}

/// Launch options with the DLL override added (or fixed), keeping
/// everything else the user had. `proton_log` adds `PROTON_LOG=1` if missing.
pub fn suggest(existing: Option<&str>, proton_log: bool) -> String {
    let existing = existing.unwrap_or("").trim();
    let (env, rest) = split_env(existing);
    let has_command = existing.contains("%command%");

    let mut parts: Vec<String> = Vec::new();
    let mut have_log = false;
    for (k, v) in &env {
        match k.as_str() {
            "WINEDLLOVERRIDES" => continue,
            "PROTON_LOG" => have_log = true,
            _ => {}
        }
        parts.push(format!("{k}={}", quote_if_needed(v)));
    }
    if proton_log && !have_log {
        parts.insert(0, "PROTON_LOG=1".into());
    }
    let other: Vec<(String, String)> = env
        .iter()
        .filter(|(k, _)| k == "WINEDLLOVERRIDES")
        .cloned()
        .collect();
    parts.push(merged_override(other.first().map(|(_, v)| v.as_str())));

    if has_command {
        parts.push(rest.to_owned());
    } else {
        // Old text had no %command%: it was game arguments; keep them after.
        parts.push("%command%".into());
        if !rest.is_empty() {
            parts.push(rest.to_owned());
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" ")
}

/// Read every Steam account's launch options for the game under `steam_root`.
pub fn read_all(steam_root: &Path) -> Vec<UserLaunchOptions> {
    let mut out = Vec::new();
    let Ok(users) = fs::read_dir(steam_root.join("userdata")) else {
        return out;
    };
    for user in users.flatten() {
        let config_path = user.path().join("config/localconfig.vdf");
        let Ok(text) = fs::read(&config_path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&text);
        let Some(app_seen) = app_entry_exists(&text) else {
            continue; // unparsable
        };
        if !app_seen {
            continue; // this account never had the game
        }
        let options = launch_options_from_localconfig(&text);
        let check = check(options.as_deref().unwrap_or(""));
        out.push(UserLaunchOptions {
            user_id: user.file_name().to_string_lossy().into_owned(),
            config_path,
            options,
            check,
        });
    }
    out.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    out
}

fn app_node(doc: &Vdf) -> Option<&Vdf> {
    let id = APP_ID.to_string();
    doc.path(&[
        "UserLocalConfigStore",
        "Software",
        "Valve",
        "Steam",
        "apps",
        &id,
    ])
}

/// `Some(true)` if the game has an entry, `None` if the file doesn't parse.
fn app_entry_exists(text: &str) -> Option<bool> {
    let doc = vdf::parse(text).ok()?;
    Some(app_node(&doc).is_some())
}

/// The game's `LaunchOptions` from a `localconfig.vdf`, if set and non-empty.
pub fn launch_options_from_localconfig(text: &str) -> Option<String> {
    let doc = vdf::parse(text).ok()?;
    app_node(&doc)?
        .get("LaunchOptions")
        .and_then(Vdf::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Start the game through Steam (`steam://rungameid/1761390`), so Steam's
/// launch options and Proton settings apply. Returns once the request has
/// been handed to Steam; the game starts in the background.
pub fn start_game() -> std::io::Result<()> {
    let url = format!("steam://rungameid/{APP_ID}");
    open_url(&url)
}

/// Open a web page (a mod's page) in the browser. In Steam Game Mode
/// there's usually no desktop browser, so Steam's own browser is used.
pub fn open_web_page(url: &str) -> std::io::Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.contains(['"', ' ', '&', '^', '|'])
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a web address",
        ));
    }
    #[cfg(windows)]
    {
        open_url(url)
    }
    #[cfg(not(windows))]
    {
        let steam_url = format!("steam://openurl/{url}");
        let gamescope = std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some()
            || std::env::var_os("SteamGamepadUI").is_some();
        if Path::new("/.flatpak-info").exists() {
            run_first(&[&["flatpak-spawn", "--host", "xdg-open", url]])
        } else if gamescope {
            run_first(&[&["steam", &steam_url], &["xdg-open", url]])
        } else {
            run_first(&[&["xdg-open", url], &["steam", &steam_url]])
        }
    }
}

#[cfg(windows)]
fn open_url(url: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(not(windows))]
fn open_url(url: &str) -> std::io::Result<()> {
    // Inside a Flatpak, ask the host to open it.
    let in_flatpak = Path::new("/.flatpak-info").exists();
    let attempts: Vec<Vec<&str>> = if in_flatpak {
        vec![
            vec!["flatpak-spawn", "--host", "xdg-open", url],
            vec!["flatpak-spawn", "--host", "steam", url],
        ]
    } else {
        // `steam <url>` hands the URL to the running Steam client; it works in
        // Game Mode, where xdg-open may have no handler.
        vec![vec!["steam", url], vec!["xdg-open", url]]
    };
    let a: Vec<&[&str]> = attempts.iter().map(|v| v.as_slice()).collect();
    run_first(&a)
}

/// Start the first command that exists (not waiting for it).
#[cfg(not(windows))]
fn run_first(attempts: &[&[&str]]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut last_err = None;
    for a in attempts {
        match Command::new(a[0])
            .args(&a[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Reap it when it exits.
                std::thread::spawn(move || child.wait());
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no program found to open the link")))
}

// ---- helpers -----------------------------------------------------------

/// Split leading `NAME=value` words (shell-style quoting) from the rest.
/// Returns the assignments and the remaining text (starting at the first
/// word that isn't an assignment, e.g. `%command%`).
fn split_env(s: &str) -> (Vec<(String, String)>, &str) {
    let mut env = Vec::new();
    let mut rest = s.trim_start();
    while let Some((word, after)) = next_word(rest) {
        let Some((k, v)) = word.split_once('=') else {
            break;
        };
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        env.push((k.to_owned(), v.to_owned()));
        rest = after.trim_start();
    }
    (env, rest)
}

/// One shell word with quotes removed, and the text after it.
fn next_word(s: &str) -> Option<(String, &str)> {
    if s.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, c) if c.is_whitespace() => return Some((out, &s[i..])),
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (_, c) => out.push(c),
        }
    }
    Some((out, ""))
}

fn quote_if_needed(v: &str) -> String {
    if v.is_empty()
        || v.chars()
            .any(|c| c.is_whitespace() || matches!(c, ';' | '\'' | '"' | '$' | '*'))
    {
        format!("'{}'", v.replace('\'', r"'\''"))
    } else {
        v.to_owned()
    }
}

/// Wine override syntax: `name[,name2]=modes;name3=modes`. Names may or may
/// not have `.dll`. Modes: `n`, `b`, `n,b`, `b,n`, empty (= disabled).
fn override_is_native(value: &str, dll: &str) -> bool {
    parse_overrides(value)
        .into_iter()
        .any(|(names, modes)| names.iter().any(|n| dll_eq(n, dll)) && modes.starts_with('n'))
}

fn parse_overrides(value: &str) -> Vec<(Vec<String>, String)> {
    value
        .split(';')
        .filter_map(|entry| {
            let (names, modes) = entry.split_once('=')?;
            let names = names
                .split(',')
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty())
                .collect();
            Some((names, modes.trim().to_ascii_lowercase().replace(' ', "")))
        })
        .collect()
}

fn dll_eq(name: &str, dll: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.strip_suffix(".dll").unwrap_or(&n) == dll
}

/// A `WINEDLLOVERRIDES=...` word with `dinput8.dll=n,b` merged into the
/// user's existing overrides.
fn merged_override(existing: Option<&str>) -> String {
    let mut entries: Vec<String> = Vec::new();
    for (names, modes) in parse_overrides(existing.unwrap_or("")) {
        let names: Vec<_> = names
            .into_iter()
            .filter(|n| !dll_eq(n, "dinput8"))
            .collect();
        if !names.is_empty() {
            entries.push(format!("{}={modes}", names.join(",")));
        }
    }
    if entries.is_empty() {
        return DLL_OVERRIDE.to_owned();
    }
    entries.insert(0, "dinput8.dll=n,b".into());
    format!("WINEDLLOVERRIDES='{}'", entries.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THOR: &str = "PROTON_LOG=1 WINEDLLOVERRIDES='dinput8.dll=n,b' %command%";

    #[test]
    fn thor_options_are_ok() {
        let c = check(THOR);
        assert!(c.ok());
        assert!(c.proton_log);
    }

    #[test]
    fn variants_of_the_override() {
        for s in [
            "WINEDLLOVERRIDES=\"dinput8=n,b\" %command%",
            "WINEDLLOVERRIDES=dinput8.dll=n %command%",
            "WINEDLLOVERRIDES='d3d11,dinput8=n,b;xinput1_3=b' %command% -fullscreen",
            "WINEDLLOVERRIDES='DINPUT8.DLL=N,B' %command%",
        ] {
            assert!(check(s).ok(), "{s}");
        }
    }

    #[test]
    fn broken_options() {
        assert!(!check("").ok());
        assert!(!check("%command%").ok());
        assert!(!check("WINEDLLOVERRIDES='dinput8.dll=b,n' %command%").ok());
        assert!(!check("WINEDLLOVERRIDES='dinput8.dll=' %command%").ok());
        // No %command%: Steam passes it as game arguments.
        let c = check("WINEDLLOVERRIDES='dinput8.dll=n,b'");
        assert!(c.dinput8_native && !c.has_command && !c.ok());
        // After %command% it's an argument, not an env var.
        assert!(!check("%command% WINEDLLOVERRIDES='dinput8.dll=n,b'").ok());
        assert!(!check("PROTON_LOG=0 %command%").proton_log);
    }

    #[test]
    fn suggestions() {
        assert_eq!(
            suggest(None, false),
            "WINEDLLOVERRIDES='dinput8.dll=n,b' %command%"
        );
        assert_eq!(suggest(None, true), THOR);
        assert_eq!(suggest(Some(THOR), true), THOR);
        assert_eq!(
            suggest(Some(THOR), false),
            THOR,
            "existing PROTON_LOG is kept"
        );
        assert_eq!(
            suggest(Some("gamemoderun %command% -nolauncher"), false),
            "WINEDLLOVERRIDES='dinput8.dll=n,b' gamemoderun %command% -nolauncher"
        );
        assert_eq!(
            suggest(Some("-nolauncher"), false),
            "WINEDLLOVERRIDES='dinput8.dll=n,b' %command% -nolauncher"
        );
        assert_eq!(
            suggest(
                Some("DXVK_HUD=fps WINEDLLOVERRIDES='xinput1_3=b;dinput8=b' %command%"),
                false
            ),
            "DXVK_HUD=fps WINEDLLOVERRIDES='dinput8.dll=n,b;xinput1_3=b' %command%"
        );
        for s in [None, Some("-x"), Some("A='b c' %command%")] {
            assert!(check(&suggest(s, false)).ok(), "{s:?}");
        }
    }

    #[test]
    fn reads_localconfig() {
        let text = r#""UserLocalConfigStore"
{
	"Software"
	{
		"Valve"
		{
			"Steam"
			{
				"apps"
				{
					"228980" { "LastPlayed" "1" }
					"1761390"
					{
						"LastPlayed"		"1795270391"
						"LaunchOptions"		"PROTON_LOG=1 WINEDLLOVERRIDES='dinput8.dll=n,b' %command%"
					}
				}
			}
		}
	}
}
"#;
        assert_eq!(launch_options_from_localconfig(text).as_deref(), Some(THOR));
        assert_eq!(app_entry_exists(text), Some(true));
        let none = text.replace("\t\t\t\t\t\t\"LaunchOptions\"\t\t\"PROTON_LOG=1 WINEDLLOVERRIDES='dinput8.dll=n,b' %command%\"\n", "");
        assert_eq!(launch_options_from_localconfig(&none), None);
    }

    #[test]
    fn read_all_scans_userdata() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("userdata/123/config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("localconfig.vdf"),
            "\"UserLocalConfigStore\" { \"Software\" { \"Valve\" { \"Steam\" { \"apps\" { \"1761390\" { \"LastPlayed\" \"1\" } } } } } }",
        )
        .unwrap();
        fs::create_dir_all(tmp.path().join("userdata/456/config")).unwrap();
        let all = read_all(tmp.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].user_id, "123");
        assert_eq!(all[0].options, None);
        assert!(!all[0].check.ok());
    }
}
