//! GameBanana (<https://gamebanana.com>) API v11, Project DIVA Mega Mix+
//! only (game 16522).
//!
//! - list: `GET /apiv11/Mod/Index?_nPage=&_nPerpage=&_aFilters[Generic_Game]=16522
//!   [&_aFilters[Generic_Category]=<id>]&_sSort=Generic_Newest|…`
//! - search: `GET /apiv11/Util/Search/Results?_sModelName=Mod&_idGameRow=16522
//!   &_sSearchString=&_sOrder=best_match|date|udate&_nPage=` (15 per page)
//! - one mod: `GET /apiv11/Mod/<id>/ProfilePage` (files with size + MD5)
//! - files: `https://gamebanana.com/dl/<file id>` (redirects; range requests work)
//! - one-click link: `divamodmanager:https://gamebanana.com/mmdl/<file id>,Mod,<mod id>`

use std::time::Duration;

use serde::Deserialize;

use crate::error::Result;
use crate::net::{self, Client, Fetched};
use crate::remote::{ModKind, RemoteFile, RemoteMod, Site, SortBy};

pub const BASE: &str = "https://gamebanana.com";
pub const GAME_ID: u64 = 16522;
pub const PAGE_SIZE: usize = 30;

const LIST_TTL: Duration = Duration::from_secs(10 * 60);
const PROFILE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Deserialize)]
struct Page<T> {
    #[serde(rename = "_aMetadata", default)]
    metadata: Metadata,
    #[serde(rename = "_aRecords", default = "Vec::new")]
    records: Vec<T>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Metadata {
    #[serde(rename = "_bIsComplete", default)]
    is_complete: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    #[serde(rename = "_idRow")]
    pub id: u64,
    #[serde(rename = "_sModelName", default)]
    pub model: String,
    #[serde(rename = "_sName", default)]
    pub name: String,
    #[serde(rename = "_aPreviewMedia", default)]
    pub preview: Option<Preview>,
    #[serde(rename = "_aSubmitter", default)]
    pub submitter: Option<Member>,
    #[serde(rename = "_aRootCategory", default)]
    pub root_category: Option<Category>,
    #[serde(rename = "_aCategory", default)]
    pub category: Option<Category>,
    #[serde(rename = "_sVersion", default)]
    pub version: String,
    #[serde(rename = "_tsDateAdded", default)]
    pub date_added: i64,
    #[serde(rename = "_tsDateModified", default)]
    pub date_modified: i64,
    #[serde(rename = "_tsDateUpdated", default)]
    pub date_updated: Option<i64>,
    #[serde(rename = "_nLikeCount", default)]
    pub likes: u64,
    #[serde(rename = "_nDownloadCount", default)]
    pub downloads: Option<u64>,
    #[serde(rename = "_bHasContentRatings", default)]
    pub has_content_ratings: bool,
    #[serde(rename = "_bIsObsolete", default)]
    pub obsolete: bool,
    #[serde(rename = "_sText", default)]
    pub text: String,
    #[serde(rename = "_aFiles", default)]
    pub files: Option<Vec<File>>,
    #[serde(rename = "_bIsPrivate", default)]
    pub private: bool,
    #[serde(rename = "_bIsTrashed", default)]
    pub trashed: bool,
    #[serde(rename = "_bIsWithheld", default)]
    pub withheld: bool,
    #[serde(rename = "_aAlternateFileSources", default)]
    pub alternate_sources: Option<Vec<AlternateSource>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlternateSource {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
}

/// A tiny stand-in file ("download_from_alternate_file_below.zip",
/// "download-from-external.txt") uploaded because GameBanana needs a file;
/// the real download is an alternate source.
pub fn is_placeholder(name: &str, size: u64) -> bool {
    let n = name.to_ascii_lowercase();
    let wording = [
        "download_from",
        "download-from",
        "downloadfrom",
        "do_not_download",
        "alternate",
        "external",
        "google_drive",
        "drive_link",
        "readme",
        "read_me",
    ]
    .iter()
    .any(|w| n.contains(w));
    size < 64 * 1024 && (wording || n.ends_with(".txt") || n.ends_with(".url"))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Preview {
    #[serde(rename = "_aImages", default)]
    pub images: Vec<Image>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Image {
    #[serde(rename = "_sBaseUrl", default)]
    pub base_url: String,
    #[serde(rename = "_sFile", default)]
    pub file: String,
    #[serde(rename = "_sFile530", default)]
    pub file530: Option<String>,
    #[serde(rename = "_sFile220", default)]
    pub file220: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    #[serde(rename = "_sName", default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    #[serde(rename = "_sName", default)]
    pub name: String,
    #[serde(rename = "_sProfileUrl", default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct File {
    #[serde(rename = "_idRow")]
    pub id: u64,
    #[serde(rename = "_sFile", default)]
    pub name: String,
    #[serde(rename = "_nFilesize", default)]
    pub size: u64,
    #[serde(rename = "_tsDateAdded", default)]
    pub date_added: i64,
    #[serde(rename = "_sDownloadUrl", default)]
    pub download_url: String,
    #[serde(rename = "_sMd5Checksum", default)]
    pub md5: String,
    #[serde(rename = "_bIsArchived", default)]
    pub archived: bool,
}

/// Root category ids for the type filter.
fn category_id(k: ModKind) -> Option<u64> {
    Some(match k {
        ModKind::Any => return None,
        ModKind::Song => 17345,   // Custom Songs
        ModKind::Cover => 17311,  // Covers
        ModKind::Module => 17315, // Customization
        ModKind::Ui => 17306,     // User Interface
        ModKind::Plugin => 17530, // Patches
        ModKind::Other => 17288,  // Other/Misc
    })
}

/// Map a root category name to a type.
pub fn kind_of_category(name: &str) -> ModKind {
    match name {
        "Custom Songs" => ModKind::Song,
        "Covers" => ModKind::Cover,
        "Customization" => ModKind::Module,
        "User Interface" => ModKind::Ui,
        "Patches" | "Restorations & Fixes" => ModKind::Plugin,
        _ => ModKind::Other,
    }
}

pub fn list_url(sort: SortBy, kind: ModKind, page: usize) -> String {
    let sort = match sort {
        SortBy::Relevance | SortBy::Newest => "Generic_Newest",
        SortBy::Updated => "Generic_LatestUpdated",
        SortBy::Downloads => "Generic_MostDownloaded",
        SortBy::Likes => "Generic_MostLiked",
    };
    let mut url = format!(
        "{BASE}/apiv11/Mod/Index?_nPage={}&_nPerpage={PAGE_SIZE}&_aFilters%5BGeneric_Game%5D={GAME_ID}&_sSort={sort}",
        page + 1
    );
    if let Some(c) = category_id(kind) {
        url.push_str(&format!("&_aFilters%5BGeneric_Category%5D={c}"));
    }
    url
}

pub fn search_url(query: &str, sort: SortBy, page: usize) -> String {
    let order = match sort {
        SortBy::Newest => "date",
        SortBy::Updated => "udate",
        _ => "best_match",
    };
    format!(
        "{BASE}/apiv11/Util/Search/Results?_sModelName=Mod&_sOrder={order}&_idGameRow={GAME_ID}&_sSearchString={}&_nPage={}",
        net::encode(query.trim()),
        page + 1
    )
}

/// List or search mods. Returns the mods and whether more pages exist.
/// Search results can't be filtered by type on the site, so that filter
/// is applied to the page afterwards.
pub fn search(
    client: &Client,
    query: &str,
    sort: SortBy,
    kind: ModKind,
    page: usize,
) -> Result<Fetched<(Vec<RemoteMod>, bool)>> {
    let searching = query.trim().chars().count() >= 2;
    let url = if searching {
        search_url(query, sort, page)
    } else {
        list_url(sort, kind, page)
    };
    let f: Fetched<Page<Record>> = client.get_json(&url, LIST_TTL)?;
    let more = !f.value.metadata.is_complete && !f.value.records.is_empty();
    let mods = f
        .value
        .records
        .into_iter()
        .filter(|r| r.model.is_empty() || r.model == "Mod")
        .filter(|r| !r.private && !r.trashed && !r.withheld)
        .map(to_remote)
        .filter(|m| !searching || kind == ModKind::Any || m.kind == kind)
        .collect();
    Ok(Fetched {
        value: (mods, more),
        stale: f.stale,
    })
}

pub fn profile(client: &Client, id: u64, fresh: bool) -> Result<Fetched<RemoteMod>> {
    let url = format!("{BASE}/apiv11/Mod/{id}/ProfilePage");
    let ttl = if fresh { Duration::ZERO } else { PROFILE_TTL };
    let f: Fetched<Record> = client.get_json(&url, ttl)?;
    if f.value.private || f.value.trashed || f.value.withheld {
        return Err(crate::Error::RemoteNotFound(page_url(id)));
    }
    Ok(Fetched {
        value: to_remote(f.value),
        stale: f.stale,
    })
}

pub fn page_url(id: u64) -> String {
    format!("{BASE}/mods/{id}")
}

pub fn to_remote(r: Record) -> RemoteMod {
    let images: Vec<String> = r
        .preview
        .as_ref()
        .map(|p| {
            p.images
                .iter()
                .filter(|i| !i.file.is_empty())
                .map(|i| format!("{}/{}", i.base_url, i.file))
                .collect()
        })
        .unwrap_or_default();
    let thumbnail = r.preview.as_ref().and_then(|p| p.images.first()).map(|i| {
        let f = i.file530.as_ref().or(i.file220.as_ref()).unwrap_or(&i.file);
        format!("{}/{}", i.base_url, f)
    });
    let category = r
        .root_category
        .as_ref()
        .or(r.category.as_ref())
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let details_loaded = r.files.is_some();
    let mut files: Vec<RemoteFile> = r
        .files
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.archived)
        .filter(|f| !is_placeholder(&f.name, f.size))
        .map(|f| RemoteFile {
            id: f.id,
            name: f.name,
            size: f.size,
            url: if f.download_url.is_empty() {
                format!("{BASE}/dl/{}", f.id)
            } else {
                f.download_url
            },
            md5: (f.md5.len() == 32).then(|| f.md5.to_ascii_lowercase()),
            added: f.date_added,
        })
        .collect();
    // Newest file first.
    files.sort_by_key(|f| std::cmp::Reverse(f.added));
    let newest_file = files.iter().map(|f| f.added).max().unwrap_or(0);
    RemoteMod {
        site: Site::GameBanana,
        id: r.id,
        name: r.name.trim().to_owned(),
        authors: r.submitter.map(|m| vec![m.name]).unwrap_or_default(),
        kind: kind_of_category(&category),
        category,
        description: html_to_text(&r.text),
        images,
        thumbnail,
        files,
        downloads: r.downloads,
        likes: r.likes,
        updated: r
            .date_updated
            .unwrap_or(0)
            .max(r.date_added)
            .max(newest_file),
        version: Some(r.version).filter(|v| !v.trim().is_empty()),
        page_url: page_url(r.id),
        explicit: r.has_content_ratings,
        details_loaded,
        also_on: None,
        external: r
            .alternate_sources
            .unwrap_or_default()
            .iter()
            .filter(|a| a.url.starts_with("https://") || a.url.starts_with("http://"))
            .map(|a| crate::remote::ExternalLink::new(&a.url, &a.description))
            .collect(),
        requires: vec![],
        notices: None,
    }
}

/// Plain text from GameBanana's HTML descriptions.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            rest = &rest[i..];
            break;
        };
        let tag = rest[i + 1..i + j].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match name {
            "br" => out.push('\n'),
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "ul" | "ol" | "blockquote" | "tr" => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
            }
            "li" if !tag.starts_with('/') => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("• ");
            }
            _ => {}
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    let text = decode_entities(&out);
    // Collapse runs of blank lines.
    let mut cleaned = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        cleaned.push_str(l);
        cleaned.push('\n');
    }
    cleaned.trim().to_owned()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        // Entities are short ASCII; look for `;` in the first bytes only.
        let end = tail.bytes().take(12).position(|b| b == b';');
        let decoded = end.and_then(|e| {
            let ent = &tail[1..e];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    u32::from_str_radix(&ent[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_descriptions_become_text() {
        assert_eq!(
            html_to_text(
                "<i>There must be a reason...</i><br><br>This mod &amp; that.<ul><li>One</li><li>Two</li></ul><p>&#12511;&#x30AF; end</p>"
            ),
            "There must be a reason...\n\nThis mod & that.\n• One\n• Two\nミク end"
        );
        // `&` followed by multi-byte text, no `;` nearby.
        assert_eq!(html_to_text("A&Bアイウエオ; x"), "A&Bアイウエオ; x");
    }

    #[test]
    fn parses_a_profile() {
        let json = r#"{"_idRow":743721,"_sName":"Love Options - BESTie","_sText":"<b>Hi</b>",
          "_aPreviewMedia":{"_aImages":[{"_sBaseUrl":"https://images.gamebanana.com/img/ss/mods","_sFile":"a.jpg","_sFile530":"530-90_a.jpg"}]},
          "_aSubmitter":{"_sName":"Genmo"},"_aCategory":{"_sName":"Custom Songs","_sProfileUrl":"x"},
          "_sVersion":"","_tsDateAdded":1795280918,"_tsDateModified":1795280918,"_nDownloadCount":9,
          "_aFiles":[{"_idRow":1898594,"_sFile":"love_options_bestie_.zip","_nFilesize":61314748,"_tsDateAdded":1795280824,
          "_sDownloadUrl":"https://gamebanana.com/dl/1898594","_sMd5Checksum":"CEED6D0CA11B096E2812FAC331CC1296","_bIsArchived":false}]}"#;
        let r: Record = serde_json::from_str(json).unwrap();
        let m = to_remote(r);
        assert!(m.details_loaded);
        assert_eq!(m.kind, ModKind::Song);
        assert_eq!(m.version, None);
        assert_eq!(m.description, "Hi");
        assert_eq!(
            m.files[0].md5.as_deref(),
            Some("ceed6d0ca11b096e2812fac331cc1296")
        );
        assert_eq!(
            m.thumbnail.as_deref(),
            Some("https://images.gamebanana.com/img/ss/mods/530-90_a.jpg")
        );
        assert_eq!(
            m.images,
            ["https://images.gamebanana.com/img/ss/mods/a.jpg"]
        );
    }

    #[test]
    fn placeholders_and_alternate_sources() {
        let json = r#"{"_idRow":460243,"_sName":"Yoru Star Song Pack","_aFiles":[{"_idRow":1368390,
          "_sFile":"download_from_alternate_file_below_d6cc5.zip","_nFilesize":363,"_tsDateAdded":1737649449}],
          "_aAlternateFileSources":[{"url":"https://drive.google.com/drive/folders/1Wi?usp=sharing","description":"Pack V5.5"},
          {"url":"https://drive.google.com/file/d/ABC/view","description":"Update"}]}"#;
        let m = to_remote(serde_json::from_str(json).unwrap());
        assert!(m.files.is_empty());
        assert_eq!(m.external.len(), 2);
        assert_eq!(m.external[0].direct, None);
        assert!(m.external[1].direct.is_some());
        assert!(is_placeholder("download-from-external.txt", 40));
        assert!(is_placeholder(
            "use_google_drive_link_for_full_download.7z",
            2000
        ));
        assert!(is_placeholder("do_not_download_a3f4e.rar", 500));
        assert!(!is_placeholder("Song Pack.zip", 300));
        assert!(!is_placeholder("download_from_alternate.zip", 900_000_000));
    }

    #[test]
    fn builds_urls() {
        assert_eq!(
            list_url(SortBy::Downloads, ModKind::Song, 1),
            "https://gamebanana.com/apiv11/Mod/Index?_nPage=2&_nPerpage=30&_aFilters%5BGeneric_Game%5D=16522&_sSort=Generic_MostDownloaded&_aFilters%5BGeneric_Category%5D=17345"
        );
        assert!(
            search_url("miku pack", SortBy::Relevance, 0)
                .ends_with("_sSearchString=miku%20pack&_nPage=1")
        );
    }
}
