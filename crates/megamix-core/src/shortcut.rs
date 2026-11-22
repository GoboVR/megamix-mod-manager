//! Add the manager to Steam as a non-Steam game, so it shows up in the
//! library and in Game Mode (Steam Deck, SteamOS, PB-OS).
//!
//! Shortcuts live in `<steam>/userdata/<account>/config/shortcuts.vdf`
//! (binary VDF). Steam reads it at start and rewrites it on exit, so Steam
//! must be closed while it's changed; [`close_steam`] / [`start_steam`]
//! help with that. Library artwork goes to `config/grid/<appid>*.png`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::binvdf::{self, Map, Value};
use crate::error::{Error, Result};

pub const NAME: &str = "Mega Mix+ Mod Manager";

/// Artwork file names in an art folder → Steam grid suffix.
pub const ART: [(&str, &str); 4] = [
    ("capsule.png", "p.png"),  // portrait library tile, 600x900
    ("wide.png", ".png"),      // landscape tile, 920x430
    ("hero.png", "_hero.png"), // banner at the top of the game page
    ("logo.png", "_logo.png"), // logo drawn over the banner
];

/// A Steam account folder that has a `config` folder.
#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: String,
    pub dir: PathBuf,
}

pub fn accounts(steam_root: &Path) -> Vec<Account> {
    let Ok(rd) = fs::read_dir(steam_root.join("userdata")) else {
        return vec![];
    };
    let mut out: Vec<Account> = rd
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let ok = id.chars().all(|c| c.is_ascii_digit()) && id != "0";
            (ok && e.path().join("config").is_dir()).then(|| Account { id, dir: e.path() })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Steam's id for a non-Steam shortcut (CRC32 of exe + name, top bit set).
pub fn shortcut_appid(exe_field: &str, name: &str) -> u32 {
    crc32(format!("{exe_field}{name}").as_bytes()) | 0x8000_0000
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn quote(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

fn is_ours(entry: &Map) -> bool {
    let exe = entry.str("Exe").unwrap_or_default().to_lowercase();
    entry.str("AppName").as_deref() == Some(NAME) || exe.contains("megamix-gui")
}

fn shortcuts_path(acc: &Account) -> PathBuf {
    acc.dir.join("config").join("shortcuts.vdf")
}

fn load(acc: &Account) -> Result<Map> {
    let p = shortcuts_path(acc);
    match fs::read(&p) {
        Ok(b) if !b.is_empty() => {
            binvdf::parse(&b).map_err(|e| Error::Other(format!("{}: {e}", p.display())))
        }
        Ok(_) => Ok(Map::default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::default()),
        Err(e) => Err(Error::io(&p, e)),
    }
}

fn save(acc: &Account, top: &Map) -> Result<()> {
    let p = shortcuts_path(acc);
    if p.exists() {
        let bak = p.with_extension("vdf.megamix-backup");
        fs::copy(&p, &bak).map_err(|e| Error::io(&bak, e))?;
    }
    let tmp = p.with_extension("vdf.megamix-new");
    fs::write(&tmp, binvdf::write(top)).map_err(|e| Error::io(&tmp, e))?;
    fs::rename(&tmp, &p).map_err(|e| Error::io(&p, e))
}

fn list_mut(top: &mut Map) -> &mut Map {
    if top.map("shortcuts").is_none() {
        top.0.retain(|(k, _)| !k.eq_ignore_ascii_case(b"shortcuts"));
        top.push("shortcuts", Value::Map(Map::default()));
    }
    top.map_mut("shortcuts").unwrap()
}

/// Number the entries 0, 1, 2… again (Steam expects that).
fn renumber(list: &mut Map) {
    for (i, (k, _)) in list.0.iter_mut().enumerate() {
        *k = i.to_string().into_bytes();
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Added {
    pub account: String,
    pub appid: u32,
    /// Replaced an older shortcut to the manager.
    pub updated: bool,
    pub art: usize,
}

/// Add (or update) the shortcut for `exe` in every Steam account.
/// `icon`: a PNG for the library list; `art_dir`: folder with
/// capsule/wide/hero/logo.png. Steam must not be running.
pub fn add(
    steam_root: &Path,
    exe: &Path,
    icon: Option<&Path>,
    art_dir: Option<&Path>,
) -> Result<Vec<Added>> {
    refuse_if_steam_running()?;
    add_unchecked(steam_root, exe, icon, art_dir)
}

fn refuse_if_steam_running() -> Result<()> {
    if steam_running() {
        return Err(Error::Other(
            "Steam is running; close it first (it would undo the change when it exits)".into(),
        ));
    }
    Ok(())
}

fn add_unchecked(
    steam_root: &Path,
    exe: &Path,
    icon: Option<&Path>,
    art_dir: Option<&Path>,
) -> Result<Vec<Added>> {
    let accs = accounts(steam_root);
    if accs.is_empty() {
        return Err(Error::Other(format!(
            "no Steam accounts in {} (log in to Steam once first)",
            steam_root.join("userdata").display()
        )));
    }
    let exe_field = quote(exe);
    let start_dir = quote(exe.parent().unwrap_or(Path::new(".")));
    let appid = shortcut_appid(&exe_field, NAME);
    let mut out = Vec::new();
    for acc in &accs {
        let mut top = load(acc)?;
        let list = list_mut(&mut top);
        let old = list
            .0
            .iter()
            .position(|(_, v)| matches!(v, Value::Map(m) if is_ours(m)));
        let mut e = Map::default();
        e.set_int("appid", appid as i32);
        e.set_str("AppName", NAME);
        e.set_str("Exe", &exe_field);
        e.set_str("StartDir", &start_dir);
        e.set_str(
            "icon",
            &icon.map(|p| p.display().to_string()).unwrap_or_default(),
        );
        e.set_str("ShortcutPath", "");
        e.set_str("LaunchOptions", "");
        e.set_int("IsHidden", 0);
        e.set_int("AllowDesktopConfig", 1);
        e.set_int("AllowOverlay", 1);
        e.set_int("OpenVR", 0);
        e.set_int("Devkit", 0);
        e.set_str("DevkitGameID", "");
        e.set_int("DevkitOverrideAppID", 0);
        e.set_int("LastPlayTime", 0);
        e.set_str("FlatpakAppID", "");
        e.push("tags", Value::Map(Map::default()));
        let updated = match old {
            Some(i) => {
                // Keep the user's play time, tags and launch options.
                if let Value::Map(prev) = &list.0[i].1 {
                    for key in ["LastPlayTime", "tags", "LaunchOptions", "IsHidden"] {
                        if let (Some(v), Some(slot)) = (prev.get(key).cloned(), e.get_mut(key)) {
                            *slot = v;
                        }
                    }
                }
                list.0[i].1 = Value::Map(e);
                true
            }
            None => {
                list.0.push((Vec::new(), Value::Map(e)));
                false
            }
        };
        renumber(list);
        save(acc, &top)?;
        let art = match art_dir {
            Some(d) => copy_art(acc, appid, d)?,
            None => 0,
        };
        out.push(Added {
            account: acc.id.clone(),
            appid,
            updated,
            art,
        });
    }
    Ok(out)
}

fn grid_dir(acc: &Account) -> PathBuf {
    acc.dir.join("config").join("grid")
}

fn copy_art(acc: &Account, appid: u32, art_dir: &Path) -> Result<usize> {
    let grid = grid_dir(acc);
    let mut n = 0;
    for (file, suffix) in ART {
        let from = art_dir.join(file);
        if from.is_file() {
            fs::create_dir_all(&grid).map_err(|e| Error::io(&grid, e))?;
            let to = grid.join(format!("{appid}{suffix}"));
            fs::copy(&from, &to).map_err(|e| Error::io(&to, e))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Remove the manager's shortcut (and its artwork) from every account.
/// Returns the accounts it was removed from.
pub fn remove(steam_root: &Path) -> Result<Vec<String>> {
    refuse_if_steam_running()?;
    remove_unchecked(steam_root)
}

fn remove_unchecked(steam_root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for acc in accounts(steam_root) {
        let mut top = load(&acc)?;
        let Some(list) = top.map_mut("shortcuts") else {
            continue;
        };
        let mut ids = Vec::new();
        list.0.retain(|(_, v)| match v {
            Value::Map(m) if is_ours(m) => {
                if let Some(id) = m.int("appid") {
                    ids.push(id as u32);
                }
                false
            }
            _ => true,
        });
        if ids.is_empty() {
            continue;
        }
        renumber(list);
        save(&acc, &top)?;
        for id in ids {
            for (_, suffix) in ART {
                let _ = fs::remove_file(grid_dir(&acc).join(format!("{id}{suffix}")));
            }
        }
        out.push(acc.id.clone());
    }
    Ok(out)
}

/// Accounts that have the shortcut.
pub fn status(steam_root: &Path) -> Vec<String> {
    accounts(steam_root)
        .into_iter()
        .filter(|a| {
            load(a)
                .ok()
                .and_then(|t| t.map("shortcuts").cloned())
                .is_some_and(|l| {
                    l.0.iter()
                        .any(|(_, v)| matches!(v, Value::Map(m) if is_ours(m)))
                })
        })
        .map(|a| a.id)
        .collect()
}

pub fn steam_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let Ok(mut sys) = System::new() else {
        return false;
    };
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes().values().any(|p| {
        let n = p.name().to_string_lossy().to_ascii_lowercase();
        n == "steam" || n == "steam.exe"
    })
}

/// Ask Steam to exit and wait for it (up to `timeout`).
pub fn close_steam(steam_root: &Path, timeout: Duration) -> Result<()> {
    if !steam_running() {
        return Ok(());
    }
    let _ = steam_command(steam_root).arg("-shutdown").spawn();
    let start = Instant::now();
    while start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(500));
        if !steam_running() {
            // Give it a moment to finish writing its files.
            std::thread::sleep(Duration::from_secs(2));
            return Ok(());
        }
    }
    Err(Error::Other(
        "Steam didn't close; close it yourself and try again".into(),
    ))
}

/// Start Steam again in the background.
pub fn start_steam(steam_root: &Path) -> Result<()> {
    let mut c = steam_command(steam_root);
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own session, so it keeps running after this program exits.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    c.spawn()
        .map(|_| ())
        .map_err(|e| Error::Other(format!("couldn't start Steam: {e}")))
}

fn steam_command(steam_root: &Path) -> std::process::Command {
    #[cfg(windows)]
    {
        std::process::Command::new(steam_root.join("steam.exe"))
    }
    #[cfg(not(windows))]
    {
        if steam_root
            .to_string_lossy()
            .contains("com.valvesoftware.Steam")
        {
            let mut c = std::process::Command::new("flatpak");
            c.args(["run", "com.valvesoftware.Steam"]);
            c
        } else {
            std::process::Command::new("steam")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_appid() {
        // Value from Steam for this exe/name pair (as used by common tools).
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert!(shortcut_appid("\"/a/b\"", NAME) & 0x8000_0000 != 0);
    }

    #[test]
    fn add_update_remove_keeps_other_shortcuts() {
        let t = crate::mods::tests::disk_tempdir();
        let root = t.path();
        let acc = root.join("userdata/1234/config");
        fs::create_dir_all(&acc).unwrap();
        fs::create_dir_all(root.join("userdata/0/config")).unwrap();
        // An existing shortcut that must survive.
        let mut other = Map::default();
        other.set_int("appid", -5);
        other.set_str("AppName", "Other Game");
        other.set_str("Exe", "\"/games/other\"");
        let mut list = Map::default();
        list.push("0", Value::Map(other.clone()));
        let mut top = Map::default();
        top.push("shortcuts", Value::Map(list));
        fs::write(acc.join("shortcuts.vdf"), binvdf::write(&top)).unwrap();
        let art = root.join("art");
        fs::create_dir_all(&art).unwrap();
        fs::write(art.join("capsule.png"), b"png").unwrap();

        let exe = root.join("bin/megamix-gui");
        let a = add_unchecked(root, &exe, None, Some(&art)).unwrap();
        assert_eq!(a.len(), 1, "account 0 is skipped");
        assert!(!a[0].updated);
        assert_eq!(a[0].art, 1);
        let id = a[0].appid;
        assert!(acc.join(format!("grid/{id}p.png")).is_file());
        assert_eq!(status(root), ["1234"]);

        // Again (an update): still one entry, play time kept.
        let mut t2 = binvdf::parse(&fs::read(acc.join("shortcuts.vdf")).unwrap()).unwrap();
        *t2.map_mut("shortcuts")
            .unwrap()
            .map_mut("1")
            .unwrap()
            .get_mut("LastPlayTime")
            .unwrap() = Value::Int(99);
        fs::write(acc.join("shortcuts.vdf"), binvdf::write(&t2)).unwrap();
        let a = add_unchecked(root, &exe, None, None).unwrap();
        assert!(a[0].updated);
        let t3 = binvdf::parse(&fs::read(acc.join("shortcuts.vdf")).unwrap()).unwrap();
        let l = t3.map("shortcuts").unwrap();
        assert_eq!(l.0.len(), 2);
        assert_eq!(l.map("0").unwrap(), &other);
        assert_eq!(l.map("1").unwrap().int("LastPlayTime"), Some(99));
        assert!(acc.join("shortcuts.vdf.megamix-backup").is_file());

        assert_eq!(remove_unchecked(root).unwrap(), ["1234"]);
        let t4 = binvdf::parse(&fs::read(acc.join("shortcuts.vdf")).unwrap()).unwrap();
        assert_eq!(t4.map("shortcuts").unwrap().0.len(), 1);
        assert!(!acc.join(format!("grid/{id}p.png")).exists());
        assert!(status(root).is_empty());
    }
}
