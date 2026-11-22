//! `search`, `info`, installing `dma:`/`gb:` mods, `updates` and `update`.

use std::io::IsTerminal;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use serde_json::json;

use megamix_core::get::{self, Step};
use megamix_core::install::IfExists;
use megamix_core::mods::{Game, NewModPlacement};
use megamix_core::net::Client;
use megamix_core::records;
use megamix_core::remote::{
    self, ModKind, ModRef, RemoteFile, RemoteMod, SearchOptions, SortBy, Sources,
};
use megamix_core::settings::Settings;

use crate::{KindArg, SortArg, SourceArg, human_size, print_json};

pub enum FilePick {
    /// The file the reference names, or the only file.
    Default,
    Id(u64),
    /// External link number (1-based, from `megamix info`).
    Link(usize),
    All,
}

/// DivaModArchive: the notices on the post's web page (not fatal).
fn add_notices(client: &Client, m: &mut RemoteMod) {
    if m.site == remote::Site::Dma {
        m.notices = megamix_core::dma::page_notices(client, m.id).ok();
    }
}

fn date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "?".into())
}

fn compact(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 10_000 => format!("{}k", n / 1000),
        n if n >= 1000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

pub fn search(
    query: &str,
    source: SourceArg,
    sort: SortArg,
    kind: KindArg,
    page: usize,
    json: bool,
) -> Result<()> {
    let o = SearchOptions {
        query: query.to_owned(),
        sources: match source {
            SourceArg::All => Sources::All,
            SourceArg::Dma => Sources::Dma,
            SourceArg::Gb => Sources::GameBanana,
        },
        sort: match sort {
            SortArg::Relevance => SortBy::Relevance,
            SortArg::Newest => SortBy::Newest,
            SortArg::Updated => SortBy::Updated,
            SortArg::Downloads => SortBy::Downloads,
            SortArg::Likes => SortBy::Likes,
        },
        kind: match kind {
            KindArg::Any => ModKind::Any,
            KindArg::Song => ModKind::Song,
            KindArg::Cover => ModKind::Cover,
            KindArg::Module => ModKind::Module,
            KindArg::Ui => ModKind::Ui,
            KindArg::Plugin => ModKind::Plugin,
            KindArg::Other => ModKind::Other,
        },
        page: page.saturating_sub(1),
        prefer: Settings::load().prefer_site,
    };
    let client = Client::new();
    let res = remote::search(&client, &o);
    if json {
        print_json(&json!({
            "mods": res.mods,
            "more": res.more,
            "stale": res.stale,
            "errors": res.errors.iter().map(|(s, e)| json!({ "site": s, "error": e })).collect::<Vec<_>>(),
        }));
    } else {
        for (site, e) in &res.errors {
            eprintln!("warning: {} search failed: {e}", site.name());
        }
        if res.stale {
            eprintln!("note: a site couldn't be reached; showing older saved results");
        }
        if res.mods.is_empty() {
            println!("No mods found.");
        }
        for m in &res.mods {
            let also = m
                .also_on
                .map(|r| format!("  (also {r})"))
                .unwrap_or_default();
            println!(
                "{:<13} {}  [{}] by {}{also}",
                m.mod_ref().to_string(),
                m.name,
                if m.category.is_empty() {
                    m.kind.label()
                } else {
                    &m.category
                },
                if m.authors.is_empty() {
                    "?".into()
                } else {
                    m.authors.join(", ")
                },
            );
            println!(
                "              updated {}{} · {} likes",
                date(m.updated),
                m.downloads
                    .map(|d| format!(" · {} downloads", compact(d)))
                    .unwrap_or_default(),
                compact(m.likes)
            );
        }
        if res.more {
            println!("More results: --page {}", page + 1);
        }
    }
    if res.mods.is_empty() && !res.errors.is_empty() {
        bail!("search failed");
    }
    Ok(())
}

fn parse(s: &str) -> Result<ModRef> {
    remote::parse_ref(s).ok_or_else(|| megamix_core::Error::BadModRef(s.to_owned()).into())
}

pub fn info(s: &str, json: bool) -> Result<()> {
    let r = parse(s)?;
    let client = Client::new();
    let f = remote::details(&client, r, false).with_context(|| format!("looking up {r}"))?;
    let mut m = f.value;
    add_notices(&client, &mut m);
    if json {
        print_json(&m);
        return Ok(());
    }
    if f.stale {
        eprintln!(
            "note: {} couldn't be reached; showing older saved details",
            r.site.name()
        );
    }
    println!("{}  ({})", m.name, m.mod_ref());
    println!("  {}", m.page_url);
    if !m.authors.is_empty() {
        println!("  by {}", m.authors.join(", "));
    }
    println!(
        "  {} · updated {}{} · {} likes{}",
        if m.category.is_empty() {
            m.kind.label()
        } else {
            &m.category
        },
        date(m.updated),
        m.downloads
            .map(|d| format!(" · {} downloads", compact(d)))
            .unwrap_or_default(),
        compact(m.likes),
        m.version
            .as_deref()
            .map(|v| format!(" · version {v}"))
            .unwrap_or_default()
    );
    if m.explicit {
        println!("  marked explicit / has content ratings");
    }
    if !m.files.is_empty() {
        println!("  Files:");
    }
    for file in &m.files {
        println!(
            "    --file {:<9} {}  {}  {}",
            file.id,
            file.name,
            human_size(file.size),
            date(file.added)
        );
    }
    if !m.external.is_empty() {
        println!("  Hosted elsewhere:");
        for (i, l) in m.external.iter().enumerate() {
            let how = if l.direct.is_some() {
                format!("--link {}", i + 1)
            } else {
                "browser only".into()
            };
            println!(
                "    {how:<12} {}  {}",
                if l.description.is_empty() {
                    l.host()
                } else {
                    &l.description
                },
                l.url
            );
        }
    }
    if !m.requires.is_empty() {
        println!("  Requires:");
        for q in &m.requires {
            println!(
                "    {}  ({}){}",
                q.name,
                q.r,
                if q.note.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", q.note)
                }
            );
        }
    }
    if let Some(n) = &m.notices {
        for need in &n.needs {
            let tag = match need.kind {
                megamix_core::dma::NeedKind::Required => "Needs",
                megamix_core::dma::NeedKind::Incompatible => "Doesn't work with",
                megamix_core::dma::NeedKind::Info => "Note",
            };
            println!("  {tag} {}: {}", need.name, need.text);
        }
        for sure in [true, false] {
            let rows: Vec<_> = n.conflicts.iter().filter(|c| c.sure == sure).collect();
            if rows.is_empty() {
                continue;
            }
            println!(
                "  {} (from the site):",
                if sure {
                    "Conflicts with"
                } else {
                    "May conflict with"
                }
            );
            for c in rows.iter().take(15) {
                println!(
                    "    {} {:<6} {}  ←  {}{}",
                    c.section,
                    c.id,
                    c.name,
                    c.source,
                    c.post.map(|p| format!(" (dma:{p})")).unwrap_or_default()
                );
            }
            if rows.len() > 15 {
                println!("    … and {} more", rows.len() - 15);
            }
        }
    }
    let text = m.description.trim();
    if !text.is_empty() {
        println!();
        for line in text.lines().take(30) {
            println!("  {line}");
        }
        if text.lines().count() > 30 {
            println!("  … (more on the mod page)");
        }
    }
    Ok(())
}

fn files_to_install(m: &RemoteMod, r: &ModRef, pick: &FilePick) -> Result<Vec<RemoteFile>> {
    Ok(match pick {
        FilePick::All => m.files.clone(),
        FilePick::Id(id) => vec![
            m.files
                .iter()
                .find(|f| f.id == *id)
                .cloned()
                .with_context(|| format!("{r} has no file {id} (see `megamix info {r}`)"))?,
        ],
        FilePick::Link(n) => {
            let l = m.external.get(n.wrapping_sub(1)).with_context(|| {
                format!("{r} has no external link {n} (see `megamix info {r}`)")
            })?;
            vec![l.as_file(m).with_context(|| {
                format!(
                    "{} can't be downloaded automatically; open {} in a browser",
                    l.host(),
                    l.url
                )
            })?]
        }
        FilePick::Default => match get::pick_file(m, r)? {
            Some(f) => vec![f],
            None => {
                let mut list: Vec<String> = m
                    .files
                    .iter()
                    .map(|f| format!("  --file {}  {} ({})", f.id, f.name, human_size(f.size)))
                    .collect();
                for (i, l) in m.external.iter().enumerate() {
                    if l.direct.is_some() {
                        list.push(format!(
                            "  --link {}  {} ({})",
                            i + 1,
                            l.description,
                            l.host()
                        ));
                    }
                }
                bail!(
                    "“{}” has several downloads; choose one:\n{}",
                    m.name,
                    list.join("\n")
                );
            }
        },
    })
}

/// Download and install one mod reference. Returns JSON results.
pub fn install_ref(
    game: &mut Game,
    r: ModRef,
    pick: FilePick,
    if_exists: IfExists,
    placement: NewModPlacement,
    cancel: &AtomicBool,
    json: bool,
) -> Result<Vec<serde_json::Value>> {
    let client = Client::new();
    let mut m = remote::details(&client, r, true)
        .with_context(|| format!("looking up {r}"))?
        .value;
    add_notices(&client, &mut m);
    let files = files_to_install(&m, &r, &pick)?;
    let c = megamix_core::compat::check(game, &m);
    for q in c.missing_required() {
        eprintln!(
            "warning: {} needs {}, which isn't installed{}{}",
            m.name,
            q.name,
            if q.note.is_empty() {
                String::new()
            } else {
                format!(" ({})", q.note)
            },
            q.r.map(|r| format!(" — megamix install {r}"))
                .or(q.url.as_ref().map(|u| format!(" — {u}")))
                .unwrap_or_default()
        );
    }
    for w in &c.incompatible {
        eprintln!("warning: {w}");
    }
    let mut out = Vec::new();
    for file in &files {
        if !json {
            println!(
                "{}: downloading {} ({}) from {}",
                m.name,
                file.name,
                human_size(file.size),
                m.site.name()
            );
        }
        let res = install_one(
            &client, game, &m, file, if_exists, placement, None, cancel, json,
        )?;
        out.push(json!({ "mod": r.to_string(), "file": file.name, "installed": res.installed, "backup": res.backup }));
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn install_one(
    client: &Client,
    game: &mut Game,
    m: &RemoteMod,
    file: &RemoteFile,
    if_exists: IfExists,
    placement: NewModPlacement,
    update_folder: Option<&str>,
    cancel: &AtomicBool,
    json: bool,
) -> Result<get::RemoteInstalled> {
    let tty = std::io::stderr().is_terminal();
    let mut last = String::new();
    let mut shown = false;
    let res = get::install_remote(
        client,
        game,
        m,
        file,
        if_exists,
        placement,
        update_folder,
        &mut |s| {
            if json || !tty {
                return;
            }
            let line = match s {
                Step::Downloading(p) => match &p.retry {
                    Some((n, why)) => format!("  connection problem, retry {n}: {why}"),
                    None => {
                        let pct = (p.done * 100).checked_div(p.total).unwrap_or(0);
                        format!(
                            "  downloading {pct:>3}%  {} / {}  {}/s",
                            human_size(p.done),
                            human_size(p.total),
                            human_size(p.speed)
                        )
                    }
                },
                Step::Installing(megamix_core::install::Step::Unpacking(p)) => {
                    let pct = (p.done * 100).checked_div(p.total).unwrap_or(0);
                    format!(
                        "  unpacking {pct:>3}%  {} / {}",
                        human_size(p.done),
                        human_size(p.total)
                    )
                }
                Step::Installing(_) => "  installing…".into(),
            };
            if line != last {
                eprint!("\r{line:<70}");
                last = line;
                shown = true;
            }
        },
        cancel,
    );
    if shown {
        eprintln!();
    }
    let res = res.with_context(|| format!("installing {} from {}", file.name, m.site.name()))?;
    if !json {
        if res.installed.is_empty() {
            println!("  skipped (already installed)");
        }
        for i in &res.installed {
            let what = if i.replaced { "Updated" } else { "Installed" };
            let kept = if i.kept_settings.is_empty() {
                String::new()
            } else {
                format!(" (kept: {})", i.kept_settings.join(", "))
            };
            println!("  {what} {}{kept}", i.folder);
        }
        if let Some(b) = &res.backup {
            println!(
                "  Load order saved. Undo the load-order change with `megamix undo` (backup {b})."
            );
        }
    }
    Ok(res)
}

pub fn updates(game: &Game, json: bool) -> Result<()> {
    let client = Client::new();
    if !json && std::io::stderr().is_terminal() {
        eprintln!("Checking…");
    }
    let c = get::check_updates(&client, &game.dir, None, &AtomicBool::new(false));
    if json {
        print_json(&c);
        return Ok(());
    }
    for (f, e) in &c.errors {
        eprintln!("warning: couldn't check {f}: {e}");
    }
    if c.checked == 0 {
        println!(
            "No mods installed from DivaModArchive or GameBanana yet (only those can be checked)."
        );
    } else if c.updates.is_empty() {
        println!(
            "All {} mod(s) from DivaModArchive / GameBanana are up to date.",
            c.checked
        );
    }
    for u in &c.updates {
        let ver = match (&u.installed_version, &u.new_version) {
            (Some(a), Some(b)) if a != b => format!("  {a} → {b}"),
            _ => String::new(),
        };
        let choose = if u.file.is_none() {
            "  (several files: choose with `megamix install`)"
        } else {
            ""
        };
        println!(
            "{}  ({}) updated {}{ver}{choose}",
            u.folder,
            u.remote,
            date(u.updated)
        );
    }
    if !c.updates.is_empty() {
        println!("Update with `megamix update <mod>` or `megamix update --all`.");
    }
    Ok(())
}

pub fn update(game: &mut Game, mods: &[String], cancel: &AtomicBool, json: bool) -> Result<()> {
    let only: Option<Vec<String>> = if mods.is_empty() {
        None
    } else {
        let mut v = Vec::new();
        for m in mods {
            let i = game.find(m)?;
            v.push(game.mods[i].folder.clone());
        }
        let recs = records::load(&game.dir);
        for f in &v {
            if !recs.get(f).is_some_and(|r| get::installed_ref(r).is_some()) {
                bail!(
                    "{f} wasn't installed from DivaModArchive or GameBanana, so it can't be updated here"
                );
            }
        }
        Some(v)
    };
    let client = Client::new();
    let c = get::check_updates(&client, &game.dir, only.as_deref(), cancel);
    for (f, e) in &c.errors {
        eprintln!("warning: couldn't check {f}: {e}");
    }
    let mut results = Vec::new();
    let mut skipped = Vec::new();
    for u in &c.updates {
        let Some(file) = &u.file else {
            skipped.push(u.folder.clone());
            continue;
        };
        let m = remote::details(&client, u.remote, true)?.value;
        if !json {
            println!(
                "{}: downloading {} ({})",
                u.folder,
                file.name,
                human_size(file.size)
            );
        }
        let res = install_one(
            &client,
            game,
            &m,
            file,
            IfExists::Replace,
            NewModPlacement::Top,
            Some(&u.folder),
            cancel,
            json,
        )?;
        results.push(json!({ "folder": u.folder, "file": file.name, "installed": res.installed, "backup": res.backup }));
    }
    if json {
        print_json(&json!({ "updated": results, "need_choice": skipped, "errors": c.errors }));
    } else {
        if c.updates.is_empty() {
            println!("Everything is up to date.");
        }
        for f in &skipped {
            println!(
                "{f}: the mod page has several files; pick one with `megamix install <mod> --file <id>`."
            );
        }
    }
    Ok(())
}

pub fn link(game: &Game, m: &str, page: &str, file: Option<u64>, json: bool) -> Result<()> {
    let folder = game.mods[game.find(m)?].folder.clone();
    let r = parse(page)?;
    let client = Client::new();
    let remote = remote::details(&client, r, true)
        .with_context(|| format!("looking up {r}"))?
        .value;
    let file = match file.or(r.file) {
        Some(id) => Some(
            remote
                .files
                .iter()
                .find(|f| f.id == id)
                .cloned()
                .with_context(|| format!("{r} has no file {id}"))?,
        ),
        None => None,
    };
    get::link_mod(&game.dir, &folder, &remote, file.as_ref())?;
    if json {
        print_json(&json!({ "folder": folder, "linked": r.to_string() }));
    } else {
        println!(
            "{folder} is now linked to “{}” ({r}). It will be offered updates from there.",
            remote.name
        );
    }
    Ok(())
}
