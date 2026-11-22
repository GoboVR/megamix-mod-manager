//! Updating the manager itself from its GitHub releases.
//!
//! - Linux: the release tarball for this CPU replaces `megamix-gui` and
//!   `megamix` where they are (if that folder is writable).
//! - Windows: the release's installer for this CPU is run silently (only
//!   when the app was installed with it); the app quits so it can be
//!   replaced, and the installer starts it again.
//! - Otherwise (portable zip, read-only folder): open the releases page.
//!
//! Every download is checked against the release's `SHA256SUMS.txt`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::doctor::sha256_file;
use crate::download::{self, Progress};
use crate::error::{Error, Result};
use crate::net::Client;
use crate::remote::RemoteFile;

pub const REPO: &str = "GoboVR/megamix-mod-manager";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const CHECK_TTL: Duration = Duration::from_secs(6 * 3600);

pub fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases/latest")
}

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppRelease {
    pub version: String,
    pub title: String,
    pub notes: String,
    pub page_url: String,
    /// The file for this computer, if the release has one.
    pub asset: Option<Asset>,
    pub sums_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// How this copy can be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Replace the program files in place (Linux).
    InPlace,
    /// Run the Windows installer.
    Installer,
    /// Only point to the download page.
    Manual,
}

/// "v1.2.3" / "1.2.3-beta" → (1, 2, 3). Missing parts are 0.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    let a = it.next()?.ok()?;
    let b = it.next().unwrap_or(Ok(0)).ok()?;
    let c = it.next().unwrap_or(Ok(0)).ok()?;
    Some((a, b, c))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The release file name for this computer.
pub fn asset_name_for_this_pc(version: &str) -> Option<String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        return None;
    };
    if cfg!(windows) {
        let a = if arch == "aarch64" { "arm64" } else { "x64" };
        Some(format!("MegaMixModManager-{version}-Setup-{a}.exe"))
    } else if cfg!(target_os = "linux") {
        Some(format!("megamix-linux-{arch}.tar.gz"))
    } else {
        None
    }
}

/// A newer release than this one, if there is one. `fresh` skips the cache.
pub fn check(client: &Client, fresh: bool) -> Result<Option<AppRelease>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let ttl = if fresh { Duration::ZERO } else { CHECK_TTL };
    let r: GhRelease = match client.get_json(&url, ttl) {
        Ok(f) => f.value,
        // No release published yet.
        Err(Error::Http { status: 404, .. } | Error::RemoteNotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(newer_from(r, CURRENT))
}

fn newer_from(r: GhRelease, current: &str) -> Option<AppRelease> {
    if r.draft || r.prerelease || !is_newer(&r.tag_name, current) {
        return None;
    }
    let version = r.tag_name.trim_start_matches(['v', 'V']).to_owned();
    let want = asset_name_for_this_pc(&version);
    let asset = want.and_then(|w| {
        r.assets.iter().find(|a| a.name == w).map(|a| Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    });
    let sums_url = r
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .map(|a| a.browser_download_url.clone());
    Some(AppRelease {
        title: r
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Version {version}")),
        version,
        notes: r.body.unwrap_or_default(),
        page_url: r.html_url,
        asset,
        sums_url,
    })
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// How this copy would be updated.
pub fn method() -> Method {
    let Some(dir) = exe_dir() else {
        return Method::Manual;
    };
    if cfg!(windows) {
        // Installed with the installer: Inno Setup leaves its uninstaller.
        let base = if dir.ends_with("bin") {
            dir.parent().unwrap_or(&dir).to_path_buf()
        } else {
            dir
        };
        if base.join("unins000.exe").is_file() {
            Method::Installer
        } else {
            Method::Manual
        }
    } else if cfg!(target_os = "linux") {
        if Path::new("/.flatpak-info").exists() || std::env::var_os("APPIMAGE").is_some() {
            return Method::Manual;
        }
        // Writable folder (e.g. ~/.local/bin) and not a development build.
        let probe = dir.join(format!(".megamix-write-test-{}", std::process::id()));
        let writable = fs::write(&probe, b"").is_ok();
        let _ = fs::remove_file(&probe);
        let dev = dir.ends_with("debug") || dir.ends_with("release");
        if writable && !dev {
            Method::InPlace
        } else {
            Method::Manual
        }
    } else {
        Method::Manual
    }
}

fn work_dir() -> Result<PathBuf> {
    // Not /tmp: it's RAM on SteamOS/PB-OS.
    let d = dirs::cache_dir()
        .ok_or_else(|| Error::Other("no cache folder on this system".into()))?
        .join("megamix")
        .join("self-update");
    fs::create_dir_all(&d).map_err(|e| Error::io(&d, e))?;
    Ok(d)
}

/// Download this PC's file of the release and check its SHA-256.
fn fetch_checked(
    client: &Client,
    rel: &AppRelease,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<(download::Download, PathBuf)> {
    let asset = rel
        .asset
        .as_ref()
        .ok_or_else(|| Error::Other("this release has no download for this computer".into()))?;
    let sums_url = rel.sums_url.as_ref().ok_or_else(|| {
        Error::Other("the release has no checksums, so it isn't installed automatically".into())
    })?;
    let sums = client.get_bytes(sums_url, Duration::ZERO)?.value;
    let sums = String::from_utf8_lossy(&sums);
    let want = sums
        .lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            let h = p.next()?;
            let n = p.next()?.trim_start_matches('*');
            (n == asset.name).then(|| h.to_ascii_lowercase())
        })
        .next()
        .ok_or_else(|| Error::Other(format!("{} isn't in the release's checksums", asset.name)))?;
    let file = RemoteFile {
        id: 0,
        name: asset.name.clone(),
        size: asset.size,
        url: asset.url.clone(),
        md5: None,
        added: 0,
    };
    let root = work_dir()?;
    let dl = download::download(
        client,
        &root,
        &format!("app-{}", rel.version),
        &file,
        asset.size * 3,
        progress,
        cancel,
    )?;
    let got = sha256_file(&dl.path).map_err(|e| Error::io(&dl.path, e))?;
    if got != want {
        return Err(Error::DownloadDamaged {
            file: asset.name.clone(),
            detail: "its checksum doesn't match the release".into(),
        });
    }
    let path = dl.path.clone();
    Ok((dl, path))
}

#[derive(Debug, Clone, Serialize)]
pub struct Updated {
    /// Linux: the files that were replaced (restart to use them).
    pub replaced: Vec<PathBuf>,
    /// Windows: the installer was started; quit now.
    pub installer_started: bool,
}

/// Download and install `rel`. On Windows this starts the installer: the
/// caller must quit right after so its files can be replaced.
pub fn install(
    client: &Client,
    rel: &AppRelease,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<Updated> {
    match method() {
        Method::Manual => Err(Error::Other(format!(
            "this copy can't update itself; download the new version from {}",
            rel.page_url
        ))),
        Method::InPlace => {
            let (_dl, path) = fetch_checked(client, rel, progress, cancel)?;
            let dir =
                exe_dir().ok_or_else(|| Error::Other("can't find this program's folder".into()))?;
            let replaced = replace_from_tarball(&path, &dir)?;
            Ok(Updated {
                replaced,
                installer_started: false,
            })
        }
        Method::Installer => {
            let (dl, path) = fetch_checked(client, rel, progress, cancel)?;
            // Keep the installer: it must outlive this process.
            let keep = work_dir()?.join(
                rel.asset
                    .as_ref()
                    .map(|a| a.name.as_str())
                    .unwrap_or("setup.exe"),
            );
            let _ = fs::remove_file(&keep);
            fs::rename(&path, &keep).map_err(|e| Error::io(&keep, e))?;
            drop(dl);
            std::process::Command::new(&keep)
                .args([
                    "/SILENT",
                    "/SP-",
                    "/NOCANCEL",
                    "/CLOSEAPPLICATIONS",
                    "/RELAUNCH=1",
                ])
                .spawn()
                .map_err(|e| Error::Other(format!("couldn't start the installer: {e}")))?;
            Ok(Updated {
                replaced: vec![],
                installer_started: true,
            })
        }
    }
}

/// Put `megamix-gui` and `megamix` from the tarball into `dir`.
fn replace_from_tarball(archive: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    let f = fs::File::open(archive).map_err(|e| Error::io(archive, e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let bad = |e: std::io::Error| Error::Archive(format!("update archive: {e}"));
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for entry in tar.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        if !(name == "megamix" || name == "megamix-gui") || path.components().count() > 2 {
            continue;
        }
        let tmp = dir.join(format!(".{name}.update"));
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(bad)?;
        if buf.len() < 1024 || &buf[..4] != b"\x7fELF" {
            return Err(Error::Archive(format!(
                "{name} in the update isn't a program"
            )));
        }
        fs::write(&tmp, &buf).map_err(|e| Error::io(&tmp, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::io(&tmp, e))?;
        }
        staged.push((tmp, dir.join(&name)));
    }
    if !staged.iter().any(|(_, to)| to.ends_with("megamix-gui")) {
        for (tmp, _) in &staged {
            let _ = fs::remove_file(tmp);
        }
        return Err(Error::Archive(
            "the update doesn't contain megamix-gui".into(),
        ));
    }
    let mut replaced = Vec::new();
    for (tmp, to) in staged {
        // Rename over the old file: a running program keeps its old copy.
        fs::rename(&tmp, &to).map_err(|e| Error::io(&to, e))?;
        replaced.push(to);
    }
    Ok(replaced)
}

/// Start the (new) GUI again, detached, e.g. after an update.
pub fn relaunch_gui() -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| Error::Other(e.to_string()))?;
    // Linux: after the file was replaced, /proc/self/exe says "… (deleted)".
    let exe = match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) => PathBuf::from(s),
        None => exe,
    };
    let mut c = std::process::Command::new(exe);
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    c.spawn()
        .map(|_| ())
        .map_err(|e| Error::Other(format!("couldn't restart: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.2"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.0.0-beta.1"), Some((1, 0, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn picks_this_pcs_asset() {
        let json = r#"{"tag_name":"v9.0.0","name":"","body":"notes","html_url":"https://x/r","draft":false,"prerelease":false,
          "assets":[{"name":"megamix-linux-x86_64.tar.gz","size":10,"browser_download_url":"https://x/l64"},
                    {"name":"megamix-linux-aarch64.tar.gz","size":11,"browser_download_url":"https://x/la"},
                    {"name":"MegaMixModManager-9.0.0-Setup-x64.exe","size":12,"browser_download_url":"https://x/w64"},
                    {"name":"MegaMixModManager-9.0.0-Setup-arm64.exe","size":13,"browser_download_url":"https://x/wa"},
                    {"name":"SHA256SUMS.txt","size":1,"browser_download_url":"https://x/sums"}]}"#;
        let r: GhRelease = serde_json::from_str(json).unwrap();
        let rel = newer_from(r.clone(), "0.1.0").unwrap();
        assert_eq!(rel.version, "9.0.0");
        assert_eq!(rel.title, "Version 9.0.0");
        assert_eq!(rel.sums_url.as_deref(), Some("https://x/sums"));
        let a = rel.asset.unwrap();
        assert_eq!(Some(a.name), asset_name_for_this_pc("9.0.0"));
        assert!(newer_from(r, "9.0.0").is_none());
    }

    #[test]
    fn replaces_binaries_from_a_tarball() {
        let t = crate::mods::tests::disk_tempdir();
        let dir = t.path().join("bin");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("megamix-gui"), b"old").unwrap();
        let prog = |tag: &[u8]| {
            let mut b = b"\x7fELF".to_vec();
            b.extend_from_slice(tag);
            b.resize(2048, 0);
            b
        };
        let p = t.path().join("u.tar.gz");
        let f = fs::File::create(&p).unwrap();
        let mut tb = tar::Builder::new(flate2::write::GzEncoder::new(
            f,
            flate2::Compression::fast(),
        ));
        for (name, data) in [
            ("megamix-linux-x86_64/megamix-gui", prog(b"gui")),
            ("megamix-linux-x86_64/megamix", prog(b"cli")),
            ("megamix-linux-x86_64/install.sh", b"#!/bin/sh".to_vec()),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tb.append_data(&mut h, name, &data[..]).unwrap();
        }
        tb.into_inner().unwrap().finish().unwrap();
        let r = replace_from_tarball(&p, &dir).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(&fs::read(dir.join("megamix-gui")).unwrap()[4..7], b"gui");
        assert_eq!(&fs::read(dir.join("megamix")).unwrap()[4..7], b"cli");
        assert!(!dir.join("install.sh").exists());
    }
}
