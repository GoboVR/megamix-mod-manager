//! The ARM fix: the official x64 DXVK `dxgi.dll` + `d3d11.dll` in the game
//! folder.
//!
//! On ARM64 Linux (FEX + Proton, ARM64EC) the game's `dxgi.dll` is a hybrid
//! ARM64EC build, and mods that hook rendering (LyricsPatch, …) crash the
//! game with an illegal instruction. Putting the x64 DXVK DLLs next to the
//! game fixes it. On Windows on ARM the system DirectX DLLs are hybrid too,
//! so the same might help there, but it's untested: only offered as an
//! experiment. Not needed (and refused) on x86 machines.
//!
//! Files we replace are moved to `.megamix/armfix/backup/` and put back by
//! [`remove`]. DXVK files we didn't install are left alone unless the user
//! insists.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::doctor::sha256_file;
use crate::download::{self, Progress};
use crate::error::{Error, Result};
use crate::mods::Game;
use crate::net::Client;
use crate::remote::RemoteFile;

pub const DXVK_VERSION: &str = "3.1.1";
const DXVK_URL: &str =
    "https://github.com/doitsujin/dxvk/releases/download/v3.1.1/dxvk-3.1.1.tar.gz";
const DXVK_ARCHIVE_SIZE: u64 = 18_041_512;
const DXVK_ARCHIVE_SHA256: &str =
    "40565b4a724aadc4433fa4e010b4b23916d9b1f1baeee64e17186db94f54e608";

/// The two DLLs and their SHA-256 in DXVK 3.1.1's `x64` folder.
pub const FILES: [(&str, &str); 2] = [
    (
        "dxgi.dll",
        "e3c1178eb7f0dd59f91bcfcc257549e5b9cfdb5e8508719293de7f8704f53160",
    ),
    (
        "d3d11.dll",
        "0a203b6255c893430f0a8461c618962df04e62aa41538d5a875b21868317cf9f",
    ),
];

/// Does this computer need the fix?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// ARM64 Linux: known to fix crashes.
    Recommended,
    /// Windows on ARM: might help, untested.
    Experimental,
    /// x86: not needed.
    NotNeeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum State {
    /// Neither DLL is in the game folder.
    NotInstalled,
    /// DXVK 3.1.1's x64 DLLs are there. `by_us` = installed by `apply`.
    Installed { by_us: bool },
    /// Something else: other versions or other DLLs (left alone).
    Other { files: Vec<String> },
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub need: Need,
    pub state: State,
    /// Enabled mods known to crash without the fix.
    pub affected_mods: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Record {
    version: String,
    /// Files that were in the game folder before, moved to the backup folder.
    backed_up: Vec<String>,
    installed_at: i64,
}

pub fn need() -> Need {
    if cfg!(target_arch = "aarch64") {
        if cfg!(windows) {
            Need::Experimental
        } else {
            Need::Recommended
        }
    } else if cfg!(windows) && windows_on_arm() {
        // The x64 build running under emulation on an ARM PC.
        Need::Experimental
    } else {
        Need::NotNeeded
    }
}

#[cfg(windows)]
fn windows_on_arm() -> bool {
    std::env::var("PROCESSOR_ARCHITEW6432")
        .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
        .map(|a| a.eq_ignore_ascii_case("ARM64"))
        .unwrap_or(false)
        || std::env::var("PROCESSOR_IDENTIFIER")
            .map(|s| s.to_ascii_lowercase().starts_with("arm"))
            .unwrap_or(false)
}

#[cfg(not(windows))]
fn windows_on_arm() -> bool {
    false
}

fn state_dir(game_dir: &Path) -> PathBuf {
    game_dir.join(".megamix").join("armfix")
}

fn record_path(game_dir: &Path) -> PathBuf {
    state_dir(game_dir).join("installed.json")
}

fn backup_dir(game_dir: &Path) -> PathBuf {
    state_dir(game_dir).join("backup")
}

fn load_record(game_dir: &Path) -> Option<Record> {
    let b = fs::read(record_path(game_dir)).ok()?;
    serde_json::from_slice(&b).ok()
}

/// Find a file case-insensitively (Proton prefixes don't care about case).
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let n = e.file_name();
        (n.to_string_lossy().eq_ignore_ascii_case(name) && e.path().is_file()).then(|| e.path())
    })
}

/// Which of the two DLLs are there and whether they are DXVK 3.1.1 x64.
fn present(game_dir: &Path) -> Vec<(String, bool)> {
    FILES
        .iter()
        .filter_map(|(name, sha)| {
            let p = find_file(game_dir, name)?;
            let ok = sha256_file(&p).map(|h| h == *sha).unwrap_or(false);
            Some((name.to_string(), ok))
        })
        .collect()
}

pub fn status(game: &Game) -> Status {
    let files = present(&game.dir);
    let state = if files.is_empty() {
        State::NotInstalled
    } else if files.len() == FILES.len() && files.iter().all(|(_, ok)| *ok) {
        State::Installed {
            by_us: load_record(&game.dir).is_some(),
        }
    } else {
        State::Other {
            files: files
                .iter()
                .map(|(n, ok)| {
                    if *ok {
                        format!("{n} (DXVK {DXVK_VERSION})")
                    } else {
                        format!("{n} (another version)")
                    }
                })
                .collect(),
        }
    };
    let affected_mods = game
        .mods
        .iter()
        .filter(|m| m.enabled())
        .filter(|m| {
            crate::doctor::ARM_AFFECTED_MODS
                .iter()
                .any(|a| m.folder.eq_ignore_ascii_case(a) || m.name().eq_ignore_ascii_case(a))
        })
        .map(|m| m.name().to_owned())
        .collect();
    Status {
        need: need(),
        state,
        affected_mods,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Applied {
    /// Files that were there before, now in the backup folder.
    pub backed_up: Vec<String>,
    /// Already installed; nothing changed.
    pub already: bool,
}

/// Download DXVK and put its x64 `dxgi.dll` and `d3d11.dll` in the game
/// folder. `experimental` must be set on Windows on ARM; on x86 it's refused.
pub fn apply(
    client: &Client,
    game: &Game,
    experimental: bool,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<Applied> {
    match need() {
        Need::NotNeeded => {
            return Err(Error::Other(
                "this PC doesn't need the ARM fix (it's only for ARM64 devices); nothing was changed"
                    .into(),
            ));
        }
        Need::Experimental if !experimental => {
            return Err(Error::Other(
                "on Windows on ARM the fix is untested; use the experimental option to try it anyway".into(),
            ));
        }
        _ => {}
    }
    if let State::Installed { .. } = status(game).state {
        return Ok(Applied {
            backed_up: vec![],
            already: true,
        });
    }

    let file = RemoteFile {
        id: 0,
        name: format!("dxvk-{DXVK_VERSION}.tar.gz"),
        size: DXVK_ARCHIVE_SIZE,
        url: DXVK_URL.into(),
        md5: None,
        added: 0,
    };
    // Downloads go next to the mods (same drive as the game, never /tmp).
    let mods_dir = game.mods_dir()?;
    let dl = download::download(
        client,
        &mods_dir,
        &format!("dxvk-{DXVK_VERSION}"),
        &file,
        64 << 20,
        progress,
        cancel,
    )?;
    let sha = sha256_file(&dl.path).map_err(|e| Error::io(&dl.path, e))?;
    if sha != DXVK_ARCHIVE_SHA256 {
        return Err(Error::DownloadDamaged {
            file: file.name,
            detail: "its checksum doesn't match DXVK's release".into(),
        });
    }
    let dlls = extract_dlls(&dl.path)?;
    drop(dl);
    install_dlls(&game.dir, &dlls)
}

/// Read `x64/dxgi.dll` and `x64/d3d11.dll` from the release archive and
/// check them.
fn extract_dlls(archive: &Path) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let f = fs::File::open(archive).map_err(|e| Error::io(archive, e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();
    let bad = |e: std::io::Error| Error::Archive(format!("DXVK archive: {e}"));
    for entry in tar.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry
            .path()
            .map_err(bad)?
            .to_string_lossy()
            .replace('\\', "/");
        for (name, _) in FILES {
            if path.ends_with(&format!("/x64/{name}")) {
                let mut b = Vec::new();
                entry.read_to_end(&mut b).map_err(bad)?;
                out.push((name, b));
            }
        }
    }
    for (name, sha) in FILES {
        let Some((_, b)) = out.iter().find(|(n, _)| *n == name) else {
            return Err(Error::Archive(format!("DXVK archive has no x64/{name}")));
        };
        use sha2::{Digest, Sha256};
        let got: String = Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        if got != sha {
            return Err(Error::Archive(format!(
                "x64/{name} in the DXVK archive isn't the expected file"
            )));
        }
    }
    Ok(out)
}

fn install_dlls(game_dir: &Path, dlls: &[(&'static str, Vec<u8>)]) -> Result<Applied> {
    let backup = backup_dir(game_dir);
    let mut backed_up = Vec::new();
    // Write new files under temporary names first, so a failure leaves the
    // game folder as it was.
    let mut staged = Vec::new();
    for (name, bytes) in dlls {
        let tmp = game_dir.join(format!(".{name}.megamix-new"));
        fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
        staged.push((tmp, *name));
    }
    for (_, name) in &staged {
        if let Some(old) = find_file(game_dir, name) {
            fs::create_dir_all(&backup).map_err(|e| Error::io(&backup, e))?;
            let to = backup.join(old.file_name().unwrap_or_default());
            fs::rename(&old, &to).map_err(|e| Error::io(&old, e))?;
            backed_up.push(
                to.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    for (tmp, name) in &staged {
        let to = game_dir.join(name);
        fs::rename(tmp, &to).map_err(|e| Error::io(&to, e))?;
    }
    let rec = Record {
        version: DXVK_VERSION.into(),
        backed_up: backed_up.clone(),
        installed_at: chrono::Utc::now().timestamp(),
    };
    let rp = record_path(game_dir);
    fs::create_dir_all(state_dir(game_dir)).map_err(|e| Error::io(&rp, e))?;
    fs::write(&rp, serde_json::to_vec_pretty(&rec).unwrap_or_default())
        .map_err(|e| Error::io(&rp, e))?;
    Ok(Applied {
        backed_up,
        already: false,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Removed {
    pub removed: Vec<String>,
    /// Files put back from the backup.
    pub restored: Vec<String>,
}

/// Take the fix out again. Only DXVK 3.1.1's files are deleted. If they
/// weren't installed by [`apply`] (someone put them there by hand),
/// `force` is needed.
pub fn remove(game: &Game, force: bool) -> Result<Removed> {
    let rec = load_record(&game.dir);
    if rec.is_none() && !force {
        return match status(game).state {
            State::NotInstalled => Ok(Removed {
                removed: vec![],
                restored: vec![],
            }),
            _ => Err(Error::Other(
                "these DLLs weren't installed by the manager (someone put them there by hand), so they're left alone; use --force to remove them anyway".into(),
            )),
        };
    }
    let mut removed = Vec::new();
    for (name, sha) in FILES {
        if let Some(p) = find_file(&game.dir, name)
            && sha256_file(&p).map(|h| h == sha).unwrap_or(false)
        {
            fs::remove_file(&p).map_err(|e| Error::io(&p, e))?;
            removed.push(name.to_string());
        }
    }
    let mut restored = Vec::new();
    let backup = backup_dir(&game.dir);
    for name in rec.map(|r| r.backed_up).unwrap_or_default() {
        let from = backup.join(&name);
        let to = game.dir.join(&name);
        if from.is_file() && !to.exists() {
            fs::rename(&from, &to).map_err(|e| Error::io(&from, e))?;
            restored.push(name);
        }
    }
    let _ = fs::remove_file(record_path(&game.dir));
    let _ = fs::remove_dir(&backup);
    let _ = fs::remove_dir(state_dir(&game.dir));
    Ok(Removed { removed, restored })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;

    /// Fake "DLLs" whose hashes we control can't match the real ones, so
    /// these tests use install_dlls/remove with real-hash checks bypassed
    /// via files that don't match (left alone) and the record logic.
    #[test]
    fn install_backs_up_and_remove_restores() {
        let t = fake_game(
            &["LyricsPatch"],
            &[("LyricsPatch", "name = \"LyricsPatch\"\n")],
        );
        let dir = t.path();
        fs::write(dir.join("dxgi.dll"), b"old dxgi").unwrap();
        let g = Game::open(dir).unwrap();
        assert_eq!(
            status(&g).state,
            State::Other {
                files: vec!["dxgi.dll (another version)".into()]
            }
        );
        assert_eq!(status(&g).affected_mods, ["LyricsPatch"]);

        let a = install_dlls(
            dir,
            &[
                ("dxgi.dll", b"new".to_vec()),
                ("d3d11.dll", b"new".to_vec()),
            ],
        )
        .unwrap();
        assert_eq!(a.backed_up, ["dxgi.dll"]);
        assert_eq!(fs::read(dir.join("dxgi.dll")).unwrap(), b"new");
        assert_eq!(
            fs::read(backup_dir(dir).join("dxgi.dll")).unwrap(),
            b"old dxgi"
        );
        assert!(load_record(dir).is_some());

        // The fake DLLs don't have DXVK's hashes, so remove leaves them,
        // but the old file can't come back over them either.
        let r = remove(&g, false).unwrap();
        assert!(r.removed.is_empty());
        assert!(r.restored.is_empty());
        assert_eq!(fs::read(dir.join("dxgi.dll")).unwrap(), b"new");
    }

    #[test]
    fn hand_placed_files_need_force() {
        let t = fake_game(&[], &[]);
        fs::write(t.path().join("d3d11.dll"), b"x").unwrap();
        let g = Game::open(t.path()).unwrap();
        assert!(remove(&g, false).is_err());
        let r = remove(&g, true).unwrap();
        assert!(
            r.removed.is_empty(),
            "files that aren't DXVK are never deleted"
        );
        assert!(t.path().join("d3d11.dll").exists());
    }

    #[test]
    fn reads_the_real_archive_layout() {
        // A tiny tar.gz shaped like DXVK's release, with wrong contents.
        let t = crate::mods::tests::disk_tempdir();
        let p = t.path().join("dxvk.tar.gz");
        let f = fs::File::create(&p).unwrap();
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            f,
            flate2::Compression::fast(),
        ));
        for name in [
            "dxvk-3.1.1/x64/dxgi.dll",
            "dxvk-3.1.1/x64/d3d11.dll",
            "dxvk-3.1.1/x32/dxgi.dll",
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, &b"abc"[..]).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        let e = extract_dlls(&p).unwrap_err().to_string();
        assert!(e.contains("isn't the expected file"), "{e}");
    }

    /// Downloads the real DXVK release (18 MB): `cargo test -- --ignored armfix`.
    #[test]
    #[ignore]
    fn real_download_install_and_remove() {
        let t = fake_game(&[], &[]);
        let dir = t.path();
        fs::write(dir.join("dxgi.dll"), b"proton's").unwrap();
        let g = Game::open(dir).unwrap();
        let client = Client::with_cache_dir(None);
        let file = RemoteFile {
            id: 0,
            name: "dxvk.tar.gz".into(),
            size: DXVK_ARCHIVE_SIZE,
            url: DXVK_URL.into(),
            md5: None,
            added: 0,
        };
        let cancel = AtomicBool::new(false);
        let dl = download::download(
            &client,
            &g.mods_dir().unwrap(),
            "dxvk-test",
            &file,
            0,
            &mut |_| {},
            &cancel,
        )
        .unwrap();
        assert_eq!(sha256_file(&dl.path).unwrap(), DXVK_ARCHIVE_SHA256);
        let dlls = extract_dlls(&dl.path).unwrap();
        install_dlls(dir, &dlls).unwrap();
        assert_eq!(status(&g).state, State::Installed { by_us: true });
        let r = remove(&g, false).unwrap();
        assert_eq!(r.removed, ["dxgi.dll", "d3d11.dll"]);
        assert_eq!(r.restored, ["dxgi.dll"]);
        assert_eq!(fs::read(dir.join("dxgi.dll")).unwrap(), b"proton's");
        assert!(!dir.join("d3d11.dll").exists());
        assert!(!state_dir(dir).exists());
    }
}
