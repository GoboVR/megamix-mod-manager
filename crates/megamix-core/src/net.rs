//! HTTP access for the mod sites: one shared blocking client with a polite
//! User-Agent, a minimum gap between requests to the same host, and a small
//! disk cache for API answers and images. When a site can't be reached, an
//! old cached answer is used instead (marked as stale), so browsing keeps
//! working offline and the app never depends on a site being up.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

pub const USER_AGENT: &str = concat!(
    "megamix-mod-manager/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/GoboVR/megamix-mod-manager)"
);

/// Minimum time between two requests to the same host.
const MIN_GAP: Duration = Duration::from_millis(300);

/// Largest API answer or image accepted (not used for mod downloads).
const MAX_SMALL_BODY: u64 = 32 << 20;

/// A cached answer older than this is deleted on cleanup.
const CACHE_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

/// Shared HTTP client. Cheap to clone; clones share the pacing state.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    /// Agent for big downloads: no overall timeout, only per read.
    download_agent: ureq::Agent,
    cache_dir: Option<PathBuf>,
    last_request: Arc<Mutex<HashMap<String, Instant>>>,
}

/// An answer, and whether it came from an old cache entry because the
/// site couldn't be reached.
#[derive(Debug, Clone)]
pub struct Fetched<T> {
    pub value: T,
    pub stale: bool,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self::with_cache_dir(dirs::cache_dir().map(|d| d.join("megamix").join("http")))
    }

    /// `None` = no disk cache (tests).
    pub fn with_cache_dir(cache_dir: Option<PathBuf>) -> Self {
        let base = || {
            ureq::Agent::config_builder()
                .user_agent(USER_AGENT)
                .http_status_as_error(false)
                .max_redirects(8)
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_per_read(Some(Duration::from_secs(30)))
        };
        let agent = base()
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        let download_agent = base().build().into();
        Client {
            agent,
            download_agent,
            cache_dir,
            last_request: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Wait so requests to one host are at least [`MIN_GAP`] apart.
    pub(crate) fn pace(&self, url: &str) {
        let host = host_of(url).to_owned();
        loop {
            let wait = {
                let mut map = self.last_request.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                match map.get(&host) {
                    Some(t) if now.duration_since(*t) < MIN_GAP => MIN_GAP - now.duration_since(*t),
                    _ => {
                        map.insert(host.clone(), now);
                        return;
                    }
                }
            };
            std::thread::sleep(wait);
        }
    }

    pub(crate) fn download_agent(&self) -> &ureq::Agent {
        &self.download_agent
    }

    /// GET `url`, using a cached copy younger than `ttl` if there is one.
    /// On a network failure an older cached copy is returned as stale.
    pub fn get_bytes(&self, url: &str, ttl: Duration) -> Result<Fetched<Vec<u8>>> {
        self.get_cached(url, ttl, true)
    }

    /// An image (screenshot/thumbnail): cached for a week, and not paced
    /// (images come from CDNs; callers limit how many run at once).
    pub fn get_image(&self, url: &str) -> Result<Vec<u8>> {
        self.get_cached(url, Duration::from_secs(7 * 24 * 3600), false)
            .map(|f| f.value)
    }

    fn get_cached(&self, url: &str, ttl: Duration, paced: bool) -> Result<Fetched<Vec<u8>>> {
        let cache = self.cache_path(url);
        if let Some(p) = &cache
            && let Ok(meta) = fs::metadata(p)
            && age(&meta).is_some_and(|a| a < ttl)
            && let Ok(b) = fs::read(p)
        {
            return Ok(Fetched {
                value: b,
                stale: false,
            });
        }
        match self.fetch(url, paced) {
            Ok(b) => {
                if let Some(p) = &cache {
                    let _ = write_cache(p, &b);
                }
                Ok(Fetched {
                    value: b,
                    stale: false,
                })
            }
            Err(e @ Error::Network { .. }) | Err(e @ Error::Http { .. }) => {
                let is_5xx = matches!(&e, Error::Http { status, .. } if *status >= 500);
                if (matches!(e, Error::Network { .. }) || is_5xx)
                    && let Some(b) = cache.as_ref().and_then(|p| fs::read(p).ok())
                {
                    return Ok(Fetched {
                        value: b,
                        stale: true,
                    });
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// GET and parse JSON (cached like [`Client::get_bytes`]). A cached
    /// copy that doesn't parse is ignored.
    pub fn get_json<T: DeserializeOwned>(&self, url: &str, ttl: Duration) -> Result<Fetched<T>> {
        let f = self.get_bytes(url, ttl)?;
        match serde_json::from_slice(&f.value) {
            Ok(v) => Ok(Fetched {
                value: v,
                stale: f.stale,
            }),
            Err(e) => {
                if let Some(p) = self.cache_path(url) {
                    let _ = fs::remove_file(p);
                }
                Err(Error::BadResponse {
                    url: url.to_owned(),
                    message: format!("invalid JSON: {e}"),
                })
            }
        }
    }

    /// Delete cached answers older than two weeks.
    pub fn clean_cache(&self) {
        let Some(dir) = &self.cache_dir else { return };
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if let Ok(m) = e.metadata()
                && age(&m).is_none_or(|a| a > CACHE_MAX_AGE)
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    fn fetch(&self, url: &str, paced: bool) -> Result<Vec<u8>> {
        if paced {
            self.pace(url);
        }
        let mut resp = self.agent.get(url).call().map_err(|e| net_error(url, e))?;
        let status = resp.status().as_u16();
        if status == 404 || status == 410 {
            return Err(Error::RemoteNotFound(url.to_owned()));
        }
        if !(200..300).contains(&status) {
            return Err(Error::Http {
                url: url.to_owned(),
                status,
            });
        }
        let mut out = Vec::new();
        resp.body_mut()
            .as_reader()
            .take(MAX_SMALL_BODY + 1)
            .read_to_end(&mut out)
            .map_err(|e| Error::Network {
                host: host_of(url).to_owned(),
                message: e.to_string(),
            })?;
        if out.len() as u64 > MAX_SMALL_BODY {
            return Err(Error::BadResponse {
                url: url.to_owned(),
                message: "answer too large".into(),
            });
        }
        Ok(out)
    }

    fn cache_path(&self, url: &str) -> Option<PathBuf> {
        let dir = self.cache_dir.as_ref()?;
        let h = Sha256::digest(url.as_bytes());
        let name: String = h[..16].iter().map(|b| format!("{b:02x}")).collect();
        Some(dir.join(name))
    }
}

fn age(meta: &fs::Metadata) -> Option<Duration> {
    let m = meta.modified().ok()?;
    SystemTime::now().duration_since(m).ok()
}

fn write_cache(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Host part of a URL ("" if there is none).
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    host.rsplit_once('@').map_or(host, |(_, h)| h)
}

pub(crate) fn net_error(url: &str, e: ureq::Error) -> Error {
    let message = match &e {
        ureq::Error::HostNotFound => "host not found (offline?)".to_owned(),
        ureq::Error::Timeout(t) => format!("timed out ({t})"),
        ureq::Error::Io(io) => io.to_string(),
        other => other.to_string(),
    };
    Error::Network {
        host: host_of(url).to_owned(),
        message,
    }
}

/// Percent-encode a query-string value.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_encoding() {
        assert_eq!(
            host_of("https://divamodarchive.com/api/v1/posts?x=1"),
            "divamodarchive.com"
        );
        assert_eq!(host_of("https://gamebanana.com"), "gamebanana.com");
        assert_eq!(host_of("http://u:p@h.example:8080/x"), "h.example:8080");
        assert_eq!(encode("miku 39&ミ"), "miku%2039%26%E3%83%9F");
    }

    #[test]
    fn pacing_spaces_requests_per_host() {
        let c = Client::with_cache_dir(None);
        let t = Instant::now();
        c.pace("https://a.example/1");
        c.pace("https://b.example/1");
        assert!(t.elapsed() < MIN_GAP);
        c.pace("https://a.example/2");
        assert!(t.elapsed() >= MIN_GAP);
    }
}
