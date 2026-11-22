//! Mods on DivaModArchive and GameBanana, behind one set of types:
//! combined search (a mod on both sites is shown once, from the preferred
//! site), details, and parsing of ids, page URLs and one-click links.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::net::{Client, Fetched};
use crate::{dma, gamebanana};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Site {
    Dma,
    GameBanana,
}

impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Site::Dma => "DivaModArchive",
            Site::GameBanana => "GameBanana",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Site::Dma => "dma",
            Site::GameBanana => "gb",
        }
    }
    pub fn other(self) -> Site {
        match self {
            Site::Dma => Site::GameBanana,
            Site::GameBanana => Site::Dma,
        }
    }
}

/// Which sites to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sources {
    #[default]
    All,
    Dma,
    GameBanana,
}

impl Sources {
    pub fn includes(self, s: Site) -> bool {
        match self {
            Sources::All => true,
            Sources::Dma => s == Site::Dma,
            Sources::GameBanana => s == Site::GameBanana,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// Best match when searching, newest otherwise.
    #[default]
    Relevance,
    Newest,
    Updated,
    Downloads,
    Likes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModKind {
    #[default]
    Any,
    Song,
    Cover,
    Module,
    Ui,
    Plugin,
    Other,
}

impl ModKind {
    pub const ALL: [ModKind; 7] = [
        ModKind::Any,
        ModKind::Song,
        ModKind::Cover,
        ModKind::Module,
        ModKind::Ui,
        ModKind::Plugin,
        ModKind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ModKind::Any => "Any",
            ModKind::Song => "Song",
            ModKind::Cover => "Cover",
            ModKind::Module => "Module",
            ModKind::Ui => "UI",
            ModKind::Plugin => "Plugin",
            ModKind::Other => "Other",
        }
    }

    pub fn from_label(s: &str) -> ModKind {
        ModKind::ALL
            .into_iter()
            .find(|k| k.label().eq_ignore_ascii_case(s.trim()))
            .unwrap_or(ModKind::Other)
    }
}

/// A downloadable file of a mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    /// GameBanana file id, or the file's index on DivaModArchive.
    pub id: u64,
    pub name: String,
    /// 0 = unknown.
    pub size: u64,
    pub url: String,
    /// Lowercase hex MD5 (GameBanana only).
    pub md5: Option<String>,
    /// Unix time the file was uploaded (DMA: the post's update time).
    pub added: i64,
}

/// A mod on one of the sites.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteMod {
    pub site: Site,
    pub id: u64,
    pub name: String,
    pub authors: Vec<String>,
    /// The site's own category name.
    pub category: String,
    pub kind: ModKind,
    /// Plain text (DMA: Markdown source).
    pub description: String,
    /// Full-size screenshot URLs.
    pub images: Vec<String>,
    /// Small image for lists.
    pub thumbnail: Option<String>,
    /// Newest first. Empty until details are loaded on GameBanana.
    pub files: Vec<RemoteFile>,
    /// `None` = not known (GameBanana lists don't include it).
    pub downloads: Option<u64>,
    pub likes: u64,
    /// Unix time of the last update.
    pub updated: i64,
    pub version: Option<String>,
    pub page_url: String,
    /// Marked explicit / has content ratings.
    pub explicit: bool,
    /// `files` and `description` are complete (search results from
    /// GameBanana only have the basics).
    pub details_loaded: bool,
    /// The same mod (by name) on the other site, hidden by deduplication.
    pub also_on: Option<ModRef>,
    /// Downloads hosted on other sites (GameBanana "alternate file sources").
    #[serde(default)]
    pub external: Vec<ExternalLink>,
    /// Mods this one needs (DivaModArchive "dependencies").
    #[serde(default)]
    pub requires: Vec<Requirement>,
    /// Notices from the DivaModArchive web page (needs ExPatch, conflicts
    /// with other posts). `None` = not loaded; see [`dma::page_notices`].
    #[serde(default)]
    pub notices: Option<dma::PageNotices>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub r: ModRef,
    pub name: String,
    /// The author's note ("Optional", "Required for console charts").
    pub note: String,
    pub optional: bool,
}

/// File id used for downloads from an external link.
pub const EXTERNAL_FILE_ID: u64 = u64::MAX;

/// A download hosted somewhere else (Google Drive, MEGA, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalLink {
    pub url: String,
    pub description: String,
    /// A URL that downloads the file directly, if the host allows it;
    /// otherwise the link can only be opened in a browser.
    pub direct: Option<String>,
}

impl ExternalLink {
    pub fn new(url: &str, description: &str) -> Self {
        ExternalLink {
            url: url.trim().to_owned(),
            description: description.trim().to_owned(),
            direct: direct_download_url(url),
        }
    }

    /// Host name, for display ("drive.google.com").
    pub fn host(&self) -> &str {
        crate::net::host_of(&self.url)
    }

    /// A file to download for this link (named after the mod; the archive
    /// type is recognised from its content).
    pub fn as_file(&self, m: &RemoteMod) -> Option<RemoteFile> {
        Some(RemoteFile {
            id: EXTERNAL_FILE_ID,
            name: format!("{}.zip", crate::install::sanitize_folder(&m.name)),
            size: 0,
            url: self.direct.clone()?,
            md5: None,
            added: m.updated,
        })
    }
}

/// Turn a share link into a direct download link, for hosts that allow
/// it: Google Drive files, pixeldrain, Dropbox, and plain archive links.
/// Folders, MEGA (encrypted) and pages with captchas return `None`.
pub fn direct_download_url(url: &str) -> Option<String> {
    let url = url.trim();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }
    let host = crate::net::host_of(url).to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let after_host = &url[url.find(host).map(|i| i + host.len()).unwrap_or(0)..];
    let path = after_host.split(['?', '#']).next().unwrap_or("");
    let query = after_host
        .split_once('?')
        .map(|(_, q)| q.split('#').next().unwrap_or(""))
        .unwrap_or("");
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
            .map(str::to_owned)
    };
    let safe_id = |id: &str| {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    match host {
        "drive.google.com" | "docs.google.com" => {
            let id = path
                .strip_prefix("/file/d/")
                .map(|r| r.split('/').next().unwrap_or("").to_owned())
                .or_else(|| {
                    (path == "/open" || path == "/uc")
                        .then(|| param("id"))
                        .flatten()
                })?;
            safe_id(&id).then(|| format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"))
        }
        "drive.usercontent.google.com" => Some(url.to_owned()),
        "pixeldrain.com" | "pixeldrain.net" => {
            let id = path
                .strip_prefix("/u/")
                .or_else(|| path.strip_prefix("/api/file/"))?;
            let id = id.trim_end_matches('/');
            safe_id(id).then(|| format!("https://pixeldrain.com/api/file/{id}?download"))
        }
        "dropbox.com" => {
            if path.starts_with("/scl/fo/") || path.starts_with("/sh/") {
                return None; // folders
            }
            let base = url.split('#').next().unwrap_or(url);
            Some(if base.contains("dl=0") {
                base.replace("dl=0", "dl=1")
            } else if base.contains("dl=1") {
                base.to_owned()
            } else if base.contains('?') {
                format!("{base}&dl=1")
            } else {
                format!("{base}?dl=1")
            })
        }
        _ => {
            let p = path.to_ascii_lowercase();
            (p.ends_with(".zip") || p.ends_with(".7z") || p.ends_with(".rar"))
                .then(|| url.to_owned())
        }
    }
}

impl RemoteMod {
    pub fn mod_ref(&self) -> ModRef {
        ModRef {
            site: self.site,
            id: self.id,
            file: None,
        }
    }
}

/// A mod on a site, optionally one specific file of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModRef {
    pub site: Site,
    pub id: u64,
    /// GameBanana file id / DMA file index.
    pub file: Option<u64>,
}

impl std::fmt::Display for ModRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.site.short(), self.id)?;
        if let Some(file) = self.file {
            write!(f, "/{file}")?;
        }
        Ok(())
    }
}

/// Parse a mod reference:
/// - `dma:123`, `gb:123` (optionally `/<file>`)
/// - `divamodmanager:dma/123` (DivaModArchive one-click)
/// - `divamodmanager:https://gamebanana.com/mmdl/<file>,Mod,<mod>` (GameBanana one-click)
/// - `https://divamodarchive.com/post/123`, `https://gamebanana.com/mods/123`,
///   `https://gamebanana.com/dl/<file>` is not accepted (no mod id).
pub fn parse_ref(input: &str) -> Option<ModRef> {
    let s = input.trim().trim_matches(['"', '\'', '<', '>']);
    let lower = s.to_ascii_lowercase();
    // One-click links.
    if let Some(rest) = strip_prefix_ci(s, "divamodmanager:") {
        let rest = rest.trim_start_matches('/');
        if let Some(id) = strip_prefix_ci(rest, "dma/") {
            return Some(ModRef {
                site: Site::Dma,
                id: leading_number(id)?,
                file: None,
            });
        }
        // https://gamebanana.com/mmdl/<file>,Mod,<mod>
        if let Some(i) = rest.to_ascii_lowercase().find("/mmdl/") {
            let mut parts = rest[i + 6..].split(',');
            let file = leading_number(parts.next()?)?;
            let model = parts.next()?;
            if !model.eq_ignore_ascii_case("mod") {
                return None;
            }
            let id = leading_number(parts.next()?)?;
            return Some(ModRef {
                site: Site::GameBanana,
                id,
                file: Some(file),
            });
        }
        return parse_ref(rest)
            .filter(|_| !rest.to_ascii_lowercase().starts_with("divamodmanager:"));
    }
    // Short forms.
    for (prefix, site) in [("dma:", Site::Dma), ("gb:", Site::GameBanana)] {
        if let Some(rest) = strip_prefix_ci(s, prefix) {
            let (id, file) = match rest.split_once('/') {
                Some((a, b)) => (a, Some(b.parse().ok()?)),
                None => (rest, None),
            };
            return Some(ModRef {
                site,
                id: id.parse().ok()?,
                file,
            });
        }
    }
    // Page URLs.
    let no_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let no_scheme = no_scheme.strip_prefix("www.").unwrap_or(no_scheme);
    if let Some(p) = no_scheme.strip_prefix("divamodarchive.com/post/") {
        return Some(ModRef {
            site: Site::Dma,
            id: leading_number(p)?,
            file: None,
        });
    }
    if let Some(p) = no_scheme.strip_prefix("divamodarchive.com/posts/") {
        return Some(ModRef {
            site: Site::Dma,
            id: leading_number(p)?,
            file: None,
        });
    }
    if let Some(p) = no_scheme
        .strip_prefix("gamebanana.com/mods/")
        .filter(|p| p.starts_with(|c: char| c.is_ascii_digit()))
    {
        return Some(ModRef {
            site: Site::GameBanana,
            id: leading_number(p)?,
            file: None,
        });
    }
    if let Some(p) = no_scheme.strip_prefix("gamebanana.com/mods/download/") {
        return Some(ModRef {
            site: Site::GameBanana,
            id: leading_number(p)?,
            file: None,
        });
    }
    None
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix))
    .then(|| &s[prefix.len()..])
}

fn leading_number(s: &str) -> Option<u64> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s[..end].parse().ok()
}

/// Full details of one mod (GameBanana search results lack the files).
/// `fresh` skips the cache (before installing or checking for updates).
pub fn details(client: &Client, r: ModRef, fresh: bool) -> Result<Fetched<RemoteMod>> {
    match r.site {
        Site::Dma => dma::post(client, r.id, fresh),
        Site::GameBanana => gamebanana::profile(client, r.id, fresh),
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub query: String,
    pub sources: Sources,
    pub sort: SortBy,
    pub kind: ModKind,
    /// 0-based page.
    pub page: usize,
    /// Which copy to keep when a mod is on both sites.
    pub prefer: Option<Site>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub mods: Vec<RemoteMod>,
    /// A further page may have results.
    pub more: bool,
    /// Sites that failed, with the error. The others' results are still
    /// returned.
    pub errors: Vec<(Site, String)>,
    /// Some results came from an old cache because a site was unreachable.
    pub stale: bool,
}

/// Search both sites (in parallel) and merge the results.
pub fn search(client: &Client, o: &SearchOptions) -> SearchResults {
    let dma = o.sources.includes(Site::Dma).then(|| {
        let (c, o) = (client.clone(), o.clone());
        std::thread::spawn(move || dma::search(&c, &o.query, o.sort, o.kind, o.page))
    });
    let gb = o.sources.includes(Site::GameBanana).then(|| {
        let (c, o) = (client.clone(), o.clone());
        std::thread::spawn(move || gamebanana::search(&c, &o.query, o.sort, o.kind, o.page))
    });
    let mut res = SearchResults::default();
    let mut dma_mods = Vec::new();
    let mut gb_mods = Vec::new();
    if let Some(h) = dma {
        match h
            .join()
            .unwrap_or_else(|_| Err(Error::Other("search thread panicked".into())))
        {
            Ok(f) => {
                res.stale |= f.stale;
                res.more |= f.value.len() >= dma::PAGE_SIZE;
                dma_mods = f.value;
            }
            Err(e) => res.errors.push((Site::Dma, e.to_string())),
        }
    }
    if let Some(h) = gb {
        match h
            .join()
            .unwrap_or_else(|_| Err(Error::Other("search thread panicked".into())))
        {
            Ok(f) => {
                res.stale |= f.stale;
                res.more |= f.value.1;
                gb_mods = f.value.0;
            }
            Err(e) => res.errors.push((Site::GameBanana, e.to_string())),
        }
    }
    res.mods = merge(dma_mods, gb_mods, o.sort, o.prefer.unwrap_or(Site::Dma));
    res
}

/// Name used to spot the same mod on both sites.
pub fn match_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Merge two result lists: drop the non-preferred copy of a mod that is
/// on both (remembering it in `also_on`), then order the rest.
pub fn merge(
    dma: Vec<RemoteMod>,
    gb: Vec<RemoteMod>,
    sort: SortBy,
    prefer: Site,
) -> Vec<RemoteMod> {
    let (mut keep, mut other) = match prefer {
        Site::Dma => (dma, gb),
        Site::GameBanana => (gb, dma),
    };
    for m in &mut keep {
        let key = match_key(&m.name);
        if key.len() < 3 {
            continue;
        }
        if let Some(i) = other.iter().position(|o| match_key(&o.name) == key) {
            let o = other.remove(i);
            m.also_on = Some(o.mod_ref());
        }
    }
    // Order: by the sort key where it means the same on both sites,
    // otherwise alternate between the lists so both stay visible.
    let mut out = Vec::with_capacity(keep.len() + other.len());
    match sort {
        SortBy::Newest | SortBy::Updated => {
            out.extend(keep);
            out.extend(other);
            out.sort_by_key(|m| std::cmp::Reverse(m.updated));
        }
        _ => {
            let (mut a, mut b) = (keep.into_iter(), other.into_iter());
            loop {
                match (a.next(), b.next()) {
                    (None, None) => break,
                    (x, y) => out.extend(x.into_iter().chain(y)),
                }
            }
        }
    }
    out
}

/// One line of a mod's description, for lists.
pub fn summary(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '*', '>', '-', ' '])
                .trim()
        })
        .find(|l| !l.is_empty() && !l.starts_with("![") && !l.starts_with('<'))
        .unwrap_or("");
    if line.chars().count() > max_chars {
        let cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(site: Site, id: u64, file: Option<u64>) -> Option<ModRef> {
        Some(ModRef { site, id, file })
    }

    #[test]
    fn parses_refs() {
        assert_eq!(parse_ref("dma:243"), r(Site::Dma, 243, None));
        assert_eq!(
            parse_ref("GB:743721/1898594"),
            r(Site::GameBanana, 743721, Some(1898594))
        );
        assert_eq!(parse_ref("divamodmanager:dma/243"), r(Site::Dma, 243, None));
        assert_eq!(
            parse_ref("divamodmanager:https://gamebanana.com/mmdl/1898594,Mod,743721"),
            r(Site::GameBanana, 743721, Some(1898594))
        );
        assert_eq!(
            parse_ref("divamodmanager://dma/12/"),
            r(Site::Dma, 12, None)
        );
        assert_eq!(
            parse_ref("https://divamodarchive.com/post/243"),
            r(Site::Dma, 243, None)
        );
        assert_eq!(
            parse_ref(" https://www.gamebanana.com/mods/743721?x "),
            r(Site::GameBanana, 743721, None)
        );
        assert_eq!(
            parse_ref("gamebanana.com/mods/download/5#x"),
            r(Site::GameBanana, 5, None)
        );
        assert_eq!(parse_ref("https://gamebanana.com/mods/cats/17345"), None);
        assert_eq!(
            parse_ref("divamodmanager:https://gamebanana.com/mmdl/1,Sound,2"),
            None
        );
        assert_eq!(parse_ref("dma:x"), None);
        assert_eq!(parse_ref("my mod.zip"), None);
        assert_eq!(parse_ref("divamodmanager:divamodmanager:dma/1"), None);
    }

    fn m(site: Site, id: u64, name: &str, updated: i64) -> RemoteMod {
        RemoteMod {
            site,
            id,
            name: name.into(),
            authors: vec![],
            category: String::new(),
            kind: ModKind::Song,
            description: String::new(),
            images: vec![],
            thumbnail: None,
            files: vec![],
            downloads: None,
            likes: 0,
            updated,
            version: None,
            page_url: String::new(),
            explicit: false,
            details_loaded: true,
            also_on: None,
            external: vec![],
            requires: vec![],
            notices: None,
        }
    }

    #[test]
    fn direct_links() {
        let d = |u: &str| direct_download_url(u);
        assert_eq!(
            d("https://drive.google.com/file/d/1AbC-d_9/view?usp=sharing").as_deref(),
            Some(
                "https://drive.usercontent.google.com/download?id=1AbC-d_9&export=download&confirm=t"
            )
        );
        assert_eq!(
            d("https://drive.google.com/open?id=XYZ").as_deref(),
            Some("https://drive.usercontent.google.com/download?id=XYZ&export=download&confirm=t")
        );
        assert_eq!(
            d(
                "https://drive.google.com/drive/folders/1WiHDsLiOZ4fofG0Pi-rNvLH4eL5NGBX0?usp=sharing"
            ),
            None
        );
        assert_eq!(
            d("https://pixeldrain.com/u/abc123").as_deref(),
            Some("https://pixeldrain.com/api/file/abc123?download")
        );
        assert_eq!(d("https://pixeldrain.com/l/abc123"), None);
        assert_eq!(
            d("https://www.dropbox.com/s/x/pack.zip?dl=0").as_deref(),
            Some("https://www.dropbox.com/s/x/pack.zip?dl=1")
        );
        assert_eq!(d("https://www.dropbox.com/scl/fo/abc?rlkey=1"), None);
        assert_eq!(d("https://mega.nz/file/abc#key"), None);
        assert_eq!(
            d("https://example.com/files/Pack%20v2.7z").as_deref(),
            Some("https://example.com/files/Pack%20v2.7z")
        );
        assert_eq!(d("https://www.mediafire.com/file/x/pack.zip/file"), None);
        assert_eq!(d("javascript:alert(1).zip"), None);
    }

    #[test]
    fn merging_dedups_and_prefers() {
        let dma = vec![
            m(Site::Dma, 1, "Mera Mera!", 10),
            m(Site::Dma, 2, "Only DMA", 30),
        ];
        let gb = vec![
            m(Site::GameBanana, 7, "mera-mera", 20),
            m(Site::GameBanana, 8, "Only GB", 5),
        ];
        let out = merge(dma.clone(), gb.clone(), SortBy::Newest, Site::Dma);
        let ids: Vec<_> = out.iter().map(|m| (m.site, m.id)).collect();
        assert_eq!(ids, [(Site::Dma, 2), (Site::Dma, 1), (Site::GameBanana, 8)]);
        assert_eq!(out[1].also_on, r(Site::GameBanana, 7, None));

        let out = merge(dma, gb, SortBy::Downloads, Site::GameBanana);
        let ids: Vec<_> = out.iter().map(|m| (m.site, m.id)).collect();
        assert_eq!(
            ids,
            [(Site::GameBanana, 7), (Site::Dma, 2), (Site::GameBanana, 8)]
        );
        assert_eq!(out[0].also_on, r(Site::Dma, 1, None));
    }

    #[test]
    fn summaries() {
        assert_eq!(summary("\n# Title\n\nbody", 50), "Title");
        assert_eq!(summary("![img](x)\nabcdefghij", 5), "abcd…");
    }
}
