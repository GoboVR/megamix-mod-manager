//! DivaModArchive (<https://divamodarchive.com>) public API.
//!
//! - search: `GET /api/v1/posts?query=&sort=&filter=&offset=&limit=`
//!   (sort `time:desc` default, `time:asc`, `download_count:desc`,
//!   `like_count:desc`; filter `post_type=Song|Cover|Module|UI|Plugin|Other`)
//! - one post: `GET /api/v1/posts/<id>`
//! - files: `GET /api/v1/posts/<id>/download/<n>` (redirects to the host,
//!   which supports range requests)
//! - one-click link: `divamodmanager:dma/<id>`

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::net::{self, Client, Fetched};
use crate::remote::{ModKind, RemoteFile, RemoteMod, Site, SortBy};

pub const BASE: &str = "https://divamodarchive.com";

/// Page size used for searches (same as the site).
pub const PAGE_SIZE: usize = 40;

const SEARCH_TTL: Duration = Duration::from_secs(10 * 60);
const POST_TTL: Duration = Duration::from_secs(5 * 60);
const PAGE_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Deserialize)]
pub struct Post {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub file_names: Vec<String>,
    #[serde(default)]
    pub file_sizes: Vec<u64>,
    /// Last upload/update time (RFC 3339).
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub post_type: String,
    #[serde(default)]
    pub download_count: u64,
    #[serde(default)]
    pub like_count: u64,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub dependencies: Option<Vec<Dependency>>,
    /// Post id → note ("Optional", "Required for console charts").
    #[serde(default)]
    pub dependency_descriptions: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    pub id: u64,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

fn sort_param(s: SortBy) -> Option<&'static str> {
    match s {
        SortBy::Relevance | SortBy::Newest => None, // site default: time:desc
        SortBy::Updated => None,
        SortBy::Downloads => Some("download_count:desc"),
        SortBy::Likes => Some("like_count:desc"),
    }
}

fn kind_param(k: ModKind) -> Option<&'static str> {
    Some(match k {
        ModKind::Any => return None,
        ModKind::Song => "Song",
        ModKind::Cover => "Cover",
        ModKind::Module => "Module",
        ModKind::Ui => "UI",
        ModKind::Plugin => "Plugin",
        ModKind::Other => "Other",
    })
}

pub fn search_url(query: &str, sort: SortBy, kind: ModKind, page: usize) -> String {
    let mut q = Vec::new();
    if !query.trim().is_empty() {
        q.push(format!("query={}", net::encode(query.trim())));
    }
    if let Some(s) = sort_param(sort) {
        q.push(format!("sort={}", net::encode(s)));
    }
    if let Some(k) = kind_param(kind) {
        q.push(format!("filter={}", net::encode(&format!("post_type={k}"))));
    }
    if page > 0 {
        q.push(format!("offset={}", page * PAGE_SIZE));
    }
    q.push(format!("limit={PAGE_SIZE}"));
    format!("{BASE}/api/v1/posts?{}", q.join("&"))
}

pub fn search(
    client: &Client,
    query: &str,
    sort: SortBy,
    kind: ModKind,
    page: usize,
) -> Result<Fetched<Vec<RemoteMod>>> {
    let url = search_url(query, sort, kind, page);
    let f: Fetched<Vec<Post>> = client.get_json(&url, SEARCH_TTL)?;
    Ok(Fetched {
        value: f
            .value
            .into_iter()
            .filter(|p| !p.private)
            .map(to_remote)
            .collect(),
        stale: f.stale,
    })
}

pub fn post(client: &Client, id: u64, fresh: bool) -> Result<Fetched<RemoteMod>> {
    let url = format!("{BASE}/api/v1/posts/{id}");
    let ttl = if fresh { Duration::ZERO } else { POST_TTL };
    let f: Fetched<Post> = client.get_json(&url, ttl)?;
    Ok(Fetched {
        value: to_remote(f.value),
        stale: f.stale,
    })
}

pub fn page_url(id: u64) -> String {
    format!("{BASE}/post/{id}")
}

pub fn to_remote(p: Post) -> RemoteMod {
    let updated = chrono::DateTime::parse_from_rfc3339(&p.time)
        .map(|t| t.timestamp())
        .unwrap_or(0);
    let files = p
        .files
        .iter()
        .enumerate()
        .map(|(i, url)| RemoteFile {
            id: i as u64,
            name: p
                .file_names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("{}-{i}.zip", p.id)),
            size: p.file_sizes.get(i).copied().unwrap_or(0),
            url: if url.starts_with("http") {
                url.clone()
            } else {
                format!("{BASE}/api/v1/posts/{}/download/{i}", p.id)
            },
            md5: None,
            added: updated,
        })
        .collect();
    let thumbnail = p.images.first().map(|u| thumb_url(u));
    RemoteMod {
        site: Site::Dma,
        id: p.id,
        name: p.name.trim().to_owned(),
        authors: p
            .authors
            .iter()
            .map(|a| {
                a.display_name
                    .clone()
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| a.name.clone())
            })
            .collect(),
        category: p.post_type.clone(),
        kind: ModKind::from_label(&p.post_type),
        description: p.text,
        images: p.images,
        thumbnail,
        files,
        downloads: Some(p.download_count),
        likes: p.like_count,
        updated,
        version: None,
        page_url: page_url(p.id),
        explicit: p.explicit,
        details_loaded: true,
        also_on: None,
        external: vec![],
        requires: p
            .dependencies
            .iter()
            .flatten()
            .map(|d| {
                let note = p
                    .dependency_descriptions
                    .as_ref()
                    .and_then(|m| m.get(&d.id.to_string()))
                    .cloned()
                    .unwrap_or_default();
                crate::remote::Requirement {
                    r: crate::remote::ModRef {
                        site: Site::Dma,
                        id: d.id,
                        file: None,
                    },
                    name: d.name.trim().to_owned(),
                    optional: note.to_ascii_lowercase().contains("optional"),
                    note,
                }
            })
            .collect(),
        notices: None,
    }
}

/// Notices the site shows at the top of a post's web page. They are worked
/// out by the site and are not in the API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageNotices {
    /// "… ExPatch must be installed to access these songs."
    pub needs: Vec<PageNeed>,
    /// "This mod (may) conflict(s) with the following" tables.
    pub conflicts: Vec<PageConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageNeed {
    /// The whole sentence.
    pub text: String,
    /// The linked mod's name ("ExPatch", "New Classics").
    pub name: String,
    /// Its DivaModArchive post, if the link points to one.
    pub post: Option<u64>,
    /// Otherwise the link (GitHub releases, …).
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageConflict {
    /// From the red "conflicts" box (song ids reserved by another post);
    /// `false` = the yellow "may conflict" box (database ids).
    pub sure: bool,
    /// Table heading without the colon: "PVs", "Modules", "Sprite IDs", …
    pub section: String,
    pub id: String,
    pub name: String,
    pub other_name: String,
    /// The other post's name, or e.g. "MM+" for the base game.
    pub source: String,
    pub post: Option<u64>,
}

impl PageNotices {
    pub fn is_empty(&self) -> bool {
        self.needs.is_empty() && self.conflicts.is_empty()
    }
}

/// Read the notices from the post's web page.
pub fn page_notices(client: &Client, id: u64) -> Result<PageNotices> {
    let f = client.get_bytes(&page_url(id), PAGE_TTL)?;
    Ok(parse_notices(&String::from_utf8_lossy(&f.value)))
}

/// Parse the alert boxes above the post's main card.
pub fn parse_notices(html: &str) -> PageNotices {
    let mut out = PageNotices::default();
    let end = html
        .find("<div class=\"col-md-7")
        .or_else(|| html.find("col-md-7"))
        .unwrap_or(html.len());
    let head = &html[..end];
    let mut starts: Vec<(usize, bool)> = Vec::new();
    for (tag, sure) in [("alert alert-danger", true), ("alert alert-warning", false)] {
        starts.extend(head.match_indices(tag).map(|(i, _)| (i, sure)));
    }
    starts.sort();
    for (n, &(at, sure)) in starts.iter().enumerate() {
        let stop = starts.get(n + 1).map(|s| s.0).unwrap_or(head.len());
        let block = &head[at..stop];
        let Some(gt) = block.find('>') else { continue };
        let body = &block[gt + 1..];
        if body.contains("<table") {
            parse_tables(body, sure, &mut out.conflicts);
        } else {
            for para in body.split("<p>").map(str::trim).filter(|p| !p.is_empty()) {
                let text = clean(para);
                if text.is_empty() {
                    continue;
                }
                let (name, post, url) = match first_link(para) {
                    Some((href, name)) => {
                        let post = href
                            .trim_start_matches(BASE)
                            .strip_prefix("/post/")
                            .and_then(|r| r.split(['/', '?', '#']).next())
                            .and_then(|n| n.parse().ok());
                        let url = post.is_none().then(|| href.clone());
                        (name, post, url)
                    }
                    None => (String::new(), None, None),
                };
                out.needs.push(PageNeed {
                    text,
                    name,
                    post,
                    url,
                });
            }
        }
    }
    out
}

fn parse_tables(body: &str, sure: bool, out: &mut Vec<PageConflict>) {
    // Each section: <h6 …>PVs:</h6><table>…<tbody><tr><td>…</table>
    for part in body.split("<h6").skip(1) {
        let Some(gt) = part.find('>') else { continue };
        let rest = &part[gt + 1..];
        let section = clean(&rest[..rest.find("</h6>").unwrap_or(0)])
            .trim_end_matches(':')
            .trim()
            .to_owned();
        let Some(tb) = rest.find("<tbody>") else {
            continue;
        };
        let table = &rest[tb + 7..rest.find("</table>").unwrap_or(rest.len())];
        for row in table.split("<tr>").map(str::trim).filter(|r| !r.is_empty()) {
            let cells: Vec<&str> = row.split("<td>").skip(1).collect();
            if cells.len() < 4 {
                continue;
            }
            let post = first_link(cells[3]).and_then(|(href, _)| {
                href.trim_start_matches(BASE)
                    .strip_prefix("/post/")
                    .and_then(|n| n.parse().ok())
            });
            out.push(PageConflict {
                sure,
                section: section.clone(),
                id: clean(cells[0]),
                name: clean(cells[1]),
                other_name: clean(cells[2]),
                source: clean(cells[3]),
                post,
            });
        }
    }
}

/// `(href, link text)` of the first `<a>` in `s`.
fn first_link(s: &str) -> Option<(String, String)> {
    let a = s.find("<a ")?;
    let tag_end = a + s[a..].find('>')?;
    let tag = &s[a..tag_end];
    let h = tag.find("href=")? + 5;
    let v = &tag[h..];
    let href = if let Some(q) = v.strip_prefix('"') {
        &q[..q.find('"')?]
    } else if let Some(q) = v.strip_prefix('\'') {
        &q[..q.find('\'')?]
    } else {
        &v[..v.find([' ', '>']).unwrap_or(v.len())]
    };
    let close = s[tag_end..]
        .find("</a>")
        .map(|i| tag_end + i)
        .unwrap_or(s.len());
    Some((unescape(href), clean(&s[tag_end + 1..close])))
}

/// Text without tags, entities decoded, spaces collapsed.
fn clean(s: &str) -> String {
    let mut t = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => t.push(c),
            _ => {}
        }
    }
    unescape(&t)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Smaller image variant for lists (Cloudflare Images "thumbnail").
fn thumb_url(u: &str) -> String {
    match u.strip_suffix("/public") {
        Some(base) => format!("{base}/thumbnail"),
        None => u.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_post() {
        let json = r#"[{"id":243,"name":"Mera Mera ","text":"hi","images":["https://divamodarchive.com/cdn-cgi/imagedelivery/x/y/public"],
            "files":["https://divamodarchive.com/api/v1/posts/243/download/0"],"time":"2026-09-08T05:42:27.456732Z",
            "post_type":"Song","download_count":293,"like_count":0,
            "authors":[{"id":464596514510077963,"name":"mikurisu39","avatar":"a","display_name":"mikurisu39_"}],
            "dependencies":null,"dependency_descriptions":null,"file_names":["Mera Mera.zip"],"file_sizes":[24698868],
            "private":false,"explicit":false,"explicit_reason":null}]"#;
        let posts: Vec<Post> = serde_json::from_str(json).unwrap();
        let m = to_remote(posts.into_iter().next().unwrap());
        assert_eq!(m.name, "Mera Mera");
        assert_eq!(m.kind, ModKind::Song);
        assert_eq!(m.authors, ["mikurisu39_"]);
        assert_eq!(m.files[0].name, "Mera Mera.zip");
        assert_eq!(m.files[0].size, 24698868);
        assert_eq!(m.updated, 1788846147);
        assert_eq!(
            m.thumbnail.as_deref(),
            Some("https://divamodarchive.com/cdn-cgi/imagedelivery/x/y/thumbnail")
        );
    }

    #[test]
    fn parses_page_notices() {
        let html = r#"<div class="row align-items-md-stretch gap-3 m-1"><div class="alert alert-danger mb-0"><p>This mod contains songs that do not have a hard chart, <a href=https://github.com/nastys/ExPatch/releases/latest>ExPatch</a> must be installed to access these songs.<p>This mod contains songs that do not have an arcade chart, <a href=/post/169>New Classics</a> must be installed to access these songs.</div><div class="alert alert-danger mb-0"><button class="accordion accordion-button p-1 pb-0 collapsed"data-bs-target=#reservationConflicts data-bs-toggle=collapse type=button><h5 class=mb-0>This mod conflicts with the following:</h5></button><div class="p-1 pb-0 collapse"id=reservationConflicts><h6 class=mt-2>PVs:</h6><table class="table table-sm m-0"><thead><tr><th>ID<th>Name<th>Conflict Name<th>Conflict source<tbody><tr><td>8227<td>Bad ∞ End ∞ Night<td>Bad ∞ End ∞ Night<td><a href=/post/402>lavverso Song Pack</a><tr><td>8262<td>Thriller &amp; Teto<td>Thriller<td><a href=/post/134>Nitori&#39;s Song Pack</a></table></div></div><div class="alert alert-warning mb-0"><button type=button><h5 class=mb-0>This mod may conflict with the following:</h5></button><div id=dbConflicts><h6 class=mt-2>Sprite IDs:</h6><table class="table table-sm m-0"><thead><tr><th>ID<th>Name<th>Conflicting Name<th>Conflict Source<tbody><tr><td>47847<td>SPR_A<td>SPR_B<td>MM+</table></div></div><div class="col-md-7 card card-body"><div class="alert alert-danger">not a notice</div>"#;
        let n = parse_notices(html);
        assert_eq!(n.needs.len(), 2);
        assert_eq!(n.needs[0].name, "ExPatch");
        assert_eq!(n.needs[0].post, None);
        assert_eq!(
            n.needs[0].url.as_deref(),
            Some("https://github.com/nastys/ExPatch/releases/latest")
        );
        assert!(
            n.needs[0]
                .text
                .starts_with("This mod contains songs that do not have a hard chart, ExPatch must")
        );
        assert_eq!(n.needs[1].post, Some(169));
        assert_eq!(n.conflicts.len(), 3);
        let c = &n.conflicts[1];
        assert!(c.sure);
        assert_eq!((c.section.as_str(), c.id.as_str()), ("PVs", "8262"));
        assert_eq!(c.name, "Thriller & Teto");
        assert_eq!(c.source, "Nitori's Song Pack");
        assert_eq!(c.post, Some(134));
        let w = &n.conflicts[2];
        assert!(!w.sure);
        assert_eq!(
            (w.section.as_str(), w.source.as_str(), w.post),
            ("Sprite IDs", "MM+", None)
        );

        assert!(parse_notices("<html><div class=\"col-md-7\">x</div>").is_empty());
    }

    #[test]
    fn builds_search_urls() {
        assert_eq!(
            search_url("", SortBy::Newest, ModKind::Any, 0),
            "https://divamodarchive.com/api/v1/posts?limit=40"
        );
        assert_eq!(
            search_url(" miku ", SortBy::Downloads, ModKind::Ui, 2),
            "https://divamodarchive.com/api/v1/posts?query=miku&sort=download_count%3Adesc&filter=post_type%3DUI&offset=80&limit=40"
        );
    }
}
