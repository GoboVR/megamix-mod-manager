//! Auto-sort: put mods in a working load order (first in `priority` wins).
//!
//! Groups, top to bottom:
//! 1. Mods the user pinned "always first".
//! 2. Song packs that ship their own thumbnail fix and say to go above the
//!    Thumbnail Manager (only when it is installed).
//! 3. Thumbnail tools (Thumbnail Manager, ThumbnailSplitter): their pages
//!    say they must be first in the priority list.
//! 4. Mods whose page says to be high up (Future Tone UI).
//! 5. Everything else, in its current order (manual choices are kept).
//! 6. Text mods (lyrics, translations, English patches): visual mods such
//!    as covers should win over mods that change text.
//! 7. Mods the user pinned "always last".
//!
//! Then "A above B" rules from mod pages are applied, and known
//! incompatibilities are reported as warnings. Sources are next to each
//! rule (checked 2026-11-21).
//!
//! Pins are stored per game in `<game>/.megamix/sort.json`. The order is
//! only changed after the caller shows the plan and the user agrees;
//! applying it takes a backup like every change.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::error::{Error, Result};
use crate::mods::{DATA_DIR, Game, SaveReport};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    /// Folders kept at the top (in this order).
    #[serde(default)]
    pub first: Vec<String>,
    /// Folders kept at the bottom (in this order).
    #[serde(default)]
    pub last: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    PinnedFirst,
    AboveThumbnailManager,
    Thumbnail,
    High,
    Normal,
    Text,
    PinnedLast,
}

impl Tier {
    pub fn reason(self) -> &'static str {
        match self {
            Tier::PinnedFirst => "pinned to the top",
            Tier::AboveThumbnailManager => "its page says to go above the Thumbnail Manager",
            Tier::Thumbnail => "thumbnail tools must be first",
            Tier::High => "its page says to be high up",
            Tier::Normal => "keeps its place",
            Tier::Text => "text/lyrics mods go below visual mods",
            Tier::PinnedLast => "pinned to the bottom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pin {
    First,
    Last,
    None,
}

fn pins_path(game_dir: &Path) -> PathBuf {
    game_dir.join(DATA_DIR).join("sort.json")
}

pub fn load_pins(game_dir: &Path) -> Pins {
    fs::read(pins_path(game_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn pin_of(pins: &Pins, folder: &str) -> Pin {
    if pins.first.iter().any(|f| f == folder) {
        Pin::First
    } else if pins.last.iter().any(|f| f == folder) {
        Pin::Last
    } else {
        Pin::None
    }
}

/// Pin a mod to the top or bottom (or unpin it).
pub fn set_pin(game_dir: &Path, folder: &str, pin: Pin) -> Result<()> {
    let mut p = load_pins(game_dir);
    p.first.retain(|f| f != folder);
    p.last.retain(|f| f != folder);
    match pin {
        Pin::First => p.first.push(folder.to_owned()),
        Pin::Last => p.last.push(folder.to_owned()),
        Pin::None => {}
    }
    let path = pins_path(game_dir);
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(&p).expect("serialisable"))
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Known mods, recognised by their name or folder (letters and digits only,
/// lowercase).
mod known {
    pub const THUMB_SPLITTER: &[&str] = &["thumbnailsplitter"];
    pub const THUMB_MANAGER: &[&str] = &["thumbnailmanager", "thumbmanager"];
    /// "set this mod above the Thumbnail Manager in priority"
    /// (divamodarchive.com/post/402, /post/376, /post/290).
    pub const ABOVE_THUMB_MANAGER: &[&str] =
        &["lavversosongpack", "megpoidthemusic", "treeguy33songpack"];
    /// "FT UI should be high up on the priority list" (divamodarchive.com/post/25).
    pub const HIGH: &[&str] = &["futuretoneui"];
    pub const FT_UI: &[&str] = &["futuretoneui"];
    pub const CLSC: &[&str] = &["completeloadingscreencollection"];
    pub const X_UI: &[&str] = &["xuiformm"];
    pub const SANGABC: &[&str] = &["sangabcsmodulepack", "sangabcmodulepack"];
    pub const MEGPOID_MODULES: &[&str] = &["projectmegpoidmodulepack", "megpoidmodulepack"];
    pub const HOSHIKUZU: &[&str] = &["hoshikuzu"];
    pub const EDEN: &[&str] = &["edenproject", "edencore"];
    pub const NEW_CLASSICS: &[&str] = &["newclassics"];
    pub const AUTOPLAY: &[&str] = &["autoplay"];
    pub const SCORE_MM: &[&str] = &["scoremm"];
}

/// "A above B" (A wins). Sources: CLSC divamodarchive.com/post/144,
/// X UI /post/199, Sangabc's Module Pack /post/634.
const ABOVE: &[(&[&str], &[&str], &str)] = &[
    (
        known::CLSC,
        known::FT_UI,
        "its page says to go above Future Tone UI",
    ),
    (
        known::X_UI,
        known::FT_UI,
        "its page says to go above Future Tone UI",
    ),
    (
        known::SANGABC,
        known::MEGPOID_MODULES,
        "its page says to go above Project Megpoid Module Pack",
    ),
];

/// Pairs that don't work together, with what their pages say.
const INCOMPATIBLE: &[(&[&str], &[&str], &str)] = &[
    (
        known::THUMB_SPLITTER,
        known::THUMB_MANAGER,
        "ThumbnailSplitter and Thumbnail Manager are both installed; ThumbnailSplitter's page recommends using only one thumbnail mod (both want to be first). Consider turning one off.",
    ),
    (
        known::HOSHIKUZU,
        known::EDEN,
        "Project Hoshikuzu+ says it is not compatible with the Eden Project.",
    ),
    (
        known::NEW_CLASSICS,
        known::AUTOPLAY,
        "New Classics says it isn't compatible with Autoplay.",
    ),
    (
        known::NEW_CLASSICS,
        known::SCORE_MM,
        "New Classics says it isn't compatible with score-mm.",
    ),
];

/// Known incompatibilities between a mod (`name`, `folder`) and installed
/// mods (name, folder): the warning texts.
pub fn known_incompatibilities(
    name: &str,
    folder: &str,
    installed: &[(String, String)],
) -> Vec<String> {
    INCOMPATIBLE
        .iter()
        .filter(|(a, b, _)| {
            let has = |pats: &[&str]| installed.iter().any(|(n, f)| is(n, f, pats));
            (is(name, folder, a) && has(b)) || (is(name, folder, b) && has(a))
        })
        .map(|(_, _, w)| (*w).to_owned())
        .collect()
}

fn is(name: &str, folder: &str, pats: &[&str]) -> bool {
    let (n, f) = (norm(name), norm(folder));
    pats.iter().any(|p| n.contains(p) || f.contains(p))
}

/// Which group a mod belongs to, from its name and folder.
pub fn tier_of(name: &str, folder: &str, pins: &Pins) -> Tier {
    tier_in(name, folder, pins, false)
}

fn tier_in(name: &str, folder: &str, pins: &Pins, manager_installed: bool) -> Tier {
    match pin_of(pins, folder) {
        Pin::First => return Tier::PinnedFirst,
        Pin::Last => return Tier::PinnedLast,
        Pin::None => {}
    }
    let text = format!("{} {}", norm(name), norm(folder));
    if manager_installed && is(name, folder, known::ABOVE_THUMB_MANAGER) {
        return Tier::AboveThumbnailManager;
    }
    if is(name, folder, known::HIGH) {
        return Tier::High;
    }
    let thumb = text.contains("thumbnailmanager")
        || text.contains("thumbnailsplitter")
        || text.contains("thumbmanager")
        || (text.contains("thumbnail") && (text.contains("manager") || text.contains("splitter")));
    if thumb {
        return Tier::Thumbnail;
    }
    let textish = [
        "lyrics",
        "translation",
        "translated",
        "englishpatch",
        "engpatch",
        "subtitle",
        "romaji",
    ]
    .iter()
    .any(|w| text.contains(w));
    if textish {
        return Tier::Text;
    }
    Tier::Normal
}

#[derive(Debug, Clone, Serialize)]
pub struct Moved {
    pub folder: String,
    pub name: String,
    /// 1-based positions.
    pub from: usize,
    pub to: usize,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SortPlan {
    /// The new priority list.
    pub order: Vec<String>,
    /// Mods whose group put them somewhere new (not ones only shifted by
    /// others moving).
    pub moved: Vec<Moved>,
    pub changed: bool,
    /// Known problems with the installed mods (not fixed by sorting).
    pub warnings: Vec<String>,
}

/// Work out the sorted order without changing anything.
pub fn plan(game: &Game) -> Result<SortPlan> {
    let pins = load_pins(&game.dir);
    let current = game.ordered()?;
    let enabled = |pats: &[&str]| {
        current
            .iter()
            .any(|m| m.enabled() && is(m.name(), &m.folder, pats))
    };
    let manager = enabled(known::THUMB_MANAGER) && !enabled(known::THUMB_SPLITTER);
    let mut items: Vec<(Tier, usize, &crate::mods::Mod)> = current
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut t = tier_in(m.name(), &m.folder, &pins, manager);
            // A plugin that replaces no files (no `include`, e.g.
            // LyricsPatch) isn't a text mod for ordering purposes.
            let replaces_files = m
                .config
                .as_ref()
                .and_then(|c| c.include().ok().flatten())
                .is_some_and(|inc| !inc.is_empty());
            if t == Tier::Text && !replaces_files {
                t = Tier::Normal;
            }
            (t, i, *m)
        })
        .collect();
    // Pinned mods follow the pin list's order.
    let pin_index = |t: Tier, folder: &str| match t {
        Tier::PinnedFirst => pins.first.iter().position(|f| f == folder).unwrap_or(0),
        Tier::PinnedLast => pins.last.iter().position(|f| f == folder).unwrap_or(0),
        _ => 0,
    };
    items.sort_by_key(|(t, i, m)| (*t, pin_index(*t, &m.folder), *i));
    let mut reasons: std::collections::HashMap<String, &'static str> = items
        .iter()
        .filter(|(t, _, _)| *t != Tier::Normal)
        .map(|(t, _, m)| (m.folder.clone(), t.reason()))
        .collect();

    // "A above B" rules (never moving pinned mods).
    for (above, below, why) in ABOVE {
        let find = |pats: &[&str], items: &Vec<(Tier, usize, &crate::mods::Mod)>| {
            items
                .iter()
                .position(|(_, _, m)| is(m.name(), &m.folder, pats))
        };
        let (Some(a), Some(b)) = (find(above, &items), find(below, &items)) else {
            continue;
        };
        let pinned = |t: Tier| matches!(t, Tier::PinnedFirst | Tier::PinnedLast);
        if a > b && !pinned(items[a].0) && !pinned(items[b].0) {
            let it = items.remove(a);
            reasons.insert(it.2.folder.clone(), why);
            items.insert(b, it);
        }
    }

    let order: Vec<String> = items.iter().map(|(_, _, m)| m.folder.clone()).collect();
    let moved = items
        .iter()
        .enumerate()
        .filter(|(to, (_, from, _))| to != from)
        .filter_map(|(to, (_, from, m))| {
            reasons.get(&m.folder).map(|r| Moved {
                folder: m.folder.clone(),
                name: m.name().to_owned(),
                from: from + 1,
                to: to + 1,
                reason: r,
            })
        })
        .collect();
    let warnings = INCOMPATIBLE
        .iter()
        .filter(|(a, b, _)| enabled(a) && enabled(b))
        .map(|(_, _, w)| (*w).to_owned())
        .collect();
    let old: Vec<String> = game.priority()?;
    Ok(SortPlan {
        changed: order != old,
        order,
        moved,
        warnings,
    })
}

/// Apply a plan (with a backup).
pub fn apply(game: &mut Game, plan: &SortPlan) -> Result<SaveReport> {
    game.loader.set_priority(&plan.order);
    game.save("auto-sort the load order")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::tests::fake_game;

    fn named(n: &str) -> String {
        format!("enabled = true\ninclude = [\".\"]\nname = \"{n}\"\n")
    }

    #[test]
    fn groups() {
        let p = Pins::default();
        assert_eq!(
            tier_of("Thumbnail Manager", "MegaMixThumbnailManager", &p),
            Tier::Thumbnail
        );
        assert_eq!(tier_of("ThumbnailSplitter", "x", &p), Tier::Thumbnail);
        assert_eq!(tier_of("English Lyrics", "EnglishLyrics", &p), Tier::Text);
        // A lyrics *plugin* that replaces no files stays where it is.
        let t = fake_game(
            &["LyricsPatch", "Pack"],
            &[
                (
                    "LyricsPatch",
                    "dll = [\"LyricsPatch.dll\"]\nname = \"LyricsPatch\"\n",
                ),
                ("Pack", &named("Pack")),
            ],
        );
        assert!(!plan(&Game::open(t.path()).unwrap()).unwrap().changed);
        assert_eq!(
            tier_of("Project DIVA F2nd Song Pack", "F2nd", &p),
            Tier::Normal
        );
        assert_eq!(tier_of("Thumbnails for my pack", "pack", &p), Tier::Normal);
    }

    #[test]
    fn rules_from_mod_pages() {
        let t = fake_game(
            &[
                "Pack", "FTUI", "XUI", "Megpoid", "Sangabc", "TM", "lavverso", "NC", "Auto",
            ],
            &[
                ("Pack", &named("Some Song Pack")),
                ("FTUI", &named("Future Tone UI")),
                ("XUI", &named("X UI for MM+")),
                ("Megpoid", &named("Project Megpoid Module Pack")),
                ("Sangabc", &named("Sangabc's Module Pack")),
                ("TM", &named("Mega Mix Thumbnail Manager")),
                ("lavverso", &named("lavverso Song Pack")),
                ("NC", &named("New Classics")),
                ("Auto", &named("Autoplay")),
            ],
        );
        let g = Game::open(t.path()).unwrap();
        let p = plan(&g).unwrap();
        assert_eq!(
            p.order,
            [
                "lavverso", "TM", "XUI", "FTUI", "Pack", "Sangabc", "Megpoid", "NC", "Auto"
            ]
        );
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(p.warnings[0].contains("Autoplay"));

        // With ThumbnailSplitter as well: warn, and the song pack no longer
        // needs to go above the Thumbnail Manager.
        let t = fake_game(
            &["TM", "lavverso", "Split"],
            &[
                ("TM", &named("Mega Mix Thumbnail Manager")),
                ("lavverso", &named("lavverso Song Pack")),
                ("Split", &named("!  ThumbnailSplitter")),
            ],
        );
        let p = plan(&Game::open(t.path()).unwrap()).unwrap();
        assert_eq!(p.order, ["TM", "Split", "lavverso"]);
        assert!(p.warnings[0].contains("only one"));
    }

    #[test]
    fn sorts_keeping_manual_order_and_pins() {
        let t = fake_game(
            &["Lyrics", "PackA", "Thumbs", "PackB", "Megpoid"],
            &[
                ("Lyrics", &named("English Lyrics")),
                ("PackA", &named("Pack A")),
                ("Thumbs", &named("Thumbnail Manager")),
                ("PackB", &named("Pack B")),
                ("Megpoid", &named("Project Megpoid Module Pack")),
                ("Zed", &named("Not in the list")),
            ],
        );
        let mut g = Game::open(t.path()).unwrap();
        let p = plan(&g).unwrap();
        assert_eq!(
            p.order,
            ["Thumbs", "PackA", "PackB", "Megpoid", "Zed", "Lyrics"]
        );
        let moved: Vec<_> = p
            .moved
            .iter()
            .map(|m| (m.folder.as_str(), m.from, m.to))
            .collect();
        assert_eq!(moved, [("Thumbs", 3, 1), ("Lyrics", 1, 6)]);
        assert!(p.changed);

        // Megpoid's page says to put it above the Thumbnail Manager: a pin.
        set_pin(t.path(), "Megpoid", Pin::First).unwrap();
        let p = plan(&g).unwrap();
        assert_eq!(p.order[..2], ["Megpoid", "Thumbs"]);

        let r = apply(&mut g, &p).unwrap();
        assert!(r.backup.is_some());
        assert_eq!(g.priority().unwrap(), p.order);
        assert!(!plan(&g).unwrap().changed, "sorting twice changes nothing");

        set_pin(t.path(), "Megpoid", Pin::None).unwrap();
        assert_eq!(load_pins(t.path()), Pins::default());
    }
}
