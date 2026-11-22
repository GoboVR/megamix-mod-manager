//! Before installing a mod from a site: which of its requirements are
//! missing, and whether its page (or our list of known problems) says it
//! doesn't work with a mod that's installed.

use serde::Serialize;

use crate::mods::Game;
use crate::records::{self, Source};
use crate::remote::{ModRef, RemoteMod, Site};

#[derive(Debug, Clone, Serialize)]
pub struct ReqState {
    pub name: String,
    /// Its page on a mod site (`None` for e.g. ExPatch, which is on GitHub).
    pub r: Option<ModRef>,
    /// Where to get it when it isn't on a mod site.
    pub url: Option<String>,
    pub note: String,
    pub optional: bool,
    /// Installed folder, if it's installed.
    pub installed: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Compat {
    pub requirements: Vec<ReqState>,
    /// Problems with installed mods, as sentences.
    pub incompatible: Vec<String>,
}

impl Compat {
    pub fn missing_required(&self) -> Vec<&ReqState> {
        self.requirements
            .iter()
            .filter(|r| !r.optional && r.installed.is_none())
            .collect()
    }
    pub fn has_problems(&self) -> bool {
        !self.missing_required().is_empty() || !self.incompatible.is_empty()
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Installed mods: (folder, display name).
fn installed(game: &Game) -> Vec<(String, String)> {
    game.mods
        .iter()
        .filter(|m| m.enabled())
        .map(|m| (m.folder.clone(), m.name().to_owned()))
        .collect()
}

/// Is the mod `r` (called `name` on its page) installed? By where installed
/// mods came from, else by name.
fn find_installed(game: &Game, r: Option<&ModRef>, name: &str) -> Option<String> {
    if let Some(r) = r {
        let recs = records::load(&game.dir);
        let src = match r.site {
            Site::Dma => Source::DivaModArchive { id: r.id },
            Site::GameBanana => Source::GameBanana { id: r.id },
        };
        if let Some((f, _)) = recs
            .iter()
            .find(|(f, rec)| rec.source == src && game.mods.iter().any(|m| &m.folder == *f))
        {
            return Some(f.clone());
        }
    }
    let want = norm(name);
    if want.len() < 3 {
        return None;
    }
    installed(game)
        .into_iter()
        .find(|(f, n)| norm(n) == want || norm(f) == want)
        .map(|(f, _)| f)
}

const INCOMPAT_WORDS: &[&str] = &[
    "not compatible",
    "incompatib",
    "isn't compatible",
    "isnt compatible",
    "doesn't work with",
    "does not work with",
    "conflicts with",
    "do not use",
    "don't use",
];

pub fn check(game: &Game, m: &RemoteMod) -> Compat {
    let mut requirements: Vec<ReqState> = m
        .requires
        .iter()
        .map(|q| ReqState {
            name: q.name.clone(),
            r: Some(q.r),
            url: None,
            note: q.note.clone(),
            optional: q.optional,
            installed: find_installed(game, Some(&q.r), &q.name),
        })
        .collect();
    let notices = m.notices.clone().unwrap_or_default();
    // "… ExPatch must be installed to access these songs."
    for need in notices.needs.iter().filter(|n| !n.name.is_empty()) {
        let r = need.post.map(|id| ModRef {
            site: Site::Dma,
            id,
            file: None,
        });
        let note = need_note(&need.text);
        if let Some(q) = requirements
            .iter_mut()
            .find(|q| (r.is_some() && q.r == r) || norm(&q.name) == norm(&need.name))
        {
            // Already a dependency; the site's notice makes it needed.
            if q.optional {
                q.optional = false;
                q.note = note;
            }
            continue;
        }
        requirements.push(ReqState {
            name: need.name.clone(),
            installed: find_installed(game, r.as_ref(), &need.name),
            r,
            url: need.url.clone(),
            note,
            optional: false,
        });
    }

    // The mod itself (when reinstalling) doesn't count.
    let own = norm(&m.name);
    let others: Vec<(String, String)> = installed(game)
        .into_iter()
        .filter(|(f, n)| norm(n) != own && norm(f) != own)
        .collect();
    let named: Vec<(String, String)> = others.iter().map(|(f, n)| (n.clone(), f.clone())).collect();
    let mut incompatible = crate::sort::known_incompatibilities(&m.name, "", &named);

    // Sentences on the page like "not compatible with the Eden Project".
    let text = m.description.replace(['\r'], "");
    for sentence in text
        .split(['.', '!', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let low = sentence.to_lowercase();
        if !INCOMPAT_WORDS.iter().any(|w| low.contains(w)) {
            continue;
        }
        let ns = norm(sentence);
        for (folder, name) in &others {
            let key = norm(name);
            let hit = (key.len() >= 5 && ns.contains(&key)) || {
                let fk = norm(folder);
                fk.len() >= 5 && ns.contains(&fk)
            };
            if hit {
                let clean: String = sentence
                    .trim_matches(['*', '-', '#', ' '])
                    .chars()
                    .take(200)
                    .collect();
                let msg = format!("The page says: “{clean}” — you have {name}.");
                if !incompatible.contains(&msg) {
                    incompatible.push(msg);
                }
            }
        }
    }
    // Conflict tables from the site: only ones with mods that are installed.
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for c in &notices.conflicts {
        if c.post == Some(m.id) && m.site == Site::Dma {
            continue;
        }
        let r = c.post.map(|id| ModRef {
            site: Site::Dma,
            id,
            file: None,
        });
        if r.is_none() && c.source.contains('+') {
            continue; // "MM+": the base game
        }
        let Some(folder) = find_installed(game, r.as_ref(), &c.source) else {
            continue;
        };
        if norm(&folder) == own {
            continue;
        }
        let item = if c.name.is_empty() {
            format!("{} {}", c.section, c.id)
        } else {
            format!("{} {}", c.id, c.name)
        };
        match groups
            .iter_mut()
            .find(|(f, sure, _)| *f == folder && *sure == c.sure)
        {
            Some(g) => g.2.push(item),
            None => groups.push((folder, c.sure, vec![item])),
        }
    }
    for (folder, sure, items) in groups {
        let mut list = items.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        if items.len() > 3 {
            list.push_str(&format!(" and {} more", items.len() - 3));
        }
        incompatible.push(if sure {
            format!("Uses the same song IDs as {folder}, which you have: {list}. Only one of them will work.")
        } else {
            format!("May clash with {folder}, which you have (same IDs: {list}).")
        });
    }
    Compat {
        requirements,
        incompatible,
    }
}

/// "…do not have a hard chart, ExPatch must…" → "for songs with no hard chart".
fn need_note(text: &str) -> String {
    let low = text.to_lowercase();
    if let Some(i) = low.find("do not have a") {
        let rest = &text[i + "do not have a".len()..];
        let what = rest.split(',').next().unwrap_or("").trim();
        let what = what.strip_prefix("n ").unwrap_or(what).trim();
        if !what.is_empty() {
            return format!("needed for songs with no {what}");
        }
    }
    "the site says it's needed".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;
    use crate::remote::{ModKind, Requirement};

    fn page(name: &str, text: &str, requires: Vec<Requirement>) -> RemoteMod {
        RemoteMod {
            site: Site::Dma,
            id: 1,
            name: name.into(),
            authors: vec![],
            category: String::new(),
            kind: ModKind::Song,
            description: text.into(),
            images: vec![],
            thumbnail: None,
            files: vec![],
            downloads: None,
            likes: 0,
            updated: 0,
            version: None,
            page_url: String::new(),
            explicit: false,
            details_loaded: true,
            also_on: None,
            external: vec![],
            requires,
            notices: None,
        }
    }

    fn req(id: u64, name: &str, note: &str) -> Requirement {
        Requirement {
            r: ModRef {
                site: Site::Dma,
                id,
                file: None,
            },
            name: name.into(),
            optional: note.to_lowercase().contains("optional"),
            note: note.into(),
        }
    }

    #[test]
    fn requirements_and_incompatibilities() {
        let t = fake_game(
            &["New Classics", "Eden"],
            &[
                ("New Classics", "name = \"New Classics\"\n"),
                ("Eden", "name = \"Eden Project Core\"\n"),
            ],
        );
        let g = Game::open(t.path()).unwrap();
        let m = page(
            "Project Hoshikuzu+ Song Pack",
            "Great pack.\nHoshi+ is ***explicitly not compatible with the Eden Project***.",
            vec![
                req(169, "New Classics", ""),
                req(371, "ExPatch", ""),
                req(128, "Subtitle", "Optional"),
            ],
        );
        let c = check(&g, &m);
        assert_eq!(c.requirements[0].installed.as_deref(), Some("New Classics"));
        let missing: Vec<_> = c
            .missing_required()
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(missing, ["ExPatch"]);
        assert!(
            c.incompatible.iter().any(|w| w.contains("Eden")),
            "{:?}",
            c.incompatible
        );
        assert!(c.has_problems());

        let ok = check(&g, &page("Other", "Works with everything.", vec![]));
        assert!(!ok.has_problems());
    }

    #[test]
    fn page_notices() {
        use crate::dma::{PageConflict, PageNeed, PageNotices};
        let t = fake_game(
            &["lavverso", "New Classics"],
            &[
                ("lavverso", "name = \"lavverso Song Pack\"\n"),
                ("New Classics", "name = \"New Classics\"\n"),
            ],
        );
        let g = Game::open(t.path()).unwrap();
        let conflict = |id: &str, source: &str, post: Option<u64>, sure: bool| PageConflict {
            sure,
            section: "PVs".into(),
            id: id.into(),
            name: format!("Song {id}"),
            other_name: String::new(),
            source: source.into(),
            post,
        };
        let mut m = page(
            "Halloween Pack",
            "",
            vec![req(169, "New Classics", "Optional")],
        );
        m.notices = Some(PageNotices {
            needs: vec![
                PageNeed {
                    text: "This mod contains songs that do not have a hard chart, ExPatch must be installed to access these songs.".into(),
                    name: "ExPatch".into(),
                    post: None,
                    url: Some("https://github.com/nastys/ExPatch/releases/latest".into()),
                },
                PageNeed {
                    text: "This mod contains songs that do not have an arcade chart, New Classics must be installed.".into(),
                    name: "New Classics".into(),
                    post: Some(169),
                    url: None,
                },
            ],
            conflicts: vec![
                conflict("8227", "lavverso Song Pack", Some(402), true),
                conflict("8249", "lavverso Song Pack", Some(402), true),
                conflict("8262", "Nitori's Song Pack", Some(134), true),
                conflict("1", "MM+", None, false),
            ],
        });
        let c = check(&g, &m);
        assert_eq!(c.requirements.len(), 2, "{:?}", c.requirements);
        assert!(
            !c.requirements[0].optional,
            "notice makes New Classics needed"
        );
        assert_eq!(c.requirements[0].installed.as_deref(), Some("New Classics"));
        let ex = &c.requirements[1];
        assert_eq!(
            (ex.name.as_str(), ex.r, ex.installed.as_ref()),
            ("ExPatch", None, None)
        );
        assert_eq!(ex.note, "needed for songs with no hard chart");
        assert_eq!(c.incompatible.len(), 1, "{:?}", c.incompatible);
        assert!(
            c.incompatible[0]
                .contains("as lavverso, which you have: 8227 Song 8227, 8249 Song 8249."),
            "{}",
            c.incompatible[0]
        );
    }
}
