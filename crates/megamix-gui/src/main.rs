//! Mega Mix+ Mod Manager GUI (Slint).

// No console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod appupdate;
mod gamepad;
mod getmods;
mod links;
mod updates;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::{Model, ModelRc, SharedString, VecModel};

use gamepad::{InputClock, Pad, SharedClock};

use megamix_core::backup;
use megamix_core::conflicts;
use megamix_core::doctor::{self, Level};
use megamix_core::install;
use megamix_core::loadout;
use megamix_core::mods::{Game, ModInfo, Move, NewModPlacement};
use megamix_core::settings::Settings;
use megamix_core::steam;

slint::include_modules!();

/// Where an archive being installed came from.
enum Origin {
    File(PathBuf),
    /// Downloaded; the download is deleted when this is dropped.
    Remote {
        m: Box<megamix_core::remote::RemoteMod>,
        file: megamix_core::remote::RemoteFile,
        // Only held so the file lives until the install is done.
        #[allow(dead_code)]
        download: megamix_core::download::Download,
    },
}

impl Origin {
    fn title(&self) -> String {
        match self {
            Origin::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Origin::Remote { m, .. } => m.name.clone(),
        }
    }
}

/// An action waiting for the user to confirm it in the dialog.
enum Pending {
    Install(install::Plan, Origin),
    /// Remove this mod folder.
    Remove(String),
    /// Apply this sorted load order.
    Sort(megamix_core::sort::SortPlan),
    /// Download and install this file (one-click link).
    Download(
        Box<megamix_core::remote::RemoteMod>,
        megamix_core::remote::RemoteFile,
    ),
    ApplyLoadout(String),
    DeleteLoadout(String),
    RestoreBackup(String),
    /// Add (true) or take out (false) the ARM fix.
    ArmFix(bool),
    /// Update the app itself.
    AppUpdate(Box<megamix_core::selfupdate::AppRelease>),
    /// Restart after an update.
    Restart,
}

struct State {
    game: Option<Game>,
    settings: Settings,
    filter: String,
    /// `--game <dir>` / `MEGAMIX_GAME`: used for this run only, not saved.
    override_dir: Option<PathBuf>,
    pending: Option<Pending>,
    mods: Rc<VecModel<ModRow>>,
    browse_dir: PathBuf,
    install_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    get: getmods::GetPage,
    upd: updates::Updates,
    /// A newer version of the app, if one was found.
    app_release: Option<megamix_core::selfupdate::AppRelease>,
}

type Shared = Rc<RefCell<State>>;

thread_local! {
    /// The app state, for code running from `upgrade_in_event_loop`.
    static APP: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

/// Run `f` with the app state (UI thread only).
fn with_state(f: impl FnOnce(&Shared)) {
    let st = APP.with(|a| a.borrow().clone());
    if let Some(st) = st {
        f(&st);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    if std::env::args().any(|a| a == "--list-gamepads") {
        match gamepad::list() {
            Ok(l) if l.is_empty() => println!("No controllers found."),
            Ok(l) => l.iter().for_each(|n| println!("{n}")),
            Err(e) => println!("Controller support unavailable: {e}"),
        }
        return Ok(());
    }
    if std::env::args().any(|a| a == "--register-links") {
        match links::register() {
            Ok(()) => println!("One-click install links now open this program."),
            Err(e) => {
                eprintln!("Couldn't set up one-click links: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // A one-click link or mod reference on the command line. If the app is
    // already open, hand it over and quit.
    let link = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .find(|a| megamix_core::remote::parse_ref(a).is_some());
    if let Some(l) = &link
        && links::forward(l)
    {
        return Ok(());
    }
    let settings = Settings::load();
    if let Some(scale) = settings.ui_scale
        && std::env::var_os("SLINT_SCALE_FACTOR").is_none()
    {
        // SAFETY: no other threads exist yet.
        unsafe { std::env::set_var("SLINT_SCALE_FACTOR", scale.to_string()) };
    }

    let game_mode = in_game_mode();
    let theme = match settings.theme.as_deref() {
        Some("dark") => 1,
        Some("light") => 2,
        // Game Mode has no system theme; Slint would fall back to light.
        _ if game_mode => 1,
        _ => 0,
    };
    let ui = AppWindow::new()?;
    ui.set_theme(theme);
    let mods = Rc::new(VecModel::<ModRow>::default());
    ui.set_mods(ModelRc::from(mods.clone()));

    let state: Shared = Rc::new(RefCell::new(State {
        game: None,
        settings,
        filter: String::new(),
        override_dir: arg_value("--game")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("MEGAMIX_GAME").map(PathBuf::from)),
        pending: None,
        mods,
        browse_dir: PathBuf::new(),
        install_cancel: None,
        get: getmods::GetPage::new(&ui),
        upd: Default::default(),
        app_release: None,
    }));
    APP.with(|a| *a.borrow_mut() = Some(state.clone()));
    {
        let mut st = state.borrow_mut();
        st.browse_dir = st
            .settings
            .last_install_dir
            .clone()
            .filter(|d| d.is_dir())
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
    }

    let fullscreen = std::env::args().any(|a| a == "--fullscreen") || game_mode;
    if fullscreen {
        ui.window().set_fullscreen(true);
    }

    connect(&ui, &state);
    open_game(&ui, &state);
    if let Some(mods) = state.borrow().game.as_ref().and_then(|g| g.mods_dir().ok()) {
        megamix_core::download::cleanup_leftovers(&mods.join(megamix_core::download::DOWNLOAD_DIR));
        std::thread::spawn(move || megamix_core::remove::purge_old(&mods));
    }
    if let Some(p) = arg_value("--page") {
        let n = [
            "mods",
            "conflicts",
            "loadouts",
            "backups",
            "doctor",
            "settings",
            "install",
            "get",
        ]
        .iter()
        .position(|x| x.eq_ignore_ascii_case(&p));
        if let Some(n) = n {
            ui.set_page(n as i32);
            refresh_page(&ui, &state, n as i32);
        }
    }
    if let Some(l) = link {
        open_link(&ui, l);
    }
    links::listen(ui.as_weak(), open_link);
    appupdate::init(&ui, &state);
    // Look for updates a few seconds after start, in the background.
    if ui.get_game_ok() {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|st| updates::check(&ui, st, true));
            }
        });
    }
    ui.set_prefer_site(match state.borrow().settings.prefer_site {
        Some(megamix_core::remote::Site::GameBanana) => 1,
        _ => 0,
    });
    let clock: SharedClock = Default::default();
    {
        let clock = clock.clone();
        ui.on_key_allowed(move || InputClock::key_allowed(&clock));
    }
    if std::env::var_os("MEGAMIX_NO_GAMEPAD").is_none() {
        let weak = ui.as_weak();
        gamepad::spawn(clock, move |p| {
            let _ = weak.upgrade_in_event_loop(move |ui| handle_pad(&ui, p));
        });
    }
    if let Some(script) = arg_value("--pad-script") {
        run_pad_script(&ui, script);
    }
    // `--size 1100x580`: window size in logical pixels (testing layouts).
    if let Some((w, h)) = arg_value("--size").and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
    }) {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }
    if let Some(out) = arg_value("--screenshot") {
        schedule_screenshot(&ui, PathBuf::from(out));
    }
    if doctor::game_running() {
        status(
            &ui,
            "The game is running. Changes take effect the next time it starts.",
            true,
        );
    }
    // Run until explicitly quit: the window is hidden (not closed) while the
    // game runs, and that must not end the program.
    ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    ui.show()?;
    let r = slint::run_event_loop_until_quit();
    links::stop_listening();
    // Drop a download still waiting in a dialog (deletes it).
    state.borrow_mut().pending = None;
    r
}

/// A one-click link (command line or handed over by a later start).
fn open_link(ui: &AppWindow, link: String) {
    let Some(r) = megamix_core::remote::parse_ref(&link) else {
        return;
    };
    let _ = ui.show();
    if !ui.get_game_ok() {
        status(
            ui,
            "Set the game folder first, then open the link again.",
            true,
        );
        return;
    }
    if ui.get_install_running() {
        status(
            ui,
            "Finish or cancel the current install, then open the link again.",
            true,
        );
        return;
    }
    ui.set_page(7);
    with_state(|st| getmods::open_ref(ui, st, r, true));
}

/// Running inside Steam Game Mode (gamescope session)?
fn in_game_mode() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0");
    set("GAMESCOPE_WAYLAND_DISPLAY")
        || set("SteamGamepadUI")
        || set("ENABLE_GAMESCOPE_WSI")
        || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.eq_ignore_ascii_case("gamescope"))
}

// ---- installing ---------------------------------------------------------

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Fill the install page with the current folder's subfolders and archives.
fn list_browse_dir(ui: &AppWindow, state: &Shared) {
    let dir = state.borrow().browse_dir.clone();
    let mut rows = Vec::new();
    if let Some(parent) = dir.parent() {
        rows.push(BrowseRow {
            name: "..".into(),
            kind: 0,
            size: SharedString::new(),
            path: parent.display().to_string().into(),
        });
    }
    let mut folders = Vec::new();
    let mut files = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                if path.is_dir() {
                    folders.push((name, path));
                } else if megamix_core::archive::has_archive_extension(&path) {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push((name, path, size));
                }
            }
            ui.set_browse_message(if files.is_empty() {
                "No mod files (.zip, .7z, .rar) in this folder.".into()
            } else {
                SharedString::new()
            });
        }
        Err(e) => ui.set_browse_message(format!("Can't open this folder: {e}").into()),
    }
    folders.sort_by_key(|(n, _)| n.to_lowercase());
    files.sort_by_key(|(n, _, _)| n.to_lowercase());
    // Archives first: that's what people come here for.
    for (name, path, size) in files {
        rows.push(BrowseRow {
            name: name.into(),
            kind: 2,
            size: human_size(size).into(),
            path: path.display().to_string().into(),
        });
    }
    for (name, path) in folders {
        rows.push(BrowseRow {
            name: name.into(),
            kind: 1,
            size: SharedString::new(),
            path: path.display().to_string().into(),
        });
    }
    ui.set_browse_dir(dir.display().to_string().into());
    ui.set_browse_entries(ModelRc::new(VecModel::from(rows)));
    ui.set_pad_index(
        if dir.parent().is_some() && ui.get_browse_entries().row_count() > 1 {
            1
        } else {
            0
        },
    );
}

fn set_browse_dir(ui: &AppWindow, state: &Shared, dir: PathBuf) {
    {
        let mut st = state.borrow_mut();
        st.browse_dir = dir.clone();
        st.settings.last_install_dir = Some(dir);
        let _ = st.settings.save();
    }
    list_browse_dir(ui, state);
}

/// Look inside an archive and ask the user to confirm the install.
fn plan_install(ui: &AppWindow, state: &Shared, file: PathBuf) {
    let Some(mods_dir) = state.borrow().game.as_ref().and_then(|g| g.mods_dir().ok()) else {
        return;
    };
    let plan = match install::plan(&mods_dir, &file) {
        Ok(p) => p,
        Err(e) => {
            status(ui, &e.to_string(), true);
            return;
        }
    };
    let file_name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let any_exists = plan.mods.iter().any(|m| m.exists);
    let body = if plan.nested.is_some() {
        format!(
            "{file_name} contains another archive. It will be unpacked and the mods inside installed."
        )
    } else {
        let lines: Vec<String> = plan
            .mods
            .iter()
            .map(|m| {
                format!(
                    "• {} ({} files, {}){}",
                    m.folder,
                    m.files,
                    human_size(m.size),
                    if m.exists {
                        " — already installed"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        format!(
            "{}{}",
            lines.join("\n"),
            if any_exists {
                "\n\nUpdate replaces the installed version and keeps its on/off state and settings. Keep both installs it next to it."
            } else {
                ""
            }
        )
    };
    let title = if plan.mods.len() > 1 {
        format!("Install {} mods from {file_name}?", plan.mods.len())
    } else {
        format!("Install {file_name}?")
    };
    state.borrow_mut().pending = Some(Pending::Install(plan, Origin::File(file)));
    ui.set_confirm_title(title.into());
    ui.set_confirm_body(body.into());
    ui.set_confirm_accept(if any_exists { "Update" } else { "Install" }.into());
    ui.set_confirm_alt(if any_exists { "Keep both" } else { "" }.into());
    ui.set_confirm_visible(true);
}

/// Unpack in the background with progress; register on the UI thread.
fn run_install(
    ui: &AppWindow,
    state: &Shared,
    plan: install::Plan,
    origin: Origin,
    if_exists: install::IfExists,
) {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let Some(mods_dir) = state.borrow().game.as_ref().and_then(|g| g.mods_dir().ok()) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    state.borrow_mut().install_cancel = Some(cancel.clone());
    ui.set_install_title(format!("Installing {}", origin.title()).into());
    ui.set_install_detail("Checking free space…".into());
    ui.set_install_progress(0.0);
    ui.set_install_running(true);

    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let result = install::install(
            &mods_dir,
            &plan,
            if_exists,
            &mut |step| {
                let now = std::time::Instant::now();
                let (progress, detail) = match step {
                    install::Step::Checking => (0.0, "Checking free space…".to_owned()),
                    install::Step::Unpacking(p) => {
                        if now.duration_since(last) < std::time::Duration::from_millis(100) {
                            return;
                        }
                        let frac = if p.total > 0 {
                            p.done as f32 / p.total as f32
                        } else {
                            0.0
                        };
                        (
                            frac,
                            format!(
                                "{} of {} · {}",
                                human_size(p.done),
                                human_size(p.total),
                                p.file
                            ),
                        )
                    }
                    install::Step::Finishing => (1.0, "Moving into place…".to_owned()),
                };
                last = now;
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_install_progress(progress);
                    ui.set_install_detail(detail.into());
                });
            },
            &cancel,
        );
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_install_running(false);
            with_state(|st| finish_install(&ui, st, result, origin));
        });
    });
}

fn finish_install(
    ui: &AppWindow,
    state: &Shared,
    result: megamix_core::Result<Vec<install::Installed>>,
    origin: Origin,
) {
    state.borrow_mut().install_cancel = None;
    let installed = match result {
        Ok(i) => i,
        Err(megamix_core::Error::Cancelled) => {
            status(ui, "Install cancelled. Nothing was changed.", false);
            updates::after(ui, state, true);
            return;
        }
        Err(e) => {
            status(ui, &format!("Install failed: {e}"), true);
            updates::after(ui, state, false);
            return;
        }
    };
    if installed.is_empty() {
        status(ui, "Nothing installed (already installed).", false);
        updates::after(ui, state, false);
        return;
    }
    let folders: Vec<String> = installed.iter().map(|m| m.folder.clone()).collect();
    if matches!(origin, Origin::Remote { .. }) {
        updates::installed(ui, state, &folders);
    }
    let mut st = state.borrow_mut();
    let Some(game) = st.game.as_mut() else { return };
    let registered = match &origin {
        Origin::File(archive) => install::register(game, &installed, archive, NewModPlacement::Top),
        Origin::Remote { m, file, .. } => {
            megamix_core::get::register_remote(game, &installed, m, file, NewModPlacement::Top)
        }
    };
    match registered {
        Ok(_) => {
            let names: Vec<String> = installed
                .iter()
                .map(|m| {
                    format!(
                        "{} {}",
                        if m.replaced { "updated" } else { "installed" },
                        m.folder
                    )
                })
                .collect();
            status(ui, &upper_first(&format!("{}.", names.join(", "))), false);
        }
        Err(e) => status(
            ui,
            &format!("Installed, but saving the load order failed: {e}"),
            true,
        ),
    }
    let first = installed[0].folder.clone();
    st.filter.clear();
    refresh_mods(ui, &st, Some(&first));
    drop(st);
    match origin {
        // Keep browsing after installing from a site.
        Origin::Remote { .. } => getmods::refresh_badges(ui, state),
        Origin::File(_) => ui.set_page(0),
    }
    updates::after(ui, state, false);
}

/// Hide the window while the game runs and show it again when the game
/// exits. If the game hasn't started within 90 s, come back anyway.
fn hide_while_playing(ui: &AppWindow) {
    const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
    let _ = ui.hide();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut seen = false;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if doctor::game_running() {
                seen = true;
            } else if seen || started.elapsed() > START_TIMEOUT {
                break;
            }
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let _ = ui.show();
            ui.invoke_refresh();
            if seen {
                status(&ui, "Welcome back.", false);
            } else {
                status(
                    &ui,
                    "The game didn't start within 90 seconds. Check Steam for errors.",
                    true,
                );
            }
        });
    });
}

// ---- controller -----------------------------------------------------------

/// Route a controller action to the UI (`pad()` in app.slint).
fn handle_pad(ui: &AppWindow, p: Pad) {
    ui.invoke_pad(p.name().into());
}

/// `--pad-script "down,down,x,up,a,shot:/tmp/x.png,quit"`: press controller
/// buttons one by one (every 400 ms), with screenshots in between. For testing
/// controller navigation without a controller.
fn run_pad_script(ui: &AppWindow, script: String) {
    let steps: std::collections::VecDeque<String> =
        script.split(',').map(|s| s.trim().to_owned()).collect();
    let steps = RefCell::new(steps);
    let weak = ui.as_weak();
    let timer = Box::leak(Box::new(slint::Timer::default()));
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(400),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(step) = steps.borrow_mut().pop_front() else {
                return;
            };
            if let Some(path) = step.strip_prefix("shot:") {
                match ui.window().take_snapshot() {
                    Ok(b) => {
                        if let Err(e) = write_png(
                            std::path::Path::new(path),
                            b.width(),
                            b.height(),
                            b.as_bytes(),
                        ) {
                            eprintln!("pad-script: {e}");
                        }
                    }
                    Err(e) => eprintln!("pad-script: {e}"),
                }
            } else if let Some(path) = step.strip_prefix("open:") {
                ui.invoke_install_from_path(path.into());
            } else if step == "sort" {
                ui.invoke_auto_sort();
            } else if step == "wait" {
                // A pause of one step.
            } else if step == "hide" {
                let _ = ui.hide();
            } else if step == "show" {
                let _ = ui.show();
            } else if step == "quit" {
                let _ = slint::quit_event_loop();
            } else if let Some(p) = Pad::parse(&step) {
                handle_pad(&ui, p);
            } else if !step.is_empty() {
                eprintln!("pad-script: unknown step `{step}`");
            }
        },
    );
}

// ---- helpers ------------------------------------------------------------

/// `--screenshot <file.png>`: save the window after it has settled, then quit.
/// For testing on machines without a screenshot tool.
fn schedule_screenshot(ui: &AppWindow, out: PathBuf) {
    let weak = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(2000), move || {
        let Some(ui) = weak.upgrade() else { return };
        match ui.window().take_snapshot() {
            Ok(buf) => {
                if let Err(e) = write_png(&out, buf.width(), buf.height(), buf.as_bytes()) {
                    eprintln!("screenshot: {e}");
                }
            }
            Err(e) => eprintln!("screenshot: {e}"),
        }
        let _ = slint::quit_event_loop();
    });
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(rgba)
        .map_err(std::io::Error::other)?;
    Ok(())
}

fn arg_value(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            let prefix = format!("{name}=");
            args.iter()
                .find_map(|a| a.strip_prefix(&prefix).map(str::to_owned))
        })
}

fn status(ui: &AppWindow, text: &str, error: bool) {
    ui.set_status_text(text.into());
    ui.set_status_error(error);
}

fn walk_size(dir: &std::path::Path) -> u64 {
    fn go(d: &std::path::Path) -> u64 {
        std::fs::read_dir(d)
            .map(|rd| {
                rd.flatten()
                    .map(|e| match e.file_type() {
                        Ok(t) if t.is_dir() => go(&e.path()),
                        Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
                        Err(_) => 0,
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    go(dir)
}

fn net_host(url: &str) -> String {
    megamix_core::net::host_of(url).to_owned()
}

fn row(
    i: &ModInfo,
    recs: &std::collections::BTreeMap<String, megamix_core::records::Record>,
    found: &std::collections::HashMap<String, megamix_core::get::Update>,
    pins: &megamix_core::sort::Pins,
) -> ModRow {
    let (source, link) = updates::link_info(recs.get(&i.folder));
    let pin = match megamix_core::sort::pin_of(pins, &i.folder) {
        megamix_core::sort::Pin::First => 1,
        megamix_core::sort::Pin::Last => 2,
        megamix_core::sort::Pin::None => 0,
    };
    let update = found.get(&i.folder);
    let mut files = Vec::new();
    match &i.include {
        Some(inc) => files.push(format!("Replaces files from: {}", inc.join(", "))),
        None => files.push("Replaces no game files".to_owned()),
    }
    if !i.dll.is_empty() {
        files.push(format!("Code: {}", i.dll.join(", ")));
    }
    ModRow {
        folder: i.folder.as_str().into(),
        name: i.name.as_str().into(),
        version: i.version.clone().unwrap_or_default().into(),
        author: i.author.clone().unwrap_or_default().into(),
        description: i.description.clone().unwrap_or_default().into(),
        enabled: i.enabled,
        loaded: i.loaded,
        priority: i.priority.map(|p| p as i32).unwrap_or(0),
        broken: i.error.is_some(),
        error: i.error.clone().unwrap_or_default().into(),
        files: files.join("\n").into(),
        source: source.into(),
        link: link.into(),
        update: update.is_some(),
        pin,
        update_text: update
            .map(|u| match (&u.installed_version, &u.new_version) {
                (Some(a), Some(b)) if a != b => format!("Update available: {a} → {b}"),
                _ => "Update available".to_owned(),
            })
            .unwrap_or_default()
            .into(),
    }
}

fn matches(i: &ModInfo, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    [Some(&i.name), Some(&i.folder), i.author.as_ref()]
        .into_iter()
        .flatten()
        .any(|s| s.to_lowercase().contains(&f))
}

/// Re-read the mod list into the UI, keeping `keep` selected if given.
fn refresh_mods(ui: &AppWindow, st: &State, keep: Option<&str>) {
    let Some(game) = &st.game else {
        st.mods.set_vec(Vec::new());
        return;
    };
    let infos = match game.infos() {
        Ok(i) => i,
        Err(e) => {
            status(ui, &e.to_string(), true);
            return;
        }
    };
    ui.set_mod_count(infos.len() as i32);
    ui.set_loaded_count(infos.iter().filter(|i| i.loaded).count() as i32);
    let visible: Vec<&ModInfo> = infos.iter().filter(|i| matches(i, &st.filter)).collect();

    let prev = keep.map(str::to_owned).or_else(|| {
        let sel = ui.get_selected();
        (sel >= 0)
            .then(|| st.mods.row_data(sel as usize))
            .flatten()
            .map(|r| r.folder.to_string())
    });
    let recs = megamix_core::records::load(&game.dir);
    let pins = megamix_core::sort::load_pins(&game.dir);
    st.mods.set_vec(
        visible
            .iter()
            .map(|i| row(i, &recs, &st.upd.found, &pins))
            .collect::<Vec<_>>(),
    );
    let sel = prev
        .and_then(|f| visible.iter().position(|i| i.folder == f))
        .map(|p| p as i32)
        .unwrap_or(if visible.is_empty() { -1 } else { 0 });
    ui.set_selected(sel);
    if sel >= 0 {
        ui.invoke_select_mod(sel);
    }
    let sel_folder = (sel >= 0)
        .then(|| st.mods.row_data(sel as usize))
        .flatten()
        .map(|r| r.folder.to_string())
        .unwrap_or_default();
    load_settings(ui, st, &sel_folder);

    let loader_on = game.loader.enabled().unwrap_or(true);
    ui.set_loader_warning(if loader_on {
        SharedString::new()
    } else {
        "Mod loading is turned off in DivaModLoader's config.toml, so no mods load.".into()
    });
    ui.set_can_undo(matches!(backup::undo_target(&game.dir), Ok(Some(_))));
}

/// Show the selected mod's own settings.
fn load_settings(ui: &AppWindow, st: &State, folder: &str) {
    use megamix_core::modsettings::{self, Kind};
    let rows: Vec<SettingRow> = st
        .game
        .as_ref()
        .and_then(|g| g.mods.iter().find(|m| m.folder == folder))
        .and_then(|m| m.config.as_ref())
        .map(|c| {
            modsettings::list(c)
                .into_iter()
                .map(|s| SettingRow {
                    path: s.path().into(),
                    kind: match s.kind {
                        Kind::Bool => 0,
                        Kind::Int => 1,
                        Kind::Float => 2,
                        Kind::Text => 3,
                        Kind::Other => 4,
                    },
                    value: s.value.into(),
                    comment: s.comment.into(),
                })
                .collect()
        })
        .unwrap_or_default();
    ui.set_mod_settings(ModelRc::new(VecModel::from(rows)));
}

fn open_game(ui: &AppWindow, state: &Shared) {
    let mut st = state.borrow_mut();
    let manual = st
        .override_dir
        .clone()
        .or_else(|| st.settings.game_dir.clone());
    let result = steam::find_game(manual.as_deref())
        .map_err(|e| e.to_string())
        .and_then(|i| Game::open(&i.game_dir).map_err(|e| e.to_string()));
    match result {
        Ok(game) => {
            ui.set_game_ok(true);
            ui.set_game_path(game.dir.display().to_string().into());
            ui.set_game_dir_input(game.dir.display().to_string().into());
            st.game = Some(game);
            refresh_mods(ui, &st, None);
            status(ui, "Ready.", false);
        }
        Err(e) => {
            st.game = None;
            refresh_mods(ui, &st, None);
            ui.set_game_ok(false);
            ui.set_game_path(SharedString::new());
            ui.set_page(5);
            ui.set_settings_message(e.clone().into());
            status(ui, &e, true);
        }
    }
}

/// Run a change on the game, save it (with backup) and refresh the UI.
fn change(
    ui: &AppWindow,
    state: &Shared,
    keep: Option<&str>,
    f: impl FnOnce(&mut Game) -> megamix_core::Result<String>,
) {
    let mut st = state.borrow_mut();
    let Some(game) = st.game.as_mut() else {
        return;
    };
    let result = f(game).and_then(|reason| game.save(&reason).map(|r| (reason, r)));
    match result {
        Ok((reason, r)) if r.backup.is_some() => status(ui, &format!("Saved: {reason}."), false),
        Ok(_) => status(ui, "Nothing to change.", false),
        Err(e) => {
            // Drop half-applied in-memory changes.
            let _ = game.reload();
            status(ui, &e.to_string(), true);
        }
    }
    refresh_mods(ui, &st, keep);
}

fn refresh_page(ui: &AppWindow, state: &Shared, page: i32) {
    if page == 5 {
        ui.set_links_ours(links::is_handler());
    }
    let st = state.borrow();
    let Some(game) = &st.game else {
        if page == 4 {
            drop(st);
            run_doctor(ui, state);
        }
        return;
    };
    match page {
        1 => {
            drop(st);
            scan_conflicts(ui, state);
        }
        2 => {
            let rows: Vec<LoadoutRow> = loadout::list(&game.dir)
                .unwrap_or_default()
                .into_iter()
                .map(|l| LoadoutRow {
                    summary: format!("{} of {} mods on", l.enabled.len(), l.order.len()).into(),
                    name: l.name.into(),
                })
                .collect();
            ui.set_loadouts(ModelRc::new(VecModel::from(rows)));
        }
        3 => {
            let kept = game
                .mods_dir()
                .map(|d| megamix_core::remove::kept(&d))
                .unwrap_or_default();
            ui.set_removed_summary(if kept.is_empty() {
                SharedString::new()
            } else {
                let n: usize = kept.iter().map(|k| k.folders.len()).sum();
                let size: u64 = kept.iter().map(|k| k.size).sum();
                format!(
                    "{n} removed mod(s) kept for undo ({}). They're deleted {} days after removal.",
                    human_size(size),
                    megamix_core::remove::KEEP_DAYS
                )
                .into()
            });
            let all = backup::list(&game.dir).unwrap_or_default();
            let reason_of = |id: &str| {
                all.iter()
                    .find(|b| b.id == id)
                    .map(|b| b.reason.clone())
                    .unwrap_or_else(|| id.to_owned())
            };
            let rows: Vec<BackupRow> = all
                .iter()
                .map(|b| BackupRow {
                    when: pretty_time(&b.created).into(),
                    reason: match &b.restored_from {
                        Some(from) => format!("Before undoing “{}”", reason_of(from)),
                        None => upper_first(&b.reason),
                    }
                    .into(),
                    id: b.id.as_str().into(),
                })
                .collect();
            ui.set_backups(ModelRc::new(VecModel::from(rows)));
        }
        4 => {
            drop(st);
            run_doctor(ui, state);
        }
        6 => {
            drop(st);
            list_browse_dir(ui, state);
        }
        7 => {
            drop(st);
            getmods::on_page_opened(ui, state);
        }
        _ => {}
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn pretty_time(rfc3339: &str) -> String {
    // "2026-11-21T12:00:00.123+01:00" -> "2026-11-21 12:00:00"
    rfc3339
        .get(..19)
        .map(|s| s.replace('T', " "))
        .unwrap_or_else(|| rfc3339.to_owned())
}

fn run_doctor(ui: &AppWindow, state: &Shared) {
    let dir = state.borrow().game.as_ref().map(|g| g.dir.clone());
    ui.set_busy(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let report = doctor::run(dir.as_deref());
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let rows: Vec<CheckRow> = report
                .checks
                .iter()
                .map(|c| CheckRow {
                    level: match c.level {
                        Level::Ok => 0,
                        Level::Info => 1,
                        Level::Warn => 2,
                        Level::Error => 3,
                    },
                    summary: c.summary.as_str().into(),
                    details: c.details.join("\n").into(),
                    fix: c
                        .fix
                        .clone()
                        .map(|f| format!("Fix: {f}"))
                        .unwrap_or_default()
                        .into(),
                })
                .collect();
            let summary = match report.worst() {
                Level::Error => "Some problems stop mods from loading.",
                Level::Warn => "Mostly fine, with warnings.",
                _ => "Everything looks good.",
            };
            ui.set_doctor_summary(summary.into());
            ui.set_checks(ModelRc::new(VecModel::from(rows)));
            ui.set_busy(false);
            with_state(|st| refresh_armfix(&ui, st));
        });
    });
}

/// The ARM fix card on the Doctor page.
fn refresh_armfix(ui: &AppWindow, state: &Shared) {
    use megamix_core::armfix::{self, DXVK_VERSION, Need, State};
    let s = state.borrow();
    let Some(game) = s.game.as_ref() else {
        ui.set_armfix_visible(false);
        return;
    };
    let st = armfix::status(game);
    let mods = if st.affected_mods.is_empty() {
        String::new()
    } else {
        format!(
            " You have {} enabled, which needs it.",
            st.affected_mods.join(", ")
        )
    };
    let (title, text, action) = match (&st.need, &st.state) {
        // x86: only show it to take out a fix this app added.
        (
            Need::NotNeeded,
            State::NotInstalled | State::Other { .. } | State::Installed { by_us: false },
        ) => {
            ui.set_armfix_visible(false);
            return;
        }
        (_, State::Installed { by_us: true }) => (
            "ARM fix: on".to_string(),
            format!(
                "DXVK {DXVK_VERSION} (x64) dxgi.dll and d3d11.dll are in the game folder, so mods that hook rendering (like LyricsPatch) don't crash.{mods}"
            ),
            "Remove",
        ),
        (_, State::Installed { by_us: false }) => (
            "ARM fix: on (set up by hand)".to_string(),
            format!(
                "DXVK {DXVK_VERSION} (x64) dxgi.dll and d3d11.dll are in the game folder. They weren't added by this app, so it leaves them alone.{mods}"
            ),
            "",
        ),
        (_, State::Other { files }) => (
            "ARM fix: other DLLs in the game folder".to_string(),
            format!(
                "Found {}. They're left alone. Add the fix to replace them with DXVK {DXVK_VERSION} (they're backed up and come back when you remove it).{mods}",
                files.join(", ")
            ),
            "Add the fix",
        ),
        (Need::Recommended, State::NotInstalled) => (
            "ARM fix: off".to_string(),
            format!(
                "On ARM64 devices, mods that hook the game's rendering (like LyricsPatch) crash the game. Adding the official x64 DXVK {DXVK_VERSION} dxgi.dll and d3d11.dll to the game folder fixes that.{mods}"
            ),
            "Add the fix",
        ),
        (Need::Experimental, State::NotInstalled) => (
            "ARM fix (experimental on Windows on ARM)".to_string(),
            format!(
                "If rendering mods like LyricsPatch crash the game on this ARM PC, x64 DXVK {DXVK_VERSION} might help. It's untested on Windows and needs a working Vulkan driver.{mods}"
            ),
            "Try it",
        ),
    };
    ui.set_armfix_title(title.into());
    ui.set_armfix_text(text.into());
    ui.set_armfix_action(action.into());
    ui.set_armfix_visible(true);
}

fn armfix_clicked(ui: &AppWindow, state: &Shared) {
    use megamix_core::armfix::{self, DXVK_VERSION, State};
    let installed = {
        let s = state.borrow();
        let Some(game) = s.game.as_ref() else { return };
        matches!(armfix::status(game).state, State::Installed { by_us: true })
    };
    if installed {
        confirm(
            ui,
            state,
            Pending::ArmFix(false),
            "Remove the ARM fix?",
            "The DXVK dxgi.dll and d3d11.dll are deleted from the game folder, and any files they replaced are put back.",
            "Remove",
        );
    } else {
        confirm(
            ui,
            state,
            Pending::ArmFix(true),
            "Add the ARM fix?",
            &format!(
                "Downloads DXVK {DXVK_VERSION} from its official GitHub release (18 MB), checks it, and puts the x64 dxgi.dll and d3d11.dll in the game folder. Files with those names already there are backed up. Remove puts everything back."
            ),
            "Add",
        );
    }
}

fn run_armfix(ui: &AppWindow, state: &Shared, apply: bool) {
    use megamix_core::armfix;
    let Some(game) = state.borrow().game.clone() else {
        return;
    };
    if doctor::game_running() {
        status(ui, "Close the game first: its DLLs are in use.", true);
        return;
    }
    ui.set_busy(true);
    status(
        ui,
        if apply {
            "Downloading DXVK…"
        } else {
            "Removing the ARM fix…"
        },
        false,
    );
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let res: Result<String, String> = if apply {
            let client = megamix_core::net::Client::new();
            let w = weak.clone();
            let mut last = 0;
            armfix::apply(
                &client,
                &game,
                true,
                &mut |p| {
                    if p.total > 0 && p.done * 100 / p.total != last {
                        last = p.done * 100 / p.total;
                        let msg = format!("Downloading DXVK… {last}%");
                        let _ = w.upgrade_in_event_loop(move |ui| status(&ui, &msg, false));
                    }
                },
                &cancel,
            )
            .map(|a| {
                if a.already {
                    "The ARM fix was already there.".into()
                } else {
                    "ARM fix added. Mods like LyricsPatch should work now.".into()
                }
            })
            .map_err(|e| e.to_string())
        } else {
            armfix::remove(&game, false)
                .map(|_| "ARM fix removed.".into())
                .map_err(|e| e.to_string())
        };
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy(false);
            match res {
                Ok(m) => status(&ui, &m, false),
                Err(e) => status(&ui, &e, true),
            }
            with_state(|st| {
                refresh_armfix(&ui, st);
                run_doctor(&ui, st);
            });
        });
    });
}

fn scan_conflicts(ui: &AppWindow, state: &Shared) {
    let Some(game) = state.borrow().game.clone() else {
        return;
    };
    ui.set_busy(true);
    ui.set_conflicts_summary("Scanning mod files…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = conflicts::find(&game);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy(false);
            match result {
                Ok(c) => {
                    let pairs: Vec<PairRow> = conflicts::pairs(&c)
                        .into_iter()
                        .map(|p| PairRow {
                            winner: p.winner.into(),
                            loser: p.loser.into(),
                            files: p.files as i32,
                        })
                        .collect();
                    let files: Vec<FileRow> = c
                        .iter()
                        .map(|x| FileRow {
                            path: x.path.as_str().into(),
                            winner: x.winner.as_str().into(),
                            losers: x.overridden.join(", ").into(),
                        })
                        .collect();
                    ui.set_conflicts_summary(
                        if c.is_empty() {
                            "No conflicts between the mods that will load.".to_owned()
                        } else {
                            format!(
                                "{} file(s) are changed by more than one mod. The mod higher in the list wins; reorder on the Mods page to change that.",
                                c.len()
                            )
                        }
                        .into(),
                    );
                    ui.set_pairs(ModelRc::new(VecModel::from(pairs)));
                    ui.set_conflict_files(ModelRc::new(VecModel::from(files)));
                }
                Err(e) => ui.set_conflicts_summary(e.to_string().into()),
            }
        });
    });
}

fn confirm(ui: &AppWindow, state: &Shared, p: Pending, title: &str, body: &str, accept: &str) {
    state.borrow_mut().pending = Some(p);
    ui.set_confirm_title(title.into());
    ui.set_confirm_body(body.into());
    ui.set_confirm_accept(accept.into());
    ui.set_confirm_alt(SharedString::new());
    ui.set_confirm_warning(SharedString::new());
    ui.set_confirm_visible(true);
}

// ---- callbacks ----------------------------------------------------------

fn connect(ui: &AppWindow, state: &Shared) {
    macro_rules! on {
        ($setter:ident, |$ui:ident, $st:ident $(, $arg:ident : $t:ty)*| $body:block) => {{
            let weak = ui.as_weak();
            let state = state.clone();
            ui.$setter(move |$($arg: $t),*| {
                let $ui = weak.unwrap();
                let $st = &state;
                $body
            });
        }};
    }

    on!(on_search, |ui, st, text: SharedString| {
        st.borrow_mut().filter = text.to_string();
        refresh_mods(&ui, &st.borrow(), None);
    });

    on!(on_toggle, |ui, st, folder: SharedString| {
        let folder = folder.to_string();
        let keep = folder.clone();
        change(&ui, st, Some(&keep), |g| {
            let i = g.find(&folder)?;
            let on = !g.mods[i].enabled();
            g.set_enabled(&folder, on)?;
            Ok(format!(
                "{} {folder}",
                if on { "enable" } else { "disable" }
            ))
        });
    });

    on!(on_move, |ui,
                  st,
                  folder: SharedString,
                  how: SharedString| {
        if folder.is_empty() {
            return;
        }
        let folder = folder.to_string();
        let mv = match how.as_str() {
            "top" => Move::Top,
            "up" => Move::Up(1),
            "down" => Move::Down(1),
            _ => Move::Bottom,
        };
        let keep = folder.clone();
        change(&ui, st, Some(&keep), |g| {
            let pos = g.move_mod(&folder, mv)?;
            Ok(format!("move {folder} to {pos}"))
        });
    });

    on!(on_sync_priority, |ui, st| {
        change(&ui, st, None, |g| {
            let added = g.sync_priority(NewModPlacement::Top)?;
            Ok(format!("add {} to the load order", added.join(", ")))
        });
    });

    on!(on_enable_loader, |ui, st| {
        change(&ui, st, None, |g| {
            g.loader.set_enabled(true);
            Ok("turn mod loading on".into())
        });
    });

    on!(on_refresh, |ui, st| {
        {
            let mut s = st.borrow_mut();
            if let Some(g) = s.game.as_mut()
                && let Err(e) = g.reload()
            {
                status(&ui, &e.to_string(), true);
            }
        }
        refresh_mods(&ui, &st.borrow(), None);
        status(&ui, "Reloaded from disk.", false);
    });

    on!(on_undo, |ui, st| {
        let mut s = st.borrow_mut();
        let Some(game) = s.game.as_mut() else {
            return;
        };
        match backup::undo(game) {
            Ok(Some(r)) => status(&ui, &format!("Undone: {}.", r.restored.reason), false),
            Ok(None) => status(&ui, "Nothing to undo.", false),
            Err(e) => status(&ui, &e.to_string(), true),
        }
        refresh_mods(&ui, &s, None);
        let page = ui.get_page();
        drop(s);
        refresh_page(&ui, st, page);
    });

    on!(on_page_changed, |ui, st, page: i32| {
        refresh_page(&ui, st, page);
    });

    on!(on_run_doctor, |ui, st| {
        run_doctor(&ui, st);
    });

    on!(on_scan_conflicts, |ui, st| {
        scan_conflicts(&ui, st);
    });

    on!(on_save_loadout, |ui, st, name: SharedString| {
        let name = name.trim().to_owned();
        let res = st.borrow().game.as_ref().map(|g| loadout::save(g, &name));
        match res {
            Some(Ok(l)) => status(&ui, &format!("Saved loadout “{}”.", l.name), false),
            Some(Err(e)) => status(&ui, &e.to_string(), true),
            None => {}
        }
        refresh_page(&ui, st, 2);
    });

    on!(on_play, |ui, _st| {
        if doctor::game_running() {
            status(&ui, "The game is already running.", false);
            return;
        }
        if megamix_core::launch::needs_dll_override() {
            let roots = steam::steam_roots();
            let users: Vec<_> = roots
                .iter()
                .flat_map(|r| megamix_core::launch::read_all(r))
                .collect();
            if !users.is_empty() && !users.iter().any(|u| u.check.ok()) {
                status(
                    &ui,
                    "Starting, but mods won't load: Steam launch options are missing the DivaModLoader override (see Doctor).",
                    true,
                );
                if megamix_core::launch::start_game().is_ok() {
                    hide_while_playing(&ui);
                }
                return;
            }
        }
        match megamix_core::launch::start_game() {
            Ok(()) => {
                status(&ui, "Starting the game through Steam…", false);
                hide_while_playing(&ui);
            }
            Err(e) => status(&ui, &format!("Couldn't start the game: {e}"), true),
        }
    });

    on!(on_browse_activate, |ui,
                             st,
                             path: SharedString,
                             kind: i32| {
        let p = PathBuf::from(path.as_str());
        if kind == 2 {
            plan_install(&ui, st, p);
        } else {
            set_browse_dir(&ui, st, p);
        }
    });

    on!(on_browse_up, |ui, st| {
        let parent = st.borrow().browse_dir.parent().map(PathBuf::from);
        if let Some(p) = parent {
            set_browse_dir(&ui, st, p);
        } else if ui.get_game_ok() {
            ui.set_page(0);
        }
    });

    on!(on_browse_go, |ui, st, which: SharedString| {
        let dir = if which == "home" {
            dirs::home_dir()
        } else {
            dirs::download_dir().or_else(dirs::home_dir)
        };
        if let Some(d) = dir {
            set_browse_dir(&ui, st, d);
        }
    });

    on!(on_install_from_path, |ui, st, text: SharedString| {
        let p = PathBuf::from(text.trim().trim_matches('"'));
        if p.is_dir() {
            set_browse_dir(&ui, st, p);
        } else if p.is_file() {
            plan_install(&ui, st, p);
        } else {
            status(&ui, &format!("{} doesn't exist.", p.display()), true);
        }
    });

    on!(on_install_cancel, |ui, st| {
        if let Some(c) = &st.borrow().install_cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
            ui.set_install_detail("Cancelling…".into());
        }
    });

    on!(on_confirm_alt_accepted, |ui, st| {
        let pending = st.borrow_mut().pending.take();
        if let Some(Pending::Install(plan, origin)) = pending {
            run_install(&ui, st, plan, origin, install::IfExists::KeepBoth);
        }
    });

    on!(on_save_loadout_auto, |ui, st| {
        let res = st.borrow().game.as_ref().map(|g| {
            let taken: Vec<String> = loadout::list(&g.dir)
                .unwrap_or_default()
                .into_iter()
                .map(|l| l.name)
                .collect();
            let name = (1..)
                .map(|n| format!("Loadout {n}"))
                .find(|n| !taken.contains(n))
                .expect("unbounded");
            loadout::save(g, &name)
        });
        match res {
            Some(Ok(l)) => status(&ui, &format!("Saved loadout “{}”.", l.name), false),
            Some(Err(e)) => status(&ui, &e.to_string(), true),
            None => {}
        }
        refresh_page(&ui, st, 2);
    });

    on!(on_apply_loadout, |ui, st, name: SharedString| {
        confirm(
            &ui,
            st,
            Pending::ApplyLoadout(name.to_string()),
            &format!("Apply “{name}”?"),
            "Mods will be turned on or off and reordered to match this loadout. Mods that aren't in it are turned off. You can undo this.",
            "Apply",
        );
    });

    on!(on_delete_loadout, |ui, st, name: SharedString| {
        confirm(
            &ui,
            st,
            Pending::DeleteLoadout(name.to_string()),
            &format!("Delete “{name}”?"),
            "Only the saved loadout is deleted. Your mods are not changed.",
            "Delete",
        );
    });

    on!(on_restore_backup, |ui, st, id: SharedString| {
        confirm(
            &ui,
            st,
            Pending::RestoreBackup(id.to_string()),
            "Restore this backup?",
            "Mod settings and the load order go back to how they were. Your current state is backed up first, so you can undo this.",
            "Restore",
        );
    });

    on!(on_mod_selected, |ui, st, folder: SharedString| {
        load_settings(&ui, &st.borrow(), &folder);
    });
    on!(
        on_set_setting,
        |ui, st, folder: SharedString, path: SharedString, value: SharedString| {
            let (folder, path, value) = (folder.to_string(), path.to_string(), value.to_string());
            let keep = folder.clone();
            change(&ui, st, Some(&keep), |g| {
                let i = g.find(&folder)?;
                let cfg = g.mods[i].config.as_mut().ok_or_else(|| {
                    megamix_core::Error::Other(format!("{folder} has no readable config.toml"))
                })?;
                megamix_core::modsettings::set(cfg, &path, &value)?;
                Ok(format!("set {path} of {folder}"))
            });
        }
    );
    on!(on_remove_mod, |ui, st, folder: SharedString| {
        let info = {
            let s = st.borrow();
            s.game.as_ref().and_then(|g| {
                let m = g.mods.iter().find(|m| m.folder == folder.as_str())?;
                let size: u64 = walk_size(&m.dir);
                Some((m.name().to_owned(), size))
            })
        };
        let Some((name, size)) = info else { return };
        confirm(
            &ui,
            st,
            Pending::Remove(folder.to_string()),
            &format!("Remove “{name}”?"),
            &format!(
                "The folder “{folder}” ({}) leaves the mods folder and the load order.\n\nIt's kept for {} days: Undo brings it back. After that it's deleted.",
                human_size(size),
                megamix_core::remove::KEEP_DAYS
            ),
            "Remove",
        );
    });
    on!(on_auto_sort, |ui, st| {
        let plan = match st.borrow().game.as_ref().map(megamix_core::sort::plan) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                status(&ui, &e.to_string(), true);
                return;
            }
            None => return,
        };
        if !plan.changed {
            match plan.warnings.first() {
                Some(w) => status(
                    &ui,
                    &format!("The load order is already sorted. Note: {w}"),
                    true,
                ),
                None => status(&ui, "The load order is already sorted.", false),
            }
            return;
        }
        let mut lines: Vec<String> = plan
            .moved
            .iter()
            .take(12)
            .map(|m| format!("• {}: {} → {} ({})", m.name, m.from, m.to, m.reason))
            .collect();
        if plan.moved.len() > 12 {
            lines.push(format!("…and {} more", plan.moved.len() - 12));
        }
        if lines.is_empty() {
            lines.push("• Mods not in the load order yet are added to it.".into());
        }
        let warnings = if plan.warnings.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nWorth knowing:\n{}",
                plan.warnings
                    .iter()
                    .map(|w| format!("⚠ {w}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let body = format!(
            "{}{warnings}\n\nRules come from the mods' own pages (thumbnail tools first, mods that say “above X” above X, lyrics/translation mods lower), plus your pins. Everything else keeps its order. Undo reverts it.",
            lines.join("\n")
        );
        confirm(
            &ui,
            st,
            Pending::Sort(plan),
            "Sort the load order?",
            &body,
            "Sort",
        );
    });
    on!(on_set_pin, |ui, st, folder: SharedString, pin: i32| {
        let dir = st.borrow().game.as_ref().map(|g| g.dir.clone());
        let Some(dir) = dir else { return };
        let p = match pin {
            1 => megamix_core::sort::Pin::First,
            2 => megamix_core::sort::Pin::Last,
            _ => megamix_core::sort::Pin::None,
        };
        match megamix_core::sort::set_pin(&dir, &folder, p) {
            Ok(()) => status(
                &ui,
                &match p {
                    megamix_core::sort::Pin::First => {
                        format!("{folder} stays at the top when auto-sorting.")
                    }
                    megamix_core::sort::Pin::Last => {
                        format!("{folder} stays at the bottom when auto-sorting.")
                    }
                    megamix_core::sort::Pin::None => format!("{folder} is no longer pinned."),
                },
                false,
            ),
            Err(e) => status(&ui, &e.to_string(), true),
        }
        refresh_mods(&ui, &st.borrow(), Some(&folder));
    });
    on!(on_purge_removed, |ui, st| {
        let dir = st.borrow().game.as_ref().and_then(|g| g.mods_dir().ok());
        if let Some(d) = dir {
            let freed = megamix_core::remove::purge(&d, None);
            status(
                &ui,
                &format!(
                    "Deleted removed mods for good ({} freed).",
                    human_size(freed)
                ),
                false,
            );
        }
        refresh_page(&ui, st, 3);
    });

    on!(on_check_updates, |ui, st| {
        updates::check(&ui, st, false);
    });
    on!(on_update_mod, |ui, st, folder: SharedString| {
        updates::update_one(&ui, st, &folder);
    });
    on!(on_update_all, |ui, st| {
        updates::update_all(&ui, st);
    });
    on!(on_link_mod, |ui,
                      st,
                      folder: SharedString,
                      text: SharedString| {
        updates::link(&ui, st, folder.to_string(), text.to_string());
    });
    on!(on_unlink_mod, |ui, st, folder: SharedString| {
        updates::unlink(&ui, st, &folder);
    });
    on!(on_open_mod_page, |ui, st, folder: SharedString| {
        updates::open_page(&ui, st, &folder);
    });

    on!(on_get_search, |ui, st| {
        getmods::search(&ui, st, false);
    });
    on!(on_get_load_more, |ui, st| {
        getmods::search(&ui, st, true);
    });
    on!(on_get_select, |ui, st, i: i32| {
        if i >= 0 {
            getmods::select(&ui, st, i as usize);
        }
    });
    on!(on_get_install, |ui, st, i: i32| {
        if i >= 0 {
            getmods::install_file(&ui, st, i as usize);
        }
    });
    on!(on_get_open_req, |ui, st, i: i32| {
        getmods::open_requirement(&ui, st, i.max(0) as usize);
    });
    on!(on_get_image, |ui, st, dir: i32| {
        getmods::step_image(&ui, st, dir);
    });
    on!(on_get_open_page, |ui, st| {
        getmods::open_page(&ui, st);
    });
    on!(on_get_open_also, |ui, st| {
        getmods::open_also(&ui, st);
    });

    on!(on_app_update_clicked, |ui, st| {
        appupdate::clicked(&ui, st);
    });
    on!(on_app_update_check_now, |ui, st| {
        appupdate::check(&ui, st, true);
    });
    on!(on_app_updates_toggled, |ui, st, on: bool| {
        appupdate::toggled(&ui, st, on);
    });

    on!(on_armfix_clicked, |ui, st| {
        armfix_clicked(&ui, st);
    });

    on!(on_confirm_cancelled, |ui, st| {
        let was_download = matches!(
            st.borrow_mut().pending.take(),
            Some(Pending::Install(_, Origin::Remote { .. }))
        );
        if was_download {
            status(&ui, "Cancelled. The download was deleted.", false);
        }
    });

    on!(on_confirm_accepted, |ui, st| {
        let pending = st.borrow_mut().pending.take();
        match pending {
            Some(Pending::Install(plan, origin)) => {
                run_install(&ui, st, plan, origin, install::IfExists::Replace);
            }
            Some(Pending::Sort(plan)) => {
                change(&ui, st, None, |g| {
                    g.loader.set_priority(&plan.order);
                    Ok("auto-sort the load order".into())
                });
            }
            Some(Pending::Remove(folder)) => {
                let res = {
                    let mut s = st.borrow_mut();
                    match s.game.as_mut() {
                        Some(g) => megamix_core::remove::remove(g, std::slice::from_ref(&folder)),
                        None => return,
                    }
                };
                match res {
                    Ok(_) => status(
                        &ui,
                        &format!(
                            "Removed {folder}. Undo brings it back (kept for {} days).",
                            megamix_core::remove::KEEP_DAYS
                        ),
                        false,
                    ),
                    Err(e) => status(&ui, &e.to_string(), true),
                }
                updates::installed(&ui, st, std::slice::from_ref(&folder));
                refresh_mods(&ui, &st.borrow(), None);
            }
            Some(Pending::Download(m, file)) => {
                getmods::start_download(&ui, st, *m, file, None);
            }
            Some(Pending::ApplyLoadout(name)) => {
                change(&ui, st, None, |g| {
                    let l = loadout::load(&g.dir, &name)?;
                    loadout::apply(g, &l)?;
                    Ok(format!("apply loadout {name}"))
                });
            }
            Some(Pending::DeleteLoadout(name)) => {
                let res = st
                    .borrow()
                    .game
                    .as_ref()
                    .map(|g| loadout::delete(&g.dir, &name));
                match res {
                    Some(Ok(())) => status(&ui, &format!("Deleted loadout “{name}”."), false),
                    Some(Err(e)) => status(&ui, &e.to_string(), true),
                    None => {}
                }
                refresh_page(&ui, st, 2);
            }
            Some(Pending::ArmFix(apply)) => run_armfix(&ui, st, apply),
            Some(Pending::AppUpdate(rel)) => appupdate::run(&ui, st, *rel),
            Some(Pending::Restart) => appupdate::restart(&ui),
            Some(Pending::RestoreBackup(id)) => {
                let mut s = st.borrow_mut();
                if let Some(game) = s.game.as_mut() {
                    match backup::restore(game, &id) {
                        Ok(r) => status(&ui, &format!("Restored: {}.", r.restored.reason), false),
                        Err(e) => status(&ui, &e.to_string(), true),
                    }
                }
                refresh_mods(&ui, &s, None);
                drop(s);
                refresh_page(&ui, st, 3);
            }
            None => {}
        }
    });

    on!(on_prefer_site_chosen, |ui, st, i: i32| {
        let mut s = st.borrow_mut();
        s.settings.prefer_site = Some(if i == 1 {
            megamix_core::remote::Site::GameBanana
        } else {
            megamix_core::remote::Site::Dma
        });
        if let Err(e) = s.settings.save() {
            status(&ui, &format!("Couldn't save settings: {e}"), true);
        }
    });

    on!(on_register_links, |ui, _st| {
        match links::register() {
            Ok(()) => status(&ui, "One-click install links now open this app.", false),
            Err(e) => status(&ui, &format!("Couldn't set up one-click links: {e}"), true),
        }
        ui.set_links_ours(links::is_handler());
    });

    on!(on_theme_chosen, |ui, st, t: i32| {
        let mut s = st.borrow_mut();
        s.settings.theme = match t {
            1 => Some("dark".into()),
            2 => Some("light".into()),
            _ => None,
        };
        if let Err(e) = s.settings.save() {
            status(&ui, &format!("Couldn't save settings: {e}"), true);
        }
    });

    on!(on_use_game_dir, |ui, st, dir: SharedString| {
        let dir = PathBuf::from(dir.trim());
        if !steam::is_game_dir(&dir) {
            ui.set_settings_message(
                format!("{} doesn't contain {}.", dir.display(), steam::GAME_EXE).into(),
            );
            return;
        }
        {
            let mut s = st.borrow_mut();
            s.override_dir = None;
            s.settings.game_dir = Some(dir);
            if let Err(e) = s.settings.save() {
                ui.set_settings_message(format!("Couldn't save settings: {e}").into());
            }
        }
        open_game(&ui, st);
        if ui.get_game_ok() {
            ui.set_settings_message("Game folder set.".into());
        }
    });

    on!(on_detect_game_dir, |ui, st| {
        {
            let mut s = st.borrow_mut();
            s.override_dir = None;
            s.settings.game_dir = None;
            if let Err(e) = s.settings.save() {
                ui.set_settings_message(format!("Couldn't save settings: {e}").into());
            }
        }
        open_game(&ui, st);
        if ui.get_game_ok() {
            ui.set_settings_message("Found the game through Steam.".into());
        }
    });
}
