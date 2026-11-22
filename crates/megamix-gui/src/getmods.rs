//! "Get mods" page: search DivaModArchive + GameBanana, details with
//! pictures, and download → install.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};

use megamix_core::dma;
use megamix_core::get;
use megamix_core::install;
use megamix_core::net::Client;
use megamix_core::records;
use megamix_core::remote::{
    self, ModKind, ModRef, RemoteFile, RemoteMod, SearchOptions, Site, SortBy, Sources,
};

use crate::{
    AppWindow, FileChoice, Origin, Pending, RemoteRow, ReqRow, Shared, human_size, run_install,
    status, with_state,
};

/// Largest description shown (long ones make the text layout slow).
const MAX_DESC: usize = 8000;

enum ImageTarget {
    Thumb,
    /// Screenshot `n` of the selected mod.
    Detail(usize),
}

struct ImageJob {
    url: String,
    generation: u64,
    key: String,
    target: ImageTarget,
}

pub struct GetPage {
    pub client: Client,
    results: Vec<RemoteMod>,
    model: Rc<VecModel<RemoteRow>>,
    /// Bumped on every new search; stale answers and image jobs are dropped.
    generation: Arc<AtomicU64>,
    page: usize,
    opts: SearchOptions,
    selected: Option<usize>,
    image_idx: usize,
    thumbs: Sender<ImageJob>,
    details: Sender<ImageJob>,
    searched: bool,
}

impl GetPage {
    pub fn new(ui: &AppWindow) -> Self {
        let client = Client::new();
        {
            let c = client.clone();
            std::thread::spawn(move || c.clean_cache());
        }
        let generation = Arc::new(AtomicU64::new(0));
        let (thumbs, trx) = channel();
        let (details, drx) = channel();
        let trx = Arc::new(Mutex::new(trx));
        for _ in 0..4 {
            spawn_image_worker(
                client.clone(),
                trx.clone(),
                generation.clone(),
                ui.as_weak(),
            );
        }
        spawn_image_worker(
            client.clone(),
            Arc::new(Mutex::new(drx)),
            generation.clone(),
            ui.as_weak(),
        );
        let model = Rc::new(VecModel::default());
        ui.set_get_results(ModelRc::from(model.clone()));
        GetPage {
            client,
            results: Vec::new(),
            model,
            generation,
            page: 0,
            opts: SearchOptions::default(),
            selected: None,
            image_idx: 0,
            thumbs,
            details,
            searched: false,
        }
    }
}

fn spawn_image_worker(
    client: Client,
    rx: Arc<Mutex<Receiver<ImageJob>>>,
    generation: Arc<AtomicU64>,
    weak: Weak<AppWindow>,
) {
    std::thread::spawn(move || {
        loop {
            let job = {
                let Ok(rx) = rx.lock() else { return };
                match rx.recv() {
                    Ok(j) => j,
                    Err(_) => return,
                }
            };
            if job.generation != generation.load(Ordering::Relaxed) {
                continue;
            }
            let Some(buf) = load_image(&client, &job) else {
                continue;
            };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                with_state(|st| apply_image(&ui, st, job, buf));
            });
        }
    });
}

fn load_image(client: &Client, job: &ImageJob) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let bytes = client.get_image(&job.url).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let img = match job.target {
        ImageTarget::Thumb => img.thumbnail(256, 144),
        ImageTarget::Detail(_) if img.width() > 1280 || img.height() > 720 => {
            img.resize(1280, 720, image::imageops::FilterType::Triangle)
        }
        ImageTarget::Detail(_) => img,
    };
    let rgba = img.to_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

fn apply_image(ui: &AppWindow, st: &Shared, job: ImageJob, buf: SharedPixelBuffer<Rgba8Pixel>) {
    let s = st.borrow();
    let g = &s.get;
    if job.generation != g.generation.load(Ordering::Relaxed) {
        return;
    }
    match job.target {
        ImageTarget::Thumb => {
            if let Some(i) = g.model.iter().position(|r| r.key.as_str() == job.key) {
                let mut row = g.model.row_data(i).expect("row");
                row.thumb = slint::Image::from_rgba8(buf);
                row.has_thumb = true;
                g.model.set_row_data(i, row);
            }
        }
        ImageTarget::Detail(n) => {
            let sel_key = g.selected.and_then(|i| g.results.get(i)).map(key);
            if sel_key.as_deref() == Some(job.key.as_str()) && g.image_idx == n {
                ui.set_det_image(slint::Image::from_rgba8(buf));
                ui.set_det_has_image(true);
            }
        }
    }
}

fn key(m: &RemoteMod) -> String {
    m.mod_ref().to_string()
}

fn date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

fn compact(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 10_000 => format!("{}k", n / 1000),
        n if n >= 1000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// Folder and update state of mods installed from the sites.
struct InstalledInfo {
    by_ref: HashMap<(Site, u64), (String, records::Record)>,
}

impl InstalledInfo {
    fn load(st: &Shared) -> Self {
        let s = st.borrow();
        let mut by_ref = HashMap::new();
        if let Some(game) = &s.game {
            let mods_dir = game.mods_dir().ok();
            for (folder, rec) in records::load(&game.dir) {
                if mods_dir.as_ref().is_some_and(|d| !d.join(&folder).is_dir()) {
                    continue;
                }
                if let Some(r) = get::installed_ref(&rec) {
                    by_ref.insert((r.site, r.id), (folder, rec));
                }
            }
        }
        InstalledInfo { by_ref }
    }

    /// ("" | "Installed" | "Update", folder)
    fn state(&self, m: &RemoteMod) -> (&'static str, Option<&str>) {
        match self.by_ref.get(&(m.site, m.id)) {
            // Installed from the other site's copy of the same mod.
            None => match m.also_on.and_then(|r| self.by_ref.get(&(r.site, r.id))) {
                Some((folder, _)) => ("Installed", Some(folder.as_str())),
                None => ("", None),
            },
            Some((folder, rec)) => {
                let label = if get::update_for(folder, rec, m).is_some() {
                    "Update"
                } else {
                    "Installed"
                };
                (label, Some(folder.as_str()))
            }
        }
    }
}

fn to_row(m: &RemoteMod, inst: &InstalledInfo) -> RemoteRow {
    let mut meta = Vec::new();
    meta.push(if m.category.is_empty() {
        m.kind.label().to_owned()
    } else {
        m.category.clone()
    });
    if !m.authors.is_empty() {
        meta.push(format!("by {}", m.authors.join(", ")));
    }
    let mut stats = vec![format!("updated {}", date(m.updated))];
    if let Some(d) = m.downloads {
        stats.push(format!("{} downloads", compact(d)));
    }
    stats.push(format!("{} likes", compact(m.likes)));
    if m.also_on.is_some() {
        stats.push(format!("also on {}", m.site.other().name()));
    }
    RemoteRow {
        key: key(m).into(),
        name: m.name.as_str().into(),
        meta: meta.join(" · ").into(),
        stats: stats.join(" · ").into(),
        site: match m.site {
            Site::Dma => "DMA",
            Site::GameBanana => "GB",
        }
        .into(),
        thumb: Default::default(),
        has_thumb: false,
        installed: inst.state(m).0.into(),
        explicit: m.explicit,
    }
}

fn options_from_ui(ui: &AppWindow, prefer: Option<Site>) -> SearchOptions {
    SearchOptions {
        query: ui.get_get_query().trim().to_owned(),
        sources: match ui.get_get_source() {
            1 => Sources::Dma,
            2 => Sources::GameBanana,
            _ => Sources::All,
        },
        kind: ModKind::ALL
            .get(ui.get_get_kind().max(0) as usize)
            .copied()
            .unwrap_or_default(),
        sort: match ui.get_get_sort() {
            1 => SortBy::Newest,
            2 => SortBy::Updated,
            3 => SortBy::Downloads,
            4 => SortBy::Likes,
            _ => SortBy::Relevance,
        },
        page: 0,
        prefer,
    }
}

/// Page opened: search once, and refresh the installed badges.
pub fn on_page_opened(ui: &AppWindow, st: &Shared) {
    if !st.borrow().get.searched {
        search(ui, st, false);
    } else {
        refresh_badges(ui, st);
    }
}

pub fn refresh_badges(ui: &AppWindow, st: &Shared) {
    let inst = InstalledInfo::load(st);
    let s = st.borrow();
    for (i, m) in s.get.results.iter().enumerate() {
        if let Some(mut row) = s.get.model.row_data(i) {
            let label: slint::SharedString = inst.state(m).0.into();
            if row.installed != label {
                row.installed = label;
                s.get.model.set_row_data(i, row);
            }
        }
    }
    drop(s);
    show_selected(ui, st);
}

/// New search (`more` = next page of the same search).
pub fn search(ui: &AppWindow, st: &Shared, more: bool) {
    // A pasted link or id opens that mod.
    if !more && let Some(r) = remote::parse_ref(&ui.get_get_query()) {
        open_ref(ui, st, r, false);
        return;
    }
    let (client, opts, generation) = {
        let mut s = st.borrow_mut();
        let prefer = s.settings.prefer_site;
        let g = &mut s.get;
        g.searched = true;
        if more {
            g.page += 1;
        } else {
            g.generation.fetch_add(1, Ordering::Relaxed);
            g.page = 0;
            g.opts = options_from_ui(ui, prefer);
            g.results.clear();
            g.model.set_vec(Vec::new());
            g.selected = None;
        }
        let mut o = g.opts.clone();
        o.page = g.page;
        (g.client.clone(), o, g.generation.load(Ordering::Relaxed))
    };
    if !more {
        ui.set_get_selected(-1);
        ui.set_det_visible(false);
        ui.set_get_message("".into());
    }
    ui.set_get_loading(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = remote::search(&client, &opts);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| {
                if st.borrow().get.generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                ui.set_get_loading(false);
                let inst = InstalledInfo::load(st);
                let first_new;
                {
                    let mut s = st.borrow_mut();
                    let g = &mut s.get;
                    first_new = g.results.len();
                    for m in res.mods {
                        let k = key(&m);
                        if g.results.iter().any(|x| key(x) == k) {
                            continue;
                        }
                        let row = to_row(&m, &inst);
                        if let Some(url) = &m.thumbnail {
                            let _ = g.thumbs.send(ImageJob {
                                url: url.clone(),
                                generation,
                                key: k,
                                target: ImageTarget::Thumb,
                            });
                        }
                        g.results.push(m);
                        g.model.push(row);
                    }
                    ui.set_get_more(res.more);
                }
                let total = st.borrow().get.results.len();
                let mut msg = Vec::new();
                for (site, e) in &res.errors {
                    msg.push(format!("{} couldn't be searched: {e}", site.name()));
                }
                if res.stale {
                    msg.push(
                        "A site couldn't be reached, so some results are older saved ones.".into(),
                    );
                }
                if total == 0 && res.errors.is_empty() {
                    msg.push("No mods found.".into());
                }
                ui.set_get_message(msg.join("\n").into());
                if first_new == 0 && total > 0 {
                    ui.set_get_selected(0);
                    select(&ui, st, 0);
                }
            });
        });
    });
}

/// Show mod `i` in the details panel (loading full details if needed).
pub fn select(ui: &AppWindow, st: &Shared, i: usize) {
    let (need_details, need_notices, r, client, generation) = {
        let mut s = st.borrow_mut();
        let g = &mut s.get;
        let Some(m) = g.results.get(i) else { return };
        let same = g.selected == Some(i);
        g.selected = Some(i);
        if !same {
            g.image_idx = 0;
        }
        (
            !m.details_loaded,
            m.site == Site::Dma && m.notices.is_none(),
            m.mod_ref(),
            g.client.clone(),
            g.generation.load(Ordering::Relaxed),
        )
    };
    show_selected(ui, st);
    request_detail_image(ui, st);
    if need_details || need_notices {
        ui.set_det_loading(need_details);
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let res = need_details.then(|| remote::details(&client, r, false));
            // The site's notices (needs ExPatch, conflicts). Not fatal.
            let notices = need_notices
                .then(|| dma::page_notices(&client, r.id).ok())
                .flatten();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                with_state(|st| {
                    {
                        let mut s = st.borrow_mut();
                        let g = &mut s.get;
                        if g.generation.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let Some(idx) = g.results.iter().position(|m| m.mod_ref() == r) else {
                            return;
                        };
                        match res {
                            None => {}
                            Some(Ok(f)) => {
                                let also = g.results[idx].also_on;
                                let mut m = f.value;
                                m.also_on = also;
                                m.notices = g.results[idx].notices.take();
                                g.results[idx] = m;
                            }
                            Some(Err(e)) => {
                                drop(s);
                                ui.set_det_loading(false);
                                status(&ui, &format!("Couldn't load the mod's details: {e}"), true);
                                return;
                            }
                        }
                        if notices.is_some() {
                            g.results[idx].notices = notices;
                        }
                    }
                    if st
                        .borrow()
                        .get
                        .selected
                        .and_then(|i| st.borrow().get.results.get(i).map(|m| m.mod_ref()))
                        == Some(r)
                    {
                        show_selected(&ui, st);
                        request_detail_image(&ui, st);
                    }
                    refresh_badges(&ui, st);
                });
            });
        });
    }
}

fn show_selected(ui: &AppWindow, st: &Shared) {
    let inst = InstalledInfo::load(st);
    let s = st.borrow();
    let g = &s.get;
    let Some(m) = g.selected.and_then(|i| g.results.get(i)) else {
        ui.set_det_visible(false);
        return;
    };
    ui.set_det_visible(true);
    ui.set_det_loading(!m.details_loaded);
    ui.set_det_name(m.name.as_str().into());
    let mut meta = vec![if m.category.is_empty() {
        m.kind.label().to_owned()
    } else {
        m.category.clone()
    }];
    if !m.authors.is_empty() {
        meta.push(format!("by {}", m.authors.join(", ")));
    }
    meta.push(format!("on {}", m.site.name()));
    ui.set_det_meta(meta.join(" · ").into());
    let mut stats = vec![format!("Updated {}", date(m.updated))];
    if let Some(v) = &m.version {
        stats.push(format!("version {v}"));
    }
    if let Some(d) = m.downloads {
        stats.push(format!("{} downloads", compact(d)));
    }
    stats.push(format!("{} likes", compact(m.likes)));
    if m.explicit {
        stats.push("marked 18+ / has content ratings".into());
    }
    ui.set_det_stats(stats.join(" · ").into());
    let mut desc: String = m.description.trim().chars().take(MAX_DESC).collect();
    if m.description.trim().chars().count() > MAX_DESC {
        desc.push_str("…\n\n(The full description is on the mod page.)");
    }
    ui.set_det_desc(desc.into());
    ui.set_det_also(
        m.also_on
            .map(|r| format!("Also on {}", r.site.name()))
            .unwrap_or_default()
            .into(),
    );
    ui.set_det_site(m.site.name().into());
    ui.set_det_installed(
        match inst.state(m) {
            ("Update", Some(f)) => format!("Update available · installed as “{f}”"),
            (_, Some(f)) => format!("Installed as “{f}”"),
            _ => String::new(),
        }
        .into(),
    );
    let mut files: Vec<FileChoice> = m
        .files
        .iter()
        .map(|f| FileChoice {
            name: f.name.as_str().into(),
            detail: format!(
                "{}{}",
                if f.size > 0 {
                    human_size(f.size)
                } else {
                    "size unknown".into()
                },
                if f.added > 0 {
                    format!(" · {}", date(f.added))
                } else {
                    String::new()
                }
            )
            .into(),
            action: "".into(),
        })
        .collect();
    // Downloads hosted elsewhere (GameBanana alternate sources).
    for l in &m.external {
        let in_app = remote::parse_ref(&l.url).is_some();
        files.push(FileChoice {
            name: if l.description.is_empty() {
                l.host().into()
            } else {
                l.description.as_str().into()
            },
            detail: if l.direct.is_some() {
                format!("{} · downloaded directly", l.host())
            } else if in_app {
                format!("{} · opens here", l.host())
            } else {
                format!(
                    "{} · opens in your browser, then use Install from file",
                    l.host()
                )
            }
            .into(),
            action: if l.direct.is_some() {
                ""
            } else if in_app {
                "Show"
            } else {
                "Open"
            }
            .into(),
        });
    }
    ui.set_det_files(ModelRc::new(VecModel::from(files)));
    let c = s
        .game
        .as_ref()
        .map(|game| megamix_core::compat::check(game, m))
        .unwrap_or_default();
    ui.set_det_requires(ModelRc::new(VecModel::from(
        c.requirements
            .iter()
            .map(|q| ReqRow {
                name: q.name.as_str().into(),
                note: q.note.as_str().into(),
                ok: q.installed.is_some(),
                optional: q.optional,
                web: q.r.is_none(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_det_incompat(ModelRc::new(VecModel::from(
        c.incompatible
            .iter()
            .map(|w| slint::SharedString::from(w.as_str()))
            .collect::<Vec<_>>(),
    )));
    let count = if m.images.is_empty() {
        m.thumbnail.is_some() as usize
    } else {
        m.images.len()
    };
    ui.set_det_image_count(count as i32);
    ui.set_det_image_index(g.image_idx as i32);
}

fn request_detail_image(ui: &AppWindow, st: &Shared) {
    let s = st.borrow();
    let g = &s.get;
    let Some(m) = g.selected.and_then(|i| g.results.get(i)) else {
        return;
    };
    ui.set_det_has_image(false);
    let url = m
        .images
        .get(g.image_idx)
        .or(m.thumbnail.as_ref().filter(|_| g.image_idx == 0));
    if let Some(url) = url {
        let _ = g.details.send(ImageJob {
            url: url.clone(),
            generation: g.generation.load(Ordering::Relaxed),
            key: key(m),
            target: ImageTarget::Detail(g.image_idx),
        });
    }
}

/// Previous/next screenshot.
pub fn step_image(ui: &AppWindow, st: &Shared, dir: i32) {
    {
        let mut s = st.borrow_mut();
        let g = &mut s.get;
        let Some(m) = g.selected.and_then(|i| g.results.get(i)) else {
            return;
        };
        let n = m.images.len();
        if n < 2 {
            return;
        }
        g.image_idx = (g.image_idx as i64 + dir as i64).rem_euclid(n as i64) as usize;
        ui.set_det_image_index(g.image_idx as i32);
    }
    request_detail_image(ui, st);
}

pub fn open_page(ui: &AppWindow, st: &Shared) {
    let url = {
        let s = st.borrow();
        let g = &s.get;
        g.selected
            .and_then(|i| g.results.get(i))
            .map(|m| m.page_url.clone())
    };
    if let Some(url) = url
        && let Err(e) = megamix_core::launch::open_web_page(&url)
    {
        status(ui, &format!("Couldn't open {url}: {e}"), true);
    }
}

/// Show the other site's copy of the selected mod.
pub fn open_also(ui: &AppWindow, st: &Shared) {
    let r = {
        let s = st.borrow();
        let g = &s.get;
        g.selected
            .and_then(|i| g.results.get(i))
            .and_then(|m| m.also_on)
    };
    if let Some(r) = r {
        open_ref(ui, st, r, false);
    }
}

/// Look up one mod (from a link or id) and show it at the top of the list.
/// `install`: then ask to install it (one-click links).
pub fn open_ref(ui: &AppWindow, st: &Shared, r: ModRef, install: bool) {
    let (client, generation) = {
        let mut s = st.borrow_mut();
        s.get.searched = true;
        (
            s.get.client.clone(),
            s.get.generation.load(Ordering::Relaxed),
        )
    };
    ui.set_get_loading(true);
    status(ui, &format!("Looking up {r}…"), false);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = remote::details(&client, r, true).map(|mut f| {
            if r.site == Site::Dma {
                f.value.notices = dma::page_notices(&client, r.id).ok();
            }
            f
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| {
                ui.set_get_loading(false);
                let m = match res {
                    Ok(f) => f.value,
                    Err(e) => {
                        status(&ui, &format!("Couldn't open {r}: {e}"), true);
                        return;
                    }
                };
                status(&ui, "", false);
                let inst = InstalledInfo::load(st);
                {
                    let mut s = st.borrow_mut();
                    let g = &mut s.get;
                    let _ = generation; // a link replaces nothing; it's added on top
                    if let Some(old) = g.results.iter().position(|x| x.mod_ref() == m.mod_ref()) {
                        g.results.remove(old);
                        g.model.remove(old);
                    }
                    if let Some(url) = &m.thumbnail {
                        let _ = g.thumbs.send(ImageJob {
                            url: url.clone(),
                            generation: g.generation.load(Ordering::Relaxed),
                            key: key(&m),
                            target: ImageTarget::Thumb,
                        });
                    }
                    g.model.insert(0, to_row(&m, &inst));
                    g.results.insert(0, m.clone());
                    g.selected = None;
                }
                ui.set_get_selected(0);
                select(&ui, st, 0);
                if install {
                    ask_install(&ui, st, &m, r);
                }
            });
        });
    });
}

/// One-click link: confirm, or let the user choose a file.
fn ask_install(ui: &AppWindow, st: &Shared, m: &RemoteMod, r: ModRef) {
    match get::pick_file(m, &r) {
        Ok(Some(f)) => {
            let from = if f.id == remote::EXTERNAL_FILE_ID {
                crate::net_host(&f.url)
            } else {
                m.site.name().to_owned()
            };
            let body = format!(
                "{} ({}) from {}{}.\n\nIt's downloaded to the game's drive, checked, and unpacked into the mods folder. The load order is backed up first.",
                f.name,
                if f.size > 0 {
                    human_size(f.size)
                } else {
                    "size unknown".into()
                },
                from,
                if m.authors.is_empty() {
                    String::new()
                } else {
                    format!(", by {}", m.authors.join(", "))
                }
            );
            let verb = match InstalledInfo::load(st).state(m).0 {
                "Update" => "Update",
                "Installed" => "Reinstall",
                _ => "Install",
            };
            crate::confirm(
                ui,
                st,
                Pending::Download(Box::new(m.clone()), f),
                &format!("{verb} “{}”?", m.name),
                &body,
                verb,
            );
            ui.set_confirm_warning(compat_warning(st, m).into());
        }
        Ok(None) => {
            ui.set_files_index(0);
            ui.set_files_visible(true);
        }
        Err(e) => status(ui, &e.to_string(), true),
    }
}

/// Install button for file `idx` of the selected mod.
pub fn install_file(ui: &AppWindow, st: &Shared, idx: usize) {
    let Some(m) = ({
        let s = st.borrow();
        let g = &s.get;
        g.selected.and_then(|i| g.results.get(i)).cloned()
    }) else {
        return;
    };
    // Missing requirements or problems with installed mods: ask first.
    let file = m.files.get(idx).cloned().or_else(|| {
        m.external
            .get(idx.wrapping_sub(m.files.len()))
            .and_then(|l| l.as_file(&m))
    });
    let warning = compat_warning(st, &m);
    if let Some(f) = file
        && !warning.is_empty()
    {
        crate::confirm(
            ui,
            st,
            Pending::Download(Box::new(m.clone()), f),
            &format!("Install “{}” anyway?", m.name),
            "You can install it now and add the missing mods afterwards.",
            "Install anyway",
        );
        ui.set_confirm_warning(warning.into());
        return;
    }
    if let Some(f) = m.files.get(idx) {
        start_download(ui, st, m.clone(), f.clone(), None);
        return;
    }
    let Some(link) = m.external.get(idx - m.files.len()).cloned() else {
        return;
    };
    if let Some(f) = link.as_file(&m) {
        start_download(ui, st, m, f, None);
    } else if let Some(r) = remote::parse_ref(&link.url) {
        open_ref(ui, st, r, false);
    } else {
        match megamix_core::launch::open_web_page(&link.url) {
            Ok(()) => status(
                ui,
                &format!(
                    "Opened {} in your browser. When the download is done, install it from “Install from file”.",
                    link.host()
                ),
                false,
            ),
            Err(e) => status(ui, &format!("Couldn't open {}: {e}", link.url), true),
        }
    }
}

/// Download in the background with progress, then look inside and install
/// (asking first if it would replace a mod from somewhere else).
/// `update_of`: the installed folder this download updates.
pub fn start_download(
    ui: &AppWindow,
    st: &Shared,
    m: RemoteMod,
    file: RemoteFile,
    update_of: Option<String>,
) {
    let Some(mods_dir) = st.borrow().game.as_ref().and_then(|g| g.mods_dir().ok()) else {
        status(ui, "Choose the game folder first (Settings).", true);
        return;
    };
    if ui.get_install_running() {
        return;
    }
    let client = st.borrow().get.client.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    st.borrow_mut().install_cancel = Some(cancel.clone());
    ui.set_install_title(format!("Downloading {}", m.name).into());
    let from = if file.id == remote::EXTERNAL_FILE_ID {
        crate::net_host(&file.url)
    } else {
        m.site.name().to_owned()
    };
    ui.set_install_detail(format!("{} from {from}", file.name).into());
    ui.set_install_progress(0.0);
    ui.set_install_running(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let w2 = weak.clone();
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let res = get::download_file(
            &client,
            &mods_dir,
            &m,
            &file,
            &mut |p| {
                let now = std::time::Instant::now();
                if p.retry.is_none()
                    && now.duration_since(last) < std::time::Duration::from_millis(100)
                {
                    return;
                }
                last = now;
                let frac = if p.total > 0 {
                    p.done as f32 / p.total as f32
                } else {
                    0.0
                };
                let detail = match &p.retry {
                    Some((n, why)) => format!("Connection problem, trying again ({n}/6): {why}"),
                    None => format!(
                        "{} of {}{}",
                        human_size(p.done),
                        if p.total > 0 {
                            human_size(p.total)
                        } else {
                            "?".into()
                        },
                        if p.speed > 0 {
                            format!(" · {}/s", human_size(p.speed))
                        } else {
                            String::new()
                        }
                    ),
                };
                let _ = w2.upgrade_in_event_loop(move |ui| {
                    ui.set_install_progress(frac);
                    ui.set_install_detail(detail.into());
                });
            },
            &cancel,
        )
        .and_then(|dl| {
            let plan = install::plan(&mods_dir, &dl.path)?;
            Ok((dl, plan))
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| after_download(&ui, st, m, file, res, update_of));
        });
    });
}

fn after_download(
    ui: &AppWindow,
    st: &Shared,
    m: RemoteMod,
    file: RemoteFile,
    res: megamix_core::Result<(megamix_core::download::Download, install::Plan)>,
    update_of: Option<String>,
) {
    ui.set_install_running(false);
    st.borrow_mut().install_cancel = None;
    let (download, mut plan) = match res {
        Ok(x) => x,
        Err(megamix_core::Error::Cancelled) => {
            status(ui, "Download cancelled. Nothing was changed.", false);
            crate::updates::after(ui, st, true);
            return;
        }
        Err(e) => {
            status(ui, &format!("Download failed: {e}"), true);
            crate::updates::after(ui, st, false);
            return;
        }
    };
    if let Some(folder) = &update_of {
        // An update goes into the mod's current folder.
        get::retarget_update(&mut plan, folder);
        let origin = Origin::Remote {
            m: Box::new(m),
            file,
            download,
        };
        run_install(ui, st, plan, origin, install::IfExists::Replace);
        return;
    }
    let Some(game_dir) = st.borrow().game.as_ref().map(|g| g.dir.clone()) else {
        return;
    };
    let foreign = get::foreign_conflicts(&game_dir, &plan, &m);
    let name = m.name.clone();
    let origin = Origin::Remote {
        m: Box::new(m),
        file,
        download,
    };
    if foreign.is_empty() {
        // New, or an update of a mod installed from this same page.
        run_install(ui, st, plan, origin, install::IfExists::Replace);
        return;
    }
    let body = format!(
        "A mod folder with the same name is already installed but didn't come from this page:\n{}\n\nUpdate replaces it (keeping its on/off state and settings). Keep both installs “{name}” next to it.",
        foreign
            .iter()
            .map(|f| format!("• {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    st.borrow_mut().pending = Some(Pending::Install(plan, origin));
    ui.set_confirm_title(format!("“{name}” is already installed").into());
    ui.set_confirm_body(body.into());
    ui.set_confirm_accept("Update".into());
    ui.set_confirm_alt("Keep both".into());
    ui.set_confirm_visible(true);
}

/// Red warning text for a mod: missing requirements and incompatibilities.
fn compat_warning(st: &Shared, m: &RemoteMod) -> String {
    let s = st.borrow();
    let Some(game) = s.game.as_ref() else {
        return String::new();
    };
    let c = megamix_core::compat::check(game, m);
    let mut lines = Vec::new();
    for q in c.missing_required() {
        lines.push(format!(
            "✗ Requires {}{} — not installed.",
            q.name,
            if q.note.is_empty() {
                String::new()
            } else {
                format!(" ({})", q.note)
            }
        ));
    }
    for w in &c.incompatible {
        lines.push(format!("⚠ {w}"));
    }
    lines.join("\n")
}

/// "Show" on a missing requirement: open its page here (or in the
/// browser when it isn't on a mod site, like ExPatch).
pub fn open_requirement(ui: &AppWindow, st: &Shared, i: usize) {
    let q = {
        let s = st.borrow();
        let g = &s.get;
        let (Some(game), Some(m)) = (s.game.as_ref(), g.selected.and_then(|k| g.results.get(k)))
        else {
            return;
        };
        megamix_core::compat::check(game, m)
            .requirements
            .into_iter()
            .nth(i)
    };
    let Some(q) = q else { return };
    if let Some(r) = q.r {
        open_ref(ui, st, r, false);
    } else if let Some(url) = q.url
        && let Err(e) = megamix_core::launch::open_web_page(&url)
    {
        status(ui, &format!("Couldn't open {url}: {e}"), true);
    }
}
