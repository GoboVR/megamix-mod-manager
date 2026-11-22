//! Update checks and updates for mods installed from (or linked to)
//! DivaModArchive / GameBanana, on the Mods page.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;

use slint::ComponentHandle;

use megamix_core::get::{self, Update};
use megamix_core::records;
use megamix_core::remote::{self, Site};

use crate::{AppWindow, Shared, getmods, refresh_mods, status, with_state};

#[derive(Default)]
pub struct Updates {
    /// Folder → available update.
    pub found: HashMap<String, Update>,
    /// Updates still to run after the current one ("Update all").
    pub queue: VecDeque<Update>,
    pub checking: bool,
}

/// Check every linked mod in the background. `quiet`: only report when
/// something was found (the automatic check after startup).
pub fn check(ui: &AppWindow, st: &Shared, quiet: bool) {
    let (client, game_dir) = {
        let mut s = st.borrow_mut();
        if s.upd.checking {
            return;
        }
        let Some(dir) = s.game.as_ref().map(|g| g.dir.clone()) else {
            return;
        };
        s.upd.checking = true;
        (s.get.client.clone(), dir)
    };
    ui.set_checking_updates(true);
    if !quiet {
        status(ui, "Checking for updates…", false);
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let c = get::check_updates(&client, &game_dir, None, &AtomicBool::new(false));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| {
                ui.set_checking_updates(false);
                {
                    let mut s = st.borrow_mut();
                    s.upd.checking = false;
                    s.upd.found = c.updates.iter().map(|u| (u.folder.clone(), u.clone())).collect();
                }
                ui.set_update_count(c.updates.len() as i32);
                refresh_mods(&ui, &st.borrow(), None);
                let n = c.updates.len();
                let msg = match (n, c.checked) {
                    (_, 0) if !quiet => Some("No mods are linked to DivaModArchive or GameBanana yet. Install from Get mods, or link a mod to its page.".to_owned()),
                    (0, _) if !quiet => Some(format!("All {} linked mods are up to date.", c.checked)),
                    (0, _) => None,
                    (1, _) => Some("1 mod has an update.".to_owned()),
                    (n, _) => Some(format!("{n} mods have updates.")),
                };
                let errors = if c.errors.is_empty() || quiet {
                    String::new()
                } else {
                    format!(" Couldn't check: {}.", c.errors.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", "))
                };
                if let Some(m) = msg {
                    status(&ui, &format!("{m}{errors}"), !errors.is_empty());
                }
            });
        });
    });
}

/// Update one mod.
pub fn update_one(ui: &AppWindow, st: &Shared, folder: &str) {
    let u = st.borrow().upd.found.get(folder).cloned();
    match u {
        Some(u) => {
            st.borrow_mut().upd.queue = VecDeque::from([u]);
            next(ui, st);
        }
        None => status(
            ui,
            &format!("No update known for {folder}. Try “Check for updates”."),
            false,
        ),
    }
}

/// Update every mod that has an update (one after another).
pub fn update_all(ui: &AppWindow, st: &Shared) {
    let mut all: Vec<Update> = st.borrow().upd.found.values().cloned().collect();
    all.sort_by(|a, b| a.folder.cmp(&b.folder));
    if all.is_empty() {
        status(ui, "No updates to install.", false);
        return;
    }
    st.borrow_mut().upd.queue = all.into();
    next(ui, st);
}

/// Start the next queued update.
pub fn next(ui: &AppWindow, st: &Shared) {
    let Some(u) = st.borrow_mut().upd.queue.pop_front() else {
        return;
    };
    let left = st.borrow().upd.queue.len();
    let client = st.borrow().get.client.clone();
    ui.set_install_title(format!("Updating {}", u.folder).into());
    ui.set_install_detail("Looking up the mod page…".into());
    ui.set_install_progress(0.0);
    ui.set_install_running(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = remote::details(&client, u.remote, true);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| {
                ui.set_install_running(false);
                let m = match res {
                    Ok(f) => f.value,
                    Err(e) => {
                        status(&ui, &format!("Couldn't update {}: {e}", u.folder), true);
                        after(&ui, st, false);
                        return;
                    }
                };
                // Recompute against the fresh page.
                let recs = records::load(
                    &st.borrow()
                        .game
                        .as_ref()
                        .map(|g| g.dir.clone())
                        .unwrap_or_default(),
                );
                let fresh = recs
                    .get(&u.folder)
                    .and_then(|r| get::update_for(&u.folder, r, &m));
                match fresh.and_then(|f| f.file) {
                    Some(file) => {
                        if left > 0 {
                            status(
                                &ui,
                                &format!("{left} more update(s) after this one."),
                                false,
                            );
                        }
                        getmods::start_download(&ui, st, m, file, Some(u.folder.clone()));
                    }
                    None if left == 0 => {
                        // Several files: let the user pick on the Get mods page.
                        status(
                            &ui,
                            &format!(
                                "“{}” has several downloads; choose the one to install.",
                                m.name
                            ),
                            false,
                        );
                        ui.set_page(7);
                        getmods::open_ref(&ui, st, u.remote, true);
                    }
                    None => {
                        status(
                            &ui,
                            &format!(
                                "Skipped {}: its page has several downloads, update it on its own.",
                                u.folder
                            ),
                            false,
                        );
                        after(&ui, st, false);
                    }
                }
            });
        });
    });
}

/// After an update finished (or failed): continue the queue, or stop it if
/// the user cancelled.
pub fn after(ui: &AppWindow, st: &Shared, cancelled: bool) {
    if cancelled {
        st.borrow_mut().upd.queue.clear();
        return;
    }
    if !st.borrow().upd.queue.is_empty() {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|st| next(&ui, st));
            }
        });
    }
}

/// A mod was (re)installed: it's current now.
pub fn installed(ui: &AppWindow, st: &Shared, folders: &[String]) {
    let mut s = st.borrow_mut();
    for f in folders {
        s.upd.found.remove(f);
    }
    ui.set_update_count(s.upd.found.len() as i32);
}

/// "Linked to …" text and short site label for a mod row.
pub fn link_info(rec: Option<&records::Record>) -> (String, String) {
    let Some(r) = rec.and_then(get::installed_ref) else {
        return (String::new(), String::new());
    };
    let short = match r.site {
        Site::Dma => "DMA",
        Site::GameBanana => "GB",
    };
    (short.into(), format!("From {} ({r})", r.site.name()))
}

/// Link a hand-installed mod to a page (pasted link or id).
pub fn link(ui: &AppWindow, st: &Shared, folder: String, text: String) {
    let Some(r) = remote::parse_ref(&text) else {
        status(
            ui,
            &megamix_core::Error::BadModRef(text.trim().to_owned()).to_string(),
            true,
        );
        return;
    };
    let (client, game_dir) = {
        let s = st.borrow();
        let Some(d) = s.game.as_ref().map(|g| g.dir.clone()) else {
            return;
        };
        (s.get.client.clone(), d)
    };
    status(ui, &format!("Looking up {r}…"), false);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = remote::details(&client, r, true).and_then(|f| {
            let file = r
                .file
                .and_then(|id| f.value.files.iter().find(|x| x.id == id).cloned());
            get::link_mod(&game_dir, &folder, &f.value, file.as_ref()).map(|_| f.value)
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| {
                match res {
                    Ok(m) => {
                        ui.set_link_input("".into());
                        status(&ui, &format!("Linked {folder} to “{}” on {}. You'll be told when it has an update.", m.name, m.site.name()), false);
                    }
                    Err(e) => status(&ui, &format!("Couldn't link {folder}: {e}"), true),
                }
                refresh_mods(&ui, &st.borrow(), Some(&folder));
            });
        });
    });
}

pub fn unlink(ui: &AppWindow, st: &Shared, folder: &str) {
    let dir = st.borrow().game.as_ref().map(|g| g.dir.clone());
    let Some(dir) = dir else { return };
    match records::remove(&dir, folder) {
        Ok(_) => {
            installed(ui, st, &[folder.to_owned()]);
            status(
                ui,
                &format!("{folder} is no longer linked to a mod page."),
                false,
            );
        }
        Err(e) => status(ui, &e.to_string(), true),
    }
    refresh_mods(ui, &st.borrow(), Some(folder));
}

/// Open the page a mod is linked to.
pub fn open_page(ui: &AppWindow, st: &Shared, folder: &str) {
    let dir = st.borrow().game.as_ref().map(|g| g.dir.clone());
    let Some(dir) = dir else { return };
    let recs = records::load(&dir);
    let Some(r) = recs.get(folder).and_then(get::installed_ref) else {
        return;
    };
    let url = match r.site {
        Site::Dma => megamix_core::dma::page_url(r.id),
        Site::GameBanana => megamix_core::gamebanana::page_url(r.id),
    };
    if let Err(e) = megamix_core::launch::open_web_page(&url) {
        status(ui, &format!("Couldn't open {url}: {e}"), true);
    }
}
