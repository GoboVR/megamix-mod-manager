//! Streaming downloads of mod archives.
//!
//! Files go to `<mods>/.megamix-downloads/<job>/` — on the same drive as
//! the mods, never `/tmp` (RAM on SteamOS/PB-OS) — straight to disk in
//! small chunks, never whole in memory. A dropped connection is retried
//! with a range request that continues where it stopped (`If-Range` makes
//! the server send the whole file again if it changed). The size, and the
//! MD5 when the site gives one, are checked at the end.
//!
//! On success the caller installs the file and drops the [`Download`],
//! which deletes its folder. Cancelled or damaged downloads are deleted
//! right away. If the network gives up, the partial file is kept so the
//! next attempt continues; leftovers older than three days are removed.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::install::{check_staging, sanitize_folder};
use crate::net::{Client, host_of, net_error};
use crate::remote::RemoteFile;

pub const DOWNLOAD_DIR: &str = ".megamix-downloads";

const ATTEMPTS: u32 = 6;
const CHUNK: usize = 256 * 1024;
const LEFTOVER_AGE: Duration = Duration::from_secs(3 * 24 * 3600);
const FINISHED_AGE: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub struct Progress {
    pub done: u64,
    /// 0 = unknown.
    pub total: u64,
    /// Bytes per second over the last few seconds.
    pub speed: u64,
    /// Set while waiting to retry: (attempt, reason).
    pub retry: Option<(u32, String)>,
}

/// A finished download. Dropping it deletes the file and its folder.
#[derive(Debug)]
pub struct Download {
    pub path: PathBuf,
    dir: PathBuf,
}

impl Drop for Download {
    fn drop(&mut self) {
        remove_job_dir(&self.dir);
    }
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct Meta {
    url: String,
    size: u64,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

fn remove_job_dir(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
    if let Some(parent) = dir.parent() {
        let _ = fs::remove_dir(parent); // only if empty
    }
}

/// Delete leftover download folders: finished downloads (no `.part`,
/// so nothing to resume) after an hour, partial ones after three days.
pub fn cleanup_leftovers(root: &Path) {
    let Ok(rd) = fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let age = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());
        let partial = fs::read_dir(e.path()).is_ok_and(|rd| {
            rd.flatten()
                .any(|f| f.file_name().to_string_lossy().ends_with(".part"))
        });
        let limit = if partial { LEFTOVER_AGE } else { FINISHED_AGE };
        if age.is_none_or(|a| a > limit) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
    let _ = fs::remove_dir(root); // only if empty
}

/// Download `file` into the mods folder's download area. `job` names the
/// folder (stable per file, so an interrupted download can continue).
/// `room_for_unpacking`: also require this much extra free space (the
/// estimated unpacked size), so a download isn't wasted on a full drive.
pub fn download(
    client: &Client,
    mods_dir: &Path,
    job: &str,
    file: &RemoteFile,
    room_for_unpacking: u64,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<Download> {
    let root = mods_dir.join(DOWNLOAD_DIR);
    cleanup_leftovers(&root);
    fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
    let dir = root.join(sanitize_folder(job));
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let name = sanitize_folder(&file.name);
    let part = dir.join(format!("{name}.part"));
    let meta_path = dir.join("download.json");

    // Forget a partial file from a different URL or size.
    let meta: Meta = fs::read(&meta_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut meta = if meta.url == file.url && meta.size == file.size && part.exists() {
        meta
    } else {
        let _ = fs::remove_file(&part);
        Meta {
            url: file.url.clone(),
            size: file.size,
            ..Meta::default()
        }
    };
    let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let left = file.size.saturating_sub(have);
    if let Err(e) = check_staging(&root, left + room_for_unpacking) {
        remove_job_dir(&dir);
        return Err(e);
    }

    let result = fetch_with_retries(client, file, &part, &meta_path, &mut meta, progress, cancel);
    match result {
        Ok(()) => {}
        Err(e @ (Error::Network { .. } | Error::Http { .. })) => return Err(e), // keep .part
        Err(e) => {
            remove_job_dir(&dir);
            return Err(e);
        }
    }

    if let Err(e) = verify(&part, file, cancel) {
        remove_job_dir(&dir);
        return Err(e);
    }
    let path = dir.join(&name);
    fs::rename(&part, &path).map_err(|e| Error::io(&path, e))?;
    let _ = fs::remove_file(&meta_path);
    Ok(Download { path, dir })
}

fn sleep_unless_cancelled(d: Duration, cancel: &AtomicBool) -> Result<()> {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(100).min(end - Instant::now()));
    }
    Ok(())
}

enum Attempt {
    Done,
    /// Retry (keeping what we have) after the error.
    Retry(Error),
}

fn fetch_with_retries(
    client: &Client,
    file: &RemoteFile,
    part: &Path,
    meta_path: &Path,
    meta: &mut Meta,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut last_err = None;
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            let wait = Duration::from_secs(1 << (attempt - 2).min(4)); // 1 2 4 8 16
            let reason = last_err
                .as_ref()
                .map(|e: &Error| e.to_string())
                .unwrap_or_default();
            progress(&Progress {
                done: fs::metadata(part).map(|m| m.len()).unwrap_or(0),
                total: file.size,
                speed: 0,
                retry: Some((attempt, reason)),
            });
            sleep_unless_cancelled(wait, cancel)?;
        }
        match attempt_once(client, file, part, meta_path, meta, progress, cancel)? {
            Attempt::Done => return Ok(()),
            Attempt::Retry(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::Network {
        host: host_of(&file.url).to_owned(),
        message: "download failed".into(),
    }))
}

fn attempt_once(
    client: &Client,
    file: &RemoteFile,
    part: &Path,
    meta_path: &Path,
    meta: &mut Meta,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<Attempt> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut offset = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if file.size > 0 && offset == file.size {
        return Ok(Attempt::Done);
    }
    if file.size > 0 && offset > file.size {
        let _ = fs::remove_file(part);
        offset = 0;
    }

    client.pace(&file.url);
    let mut req = client.download_agent().get(&file.url);
    if offset > 0 {
        req = req.header("Range", format!("bytes={offset}-"));
        if let Some(v) = meta.etag.as_ref().or(meta.last_modified.as_ref()) {
            req = req.header("If-Range", v.as_str());
        }
    }
    let mut resp = match req.call() {
        Ok(r) => r,
        Err(e) => return Ok(Attempt::Retry(net_error(&file.url, e))),
    };
    let status = resp.status().as_u16();
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    match status {
        200 => offset = 0,
        206 => {
            // "bytes <start>-<end>/<total>"
            let start = header("content-range")
                .and_then(|v| v.strip_prefix("bytes ").map(str::to_owned))
                .and_then(|v| {
                    v.split('-')
                        .next()
                        .and_then(|s| s.trim().parse::<u64>().ok())
                });
            if start != Some(offset) {
                let _ = fs::remove_file(part);
                return Ok(Attempt::Retry(Error::BadResponse {
                    url: file.url.clone(),
                    message: "server resumed at the wrong place; starting over".into(),
                }));
            }
        }
        416 => {
            // Asked past the end: we probably have it all; verify decides.
            if file.size == 0 || offset >= file.size {
                return Ok(Attempt::Done);
            }
            let _ = fs::remove_file(part);
            return Ok(Attempt::Retry(Error::Http {
                url: file.url.clone(),
                status,
            }));
        }
        404 | 410 => return Err(Error::RemoteNotFound(file.url.clone())),
        408 | 425 | 429 | 500..=599 => {
            return Ok(Attempt::Retry(Error::Http {
                url: file.url.clone(),
                status,
            }));
        }
        _ => {
            return Err(Error::Http {
                url: file.url.clone(),
                status,
            });
        }
    }
    let content_len: Option<u64> = header("content-length").and_then(|v| v.parse().ok());
    meta.etag = header("etag").filter(|e| !e.starts_with("W/"));
    meta.last_modified = header("last-modified");
    if let Ok(b) = serde_json::to_vec(&*meta) {
        let _ = fs::write(meta_path, b);
    }
    let total = if file.size > 0 {
        file.size
    } else {
        content_len.map(|l| l + offset).unwrap_or(0)
    };

    let mut out = if offset == 0 {
        File::create(part)
    } else {
        OpenOptions::new().append(true).open(part)
    }
    .map_err(|e| Error::io(part, e))?;

    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; CHUNK];
    let mut done = offset;
    let mut last_report = Instant::now() - Duration::from_secs(1);
    let mut window = (Instant::now(), done);
    let mut speed = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            return Err(Error::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = out.flush();
                return Ok(Attempt::Retry(Error::Network {
                    host: host_of(&file.url).to_owned(),
                    message: e.to_string(),
                }));
            }
        };
        out.write_all(&buf[..n]).map_err(|e| Error::io(part, e))?;
        done += n as u64;
        if total > 0 && done > total {
            drop(out);
            let _ = fs::remove_file(part);
            return Err(Error::DownloadDamaged {
                file: file.name.clone(),
                detail: format!("got more data than the expected {total} bytes"),
            });
        }
        if last_report.elapsed() >= Duration::from_millis(100) {
            let el = window.0.elapsed();
            if el >= Duration::from_secs(2) {
                speed = ((done - window.1) as f64 / el.as_secs_f64()) as u64;
                window = (Instant::now(), done);
            } else if speed == 0 && el >= Duration::from_millis(500) {
                speed = ((done - window.1) as f64 / el.as_secs_f64()) as u64;
            }
            last_report = Instant::now();
            progress(&Progress {
                done,
                total,
                speed,
                retry: None,
            });
        }
    }
    out.sync_all().map_err(|e| Error::io(part, e))?;
    progress(&Progress {
        done,
        total,
        speed,
        retry: None,
    });
    if total > 0 && done < total {
        return Ok(Attempt::Retry(Error::Network {
            host: host_of(&file.url).to_owned(),
            message: format!("connection closed early ({done} of {total} bytes)"),
        }));
    }
    Ok(Attempt::Done)
}

/// Starts like HTML (a login, quota or virus-scan page)?
fn looks_like_web_page(path: &Path) -> bool {
    let mut head = [0u8; 512];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&head[..n])
        .trim_start()
        .to_ascii_lowercase();
    text.starts_with("<!doctype html") || text.starts_with("<html") || text.starts_with("<head")
}

fn verify(part: &Path, file: &RemoteFile, cancel: &AtomicBool) -> Result<()> {
    let len = fs::metadata(part).map_err(|e| Error::io(part, e))?.len();
    if file.size > 0 && len != file.size {
        return Err(Error::DownloadDamaged {
            file: file.name.clone(),
            detail: format!("size is {len} bytes, the site says {}", file.size),
        });
    }
    if len == 0 {
        return Err(Error::DownloadDamaged {
            file: file.name.clone(),
            detail: "empty file".into(),
        });
    }
    if looks_like_web_page(part) {
        return Err(Error::Other(format!(
            "{} sent a web page instead of the file (its download limit may be reached, or the file isn't shared publicly). Open the link in a browser and install the downloaded file instead.",
            crate::net::host_of(&file.url)
        )));
    }
    if let Some(want) = &file.md5 {
        let mut f = File::open(part).map_err(|e| Error::io(part, e))?;
        let mut h = Md5::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = f.read(&mut buf).map_err(|e| Error::io(part, e))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !got.eq_ignore_ascii_case(want) {
            return Err(Error::DownloadDamaged {
                file: file.name.clone(),
                detail: format!("MD5 is {got}, the site says {want}"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Against a tiny local HTTP server that can cut connections and
    //! honours range requests.
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    struct Server {
        url: String,
        requests: Arc<AtomicUsize>,
    }

    /// Serves `data`. The first `cut_first` responses stop after
    /// `cut_at` bytes. `etag` is sent with every response.
    fn serve(data: Vec<u8>, cut_first: usize, cut_at: usize, etag: &'static str) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.zip", l.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let reqs = requests.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut s) = stream else { continue };
                let n = reqs.fetch_add(1, Ordering::SeqCst);
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut range_start = None;
                let mut if_range_ok = true;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let l = line.to_ascii_lowercase();
                    if let Some(v) = l.strip_prefix("range: bytes=") {
                        range_start = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                    if let Some(v) = l.strip_prefix("if-range: ") {
                        if_range_ok = v.trim() == etag.to_ascii_lowercase();
                    }
                }
                let start = if if_range_ok {
                    range_start.unwrap_or(0)
                } else {
                    0
                };
                let body = &data[start..];
                let head = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        data.len() - 1,
                        data.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = s.write_all(head.as_bytes());
                let send = if n < cut_first {
                    &body[..cut_at.min(body.len())]
                } else {
                    body
                };
                let _ = s.write_all(send);
                let _ = s.flush();
                // Dropping `s` closes the connection (early, if cut).
            }
        });
        Server { url, requests }
    }

    fn file_for(url: &str, data: &[u8], md5: bool) -> RemoteFile {
        RemoteFile {
            id: 1,
            name: "Cool Mod.zip".into(),
            size: data.len() as u64,
            url: url.into(),
            md5: md5.then(|| {
                Md5::digest(data)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect()
            }),
            added: 0,
        }
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn mods_dir() -> (tempfile::TempDir, PathBuf) {
        let t = crate::mods::tests::disk_tempdir();
        let m = t.path().join("mods");
        fs::create_dir_all(&m).unwrap();
        (t, m)
    }

    #[test]
    fn downloads_and_cleans_up() {
        let d = data(1_000_000);
        let srv = serve(d.clone(), 0, 0, "\"e1\"");
        let (_t, mods) = mods_dir();
        let c = Client::with_cache_dir(None);
        let dl = download(
            &c,
            &mods,
            "gb-1-1",
            &file_for(&srv.url, &d, true),
            0,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(&dl.path).unwrap(), d);
        assert!(dl.path.starts_with(mods.join(DOWNLOAD_DIR)));
        drop(dl);
        assert!(
            !mods.join(DOWNLOAD_DIR).exists(),
            "download area removed when empty"
        );
    }

    #[test]
    fn resumes_after_dropped_connections() {
        let d = data(3_000_000);
        let srv = serve(d.clone(), 2, 700_000, "\"e1\"");
        let (_t, mods) = mods_dir();
        let c = Client::with_cache_dir(None);
        let mut retries = 0;
        let dl = download(
            &c,
            &mods,
            "job",
            &file_for(&srv.url, &d, true),
            0,
            &mut |p| retries += p.retry.is_some() as usize,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(&dl.path).unwrap(), d);
        assert_eq!(retries, 2);
        assert_eq!(srv.requests.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn changed_file_restarts_instead_of_mixing() {
        let d = data(500_000);
        let (_t, mods) = mods_dir();
        // A partial file from an earlier attempt with a different ETag.
        let dir = mods.join(DOWNLOAD_DIR).join("job");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Cool Mod.zip.part"), vec![0xAAu8; 100_000]).unwrap();
        let srv = serve(d.clone(), 0, 0, "\"new\"");
        let meta = Meta {
            url: srv.url.clone(),
            size: d.len() as u64,
            etag: Some("\"old\"".into()),
            last_modified: None,
        };
        fs::write(
            dir.join("download.json"),
            serde_json::to_vec(&meta).unwrap(),
        )
        .unwrap();
        let c = Client::with_cache_dir(None);
        let dl = download(
            &c,
            &mods,
            "job",
            &file_for(&srv.url, &d, false),
            0,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(&dl.path).unwrap(), d);
    }

    #[test]
    fn bad_checksum_is_deleted() {
        let d = data(10_000);
        let srv = serve(d.clone(), 0, 0, "\"e\"");
        let (_t, mods) = mods_dir();
        let mut f = file_for(&srv.url, &d, false);
        f.md5 = Some("00000000000000000000000000000000".into());
        let c = Client::with_cache_dir(None);
        let e = download(
            &c,
            &mods,
            "job",
            &f,
            0,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(e, Error::DownloadDamaged { .. }), "{e}");
        assert!(!mods.join(DOWNLOAD_DIR).exists());
    }

    #[test]
    fn cancel_deletes_partial_file() {
        let d = data(2_000_000);
        let srv = serve(d.clone(), 0, 0, "\"e\"");
        let (_t, mods) = mods_dir();
        let cancel = AtomicBool::new(false);
        let c = Client::with_cache_dir(None);
        let e = download(
            &c,
            &mods,
            "job",
            &file_for(&srv.url, &d, false),
            0,
            &mut |p| {
                if p.done > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(e, Error::Cancelled), "{e}");
        assert!(!mods.join(DOWNLOAD_DIR).exists());
    }

    #[test]
    fn refuses_when_space_is_short() {
        let d = data(1000);
        let (_t, mods) = mods_dir();
        let c = Client::with_cache_dir(None);
        let e = download(
            &c,
            &mods,
            "job",
            &file_for("http://127.0.0.1:9/x", &d, false),
            u64::MAX / 4,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(e, Error::NotEnoughSpace { .. }), "{e}");
        assert!(!mods.join(DOWNLOAD_DIR).exists());
    }
}
