//! Updating the app itself (see `megamix_core::selfupdate`).

use slint::ComponentHandle;

use megamix_core::selfupdate::{self, AppRelease, Method};

use crate::{AppWindow, Pending, Shared, confirm, status, with_state};

pub fn init(ui: &AppWindow, st: &Shared) {
    ui.set_app_version(selfupdate::CURRENT.into());
    let on = st.borrow().settings.app_updates.unwrap_or(true);
    ui.set_app_updates_on(on);
    // For testing the screens: pretend this version is out.
    if let Ok(v) = std::env::var("MEGAMIX_FAKE_APP_UPDATE") {
        ui.set_app_update_version(v.as_str().into());
        ui.set_app_update_note(format!("Version {v} is available.").into());
        st.borrow_mut().app_release = Some(AppRelease {
            version: v.clone(),
            title: format!("Version {v}"),
            notes: "- Example change one\n- Example change two".into(),
            page_url: selfupdate::releases_page(),
            asset: None,
            sums_url: None,
        });
        return;
    }
    if on {
        // A little after start, so it doesn't slow the window down.
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(5), move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|st| check(&ui, st, false));
            }
        });
    }
}

/// Look for a new version. `by_hand`: the user pressed "Check now" (skips
/// the cache and reports the result).
pub fn check(ui: &AppWindow, st: &Shared, by_hand: bool) {
    let _ = st;
    if by_hand {
        ui.set_app_update_note("Checking…".into());
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let client = megamix_core::net::Client::new();
        let res = selfupdate::check(&client, by_hand);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with_state(|st| match res {
                Ok(Some(rel)) => {
                    ui.set_app_update_version(rel.version.as_str().into());
                    ui.set_app_update_note(format!("Version {} is available.", rel.version).into());
                    st.borrow_mut().app_release = Some(rel);
                }
                Ok(None) => {
                    ui.set_app_update_version("".into());
                    if by_hand {
                        ui.set_app_update_note("You have the newest version.".into());
                    }
                }
                Err(e) => {
                    if by_hand {
                        ui.set_app_update_note(format!("Couldn't check: {e}").into());
                    }
                }
            });
        });
    });
}

pub fn toggled(ui: &AppWindow, st: &Shared, on: bool) {
    {
        let mut s = st.borrow_mut();
        s.settings.app_updates = Some(on);
        if let Err(e) = s.settings.save() {
            status(ui, &format!("Couldn't save settings: {e}"), true);
        }
    }
    ui.set_app_update_note("".into());
    if on {
        check(ui, st, false);
    }
}

fn short_notes(notes: &str) -> String {
    let lines: Vec<&str> = notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("**Full Changelog**"))
        .take(8)
        .collect();
    let mut s = lines.join("\n");
    if s.chars().count() > 600 {
        s = s.chars().take(600).collect::<String>() + "…";
    }
    s
}

pub fn clicked(ui: &AppWindow, st: &Shared) {
    let Some(rel) = st.borrow().app_release.clone() else {
        return;
    };
    let how = match selfupdate::method() {
        Method::Manual => {
            // Can't update this copy (portable, Flatpak…): show the page.
            if let Err(e) = megamix_core::launch::open_web_page(&rel.page_url) {
                status(ui, &format!("Couldn't open {}: {e}", rel.page_url), true);
            } else {
                status(
                    ui,
                    "Download the new version from the page that just opened.",
                    false,
                );
            }
            return;
        }
        Method::InPlace => {
            "It's downloaded from GitHub, checked, and replaces this app's files. Then the app restarts."
        }
        Method::Installer => {
            "The installer is downloaded from GitHub, checked, and runs on its own. The app closes and opens again when it's done."
        }
    };
    if rel.asset.is_none() {
        let _ = megamix_core::launch::open_web_page(&rel.page_url);
        status(
            ui,
            "This release has no download for this PC; its page was opened.",
            true,
        );
        return;
    }
    let notes = short_notes(&rel.notes);
    let body = if notes.is_empty() {
        format!("You have {}. {how}", selfupdate::CURRENT)
    } else {
        format!(
            "You have {}. {how}\n\nWhat's new:\n{notes}",
            selfupdate::CURRENT
        )
    };
    confirm(
        ui,
        st,
        Pending::AppUpdate(Box::new(rel.clone())),
        &format!("Update to {}?", rel.version),
        &body,
        "Update",
    );
}

pub fn run(ui: &AppWindow, st: &Shared, rel: AppRelease) {
    let _ = st;
    ui.set_busy(true);
    status(ui, "Downloading the update…", false);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let client = megamix_core::net::Client::new();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let w = weak.clone();
        let mut last = 101;
        let res = selfupdate::install(
            &client,
            &rel,
            &mut |p| {
                if p.total > 0 && p.done * 100 / p.total != last {
                    last = p.done * 100 / p.total;
                    let msg = format!("Downloading the update… {last}%");
                    let _ = w.upgrade_in_event_loop(move |ui| status(&ui, &msg, false));
                }
            },
            &cancel,
        );
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy(false);
            match res {
                Ok(u) if u.installer_started => {
                    // The installer needs this app closed to replace it.
                    crate::links::stop_listening();
                    let _ = slint::quit_event_loop();
                }
                Ok(_) => {
                    with_state(|st| {
                        confirm(
                            &ui,
                            st,
                            Pending::Restart,
                            &format!("Updated to {}", rel.version),
                            "Restart the app now to use the new version?",
                            "Restart",
                        );
                    });
                }
                Err(e) => status(&ui, &format!("Update failed: {e}"), true),
            }
        });
    });
}

pub fn restart(ui: &AppWindow) {
    // Stop answering as "the running copy" first, or the new one would
    // hand over to this one and quit.
    crate::links::stop_listening();
    match selfupdate::relaunch_gui() {
        Ok(()) => {
            let _ = slint::quit_event_loop();
        }
        Err(e) => status(ui, &format!("{e}. Close and open the app yourself."), true),
    }
}
