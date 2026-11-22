//! `megamix`: command-line mod manager for Hatsune Miku: Project DIVA Mega Mix+.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use megamix_core::backup;
use megamix_core::conflicts;
use megamix_core::doctor::{self, Level};
use megamix_core::install;
use megamix_core::loadout;
use megamix_core::mods::{Game, Move, NewModPlacement, SaveReport};
use megamix_core::remote;
use megamix_core::steam;

mod remote_cmd;

#[derive(Parser)]
#[command(
    name = "megamix",
    version,
    about = "Mod manager for Project DIVA Mega Mix+ (DivaModLoader)"
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct Global {
    /// Game folder (default: detected from Steam). Also: MEGAMIX_GAME.
    #[arg(long, global = true, env = "MEGAMIX_GAME", value_name = "DIR")]
    game: Option<PathBuf>,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the game folder.
    Game,
    /// Start the game through Steam (uses your Steam launch options).
    Play,
    /// List mods in load order.
    List,
    /// Install mods from .zip / .7z / .rar files, DivaModArchive or
    /// GameBanana (dma:<id>, gb:<id>, a mod page URL or a one-click link).
    Install {
        #[arg(required = true, value_name = "FILE|MOD")]
        items: Vec<String>,
        /// For a mod with several files: the file to install (id from
        /// `megamix info`).
        #[arg(long, value_name = "ID")]
        file: Option<u64>,
        /// For a mod with several files: install all of them.
        #[arg(long, conflicts_with = "file")]
        all_files: bool,
        /// Install from the mod's external link N (Google Drive, …; see
        /// `megamix info`).
        #[arg(long, value_name = "N", conflicts_with_all = ["file", "all_files"])]
        link: Option<usize>,
        /// When a mod is already installed: replace it (keeps its on/off
        /// state and settings), keep both, or skip it.
        #[arg(long, value_enum, default_value = "replace")]
        if_exists: IfExistsArg,
        /// Put new mods at the bottom of the load order instead of the top.
        #[arg(long)]
        bottom: bool,
    },
    /// Search DivaModArchive and GameBanana.
    Search {
        /// Words to search for (empty = newest mods).
        #[arg(default_value = "")]
        query: String,
        #[arg(long, value_enum, default_value = "all")]
        source: SourceArg,
        #[arg(long, value_enum, default_value = "relevance")]
        sort: SortArg,
        /// Only this type of mod.
        #[arg(long = "type", value_enum, default_value = "any")]
        kind: KindArg,
        /// Page of results (1 = first).
        #[arg(long, default_value_t = 1)]
        page: usize,
    },
    /// Details and files of a mod on DivaModArchive or GameBanana.
    Info {
        /// dma:<id>, gb:<id>, a mod page URL or a one-click link.
        r#mod: String,
    },
    /// Check mods installed from DivaModArchive / GameBanana for updates.
    Updates,
    /// Tell the manager which DivaModArchive / GameBanana page a mod you
    /// installed by hand comes from, so it gets update checks. The
    /// installed copy is taken to be the page's current version.
    Link {
        r#mod: String,
        /// dma:<id>, gb:<id>, a mod page URL or a one-click link.
        page: String,
        /// Which file of the page it is (id from `megamix info`).
        #[arg(long, value_name = "ID")]
        file: Option<u64>,
    },
    /// Forget which page a mod came from.
    Unlink { r#mod: String },
    /// Update mods installed from DivaModArchive / GameBanana.
    Update {
        /// Mods to update (default with --all: every mod with an update).
        mods: Vec<String>,
        #[arg(long, conflicts_with = "mods")]
        all: bool,
    },
    /// Sort the load order automatically (thumbnail tools first, text mods
    /// lower, pinned mods where you pinned them; the rest keep their order).
    Sort {
        /// Only show what would move.
        #[arg(long)]
        dry_run: bool,
    },
    /// Keep a mod at the top or bottom when sorting.
    Pin {
        r#mod: String,
        #[arg(value_enum)]
        place: PinArg,
    },
    /// Remove mods (kept for 30 days; `megamix undo` brings them back).
    Remove {
        #[arg(required = true)]
        mods: Vec<String>,
    },
    /// Removed mods still kept for undo.
    Removed {
        /// Delete them for good now.
        #[arg(long)]
        purge: bool,
    },
    /// Show a mod's own settings (from its config.toml).
    Settings { r#mod: String },
    /// Change one of a mod's settings (`megamix settings <mod>` lists them).
    Set {
        r#mod: String,
        /// Setting name ("key" or "section.key").
        key: String,
        value: String,
    },
    /// Enable mods.
    Enable {
        #[arg(required = true)]
        mods: Vec<String>,
    },
    /// Disable mods.
    Disable {
        #[arg(required = true)]
        mods: Vec<String>,
    },
    /// Show or change the load order (first wins).
    #[command(subcommand)]
    Priority(PriorityCmd),
    /// Files replaced by more than one loaded mod, and which mod wins.
    Conflicts {
        /// List every file instead of a summary per mod pair.
        #[arg(long)]
        files: bool,
    },
    /// Check that DivaModLoader and the mods are set up correctly.
    Doctor,
    /// Config backups (taken automatically before every change).
    #[command(subcommand)]
    Backup(BackupCmd),
    /// Undo the last change (run again to go further back).
    Undo,
    /// Named sets of enabled mods and their order.
    #[command(subcommand)]
    Loadout(LoadoutCmd),
    /// DivaModLoader's own settings.
    #[command(subcommand)]
    Loader(LoaderCmd),
    /// ARM fix: x64 DXVK DLLs in the game folder so rendering mods like
    /// LyricsPatch don't crash on ARM64 (PB-OS, FEX/Proton).
    #[command(subcommand)]
    Armfix(ArmfixCmd),
    /// Add the manager to Steam as a non-Steam game (for Game Mode / Big Picture).
    #[command(subcommand, name = "steam-shortcut")]
    SteamShortcut(ShortcutCmd),
    /// Update this app to the newest release.
    #[command(name = "self-update")]
    SelfUpdate {
        /// Only check, don't install.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum ShortcutCmd {
    /// Is the shortcut in Steam?
    Status,
    /// Add or update the shortcut (in every Steam account on this PC).
    Add {
        /// Close Steam first if it's running, and start it again afterwards.
        #[arg(long)]
        close_steam: bool,
        /// The program to add (default: megamix-gui next to this program).
        #[arg(long)]
        exe: Option<PathBuf>,
        /// Icon for the library list (PNG).
        #[arg(long)]
        icon: Option<PathBuf>,
        /// Folder with capsule.png, wide.png, hero.png and logo.png.
        #[arg(long)]
        art: Option<PathBuf>,
    },
    /// Take the shortcut out of Steam again.
    Remove {
        #[arg(long)]
        close_steam: bool,
    },
}

#[derive(Subcommand)]
enum ArmfixCmd {
    /// Is the fix needed and installed?
    Status,
    /// Download DXVK and add its x64 dxgi.dll + d3d11.dll to the game folder
    /// (files already there are backed up).
    Apply {
        /// Windows on ARM: try it even though it's untested there (needs a
        /// working Vulkan driver).
        #[arg(long)]
        experimental: bool,
    },
    /// Take the fix out again and put back any files it replaced.
    Remove {
        /// Also remove DXVK files that someone put there by hand.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum SourceArg {
    All,
    Dma,
    Gb,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum SortArg {
    Relevance,
    Newest,
    Updated,
    Downloads,
    Likes,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum KindArg {
    Any,
    Song,
    Cover,
    Module,
    Ui,
    Plugin,
    Other,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum PinArg {
    First,
    Last,
    None,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum IfExistsArg {
    Replace,
    KeepBoth,
    Skip,
}

#[derive(Subcommand)]
enum PriorityCmd {
    /// Show the load order.
    Show,
    /// Move a mod up (higher priority).
    Up {
        r#mod: String,
        #[arg(default_value_t = 1)]
        steps: usize,
    },
    /// Move a mod down.
    Down {
        r#mod: String,
        #[arg(default_value_t = 1)]
        steps: usize,
    },
    /// Move a mod to the top (wins all conflicts).
    Top { r#mod: String },
    /// Move a mod to the bottom.
    Bottom { r#mod: String },
    /// Move a mod to a 1-based position.
    To { r#mod: String, position: usize },
    /// Add mods missing from the list (unlisted mods don't load).
    Sync {
        /// Add them at the bottom instead of the top.
        #[arg(long)]
        bottom: bool,
    },
    /// Remove entries for mod folders that no longer exist.
    Prune,
}

#[derive(Subcommand)]
enum BackupCmd {
    /// List backups, newest first.
    List,
    /// Back up the configs now.
    Create {
        #[arg(default_value = "manual backup")]
        reason: String,
    },
    /// Restore a backup (the current state is backed up first).
    Restore { id: String },
}

#[derive(Subcommand)]
enum LoadoutCmd {
    List,
    /// Save the current enabled mods and order.
    Save {
        name: String,
    },
    /// Apply a saved loadout.
    Apply {
        name: String,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum LoaderCmd {
    /// Turn mod loading on.
    Enable,
    /// Turn mod loading off (play without mods).
    Disable,
    /// Show or hide DivaModLoader's console window.
    Console {
        #[arg(value_parser = clap::builder::BoolishValueParser::new())]
        on: bool,
    },
}

fn main() -> ExitCode {
    // Quit quietly when piped into `head` and the like.
    #[cfg(unix)]
    // SAFETY: restoring the default signal action before any threads start.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let json = cli.global.json;
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            if json {
                println!("{}", json!({ "error": format!("{e:#}") }));
            } else {
                eprintln!("error: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn open_game(g: &Global) -> Result<Game> {
    let install = steam::find_game(g.game.as_deref())?;
    Game::open(&install.game_dir).with_context(|| format!("opening {}", install.game_dir.display()))
}

pub(crate) fn print_json<T: Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).expect("serialisable"));
}

fn warn_if_running(json: bool) {
    if !json && doctor::game_running() {
        eprintln!("note: the game is running; changes take effect next time it starts");
    }
}

fn report_save(r: &SaveReport, json: bool, what: impl Serialize) {
    if json {
        print_json(
            &json!({ "result": what, "written": r.written, "backup": r.backup.as_ref().map(|b| &b.id) }),
        );
    } else if let Some(b) = &r.backup {
        println!(
            "Saved ({} file(s)). Undo with `megamix undo` (backup {}).",
            r.written.len(),
            b.id
        );
    } else {
        println!("Nothing to change.");
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let g = &cli.global;
    let json = g.json;
    match cli.cmd {
        Cmd::Game => {
            let i = steam::find_game(g.game.as_deref())?;
            if json {
                print_json(&i);
            } else {
                println!("{}", i.game_dir.display());
            }
        }
        Cmd::Play => {
            if doctor::game_running() {
                bail!("the game is already running");
            }
            megamix_core::launch::start_game().context("couldn't ask Steam to start the game")?;
            if json {
                print_json(&json!({ "started": true }));
            } else {
                println!("Starting the game through Steam…");
            }
        }
        Cmd::Install {
            items,
            file,
            all_files,
            link,
            if_exists,
            bottom,
        } => {
            // Existing files are archives; anything else must be a mod reference.
            let mut files: Vec<PathBuf> = Vec::new();
            let mut refs = Vec::new();
            for it in &items {
                let p = PathBuf::from(it);
                if p.exists() {
                    files.push(p);
                } else if let Some(r) = remote::parse_ref(it) {
                    refs.push(r);
                } else if megamix_core::archive::has_archive_extension(&p) {
                    bail!("{it}: file not found");
                } else {
                    bail!("{}", megamix_core::Error::BadModRef(it.clone()));
                }
            }
            let mut game = open_game(g)?;
            let mods_dir = game.mods_dir()?;
            let if_exists = match if_exists {
                IfExistsArg::Replace => install::IfExists::Replace,
                IfExistsArg::KeepBoth => install::IfExists::KeepBoth,
                IfExistsArg::Skip => install::IfExists::Skip,
            };
            let placement = if bottom {
                NewModPlacement::Bottom
            } else {
                NewModPlacement::Top
            };
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let c = cancel.clone();
                let _ =
                    ctrlc::set_handler(move || c.store(true, std::sync::atomic::Ordering::Relaxed));
            }
            if doctor::game_running() && !json {
                eprintln!("note: the game is running; new mods load the next time it starts");
            }
            let mut results = Vec::new();
            for r in &refs {
                let pick = if let Some(n) = link {
                    remote_cmd::FilePick::Link(n)
                } else if all_files {
                    remote_cmd::FilePick::All
                } else if let Some(f) = file {
                    remote_cmd::FilePick::Id(f)
                } else {
                    remote_cmd::FilePick::Default
                };
                results.extend(remote_cmd::install_ref(
                    &mut game, *r, pick, if_exists, placement, &cancel, json,
                )?);
            }
            for file in &files {
                let plan = install::plan(&mods_dir, file)
                    .with_context(|| format!("reading {}", file.display()))?;
                if !json {
                    if plan.nested.is_some() {
                        println!(
                            "{}: contains another archive; unpacking it…",
                            file.display()
                        );
                    }
                    for m in &plan.mods {
                        println!(
                            "{}: {} ({} files, {}){}",
                            file.display(),
                            m.folder,
                            m.files,
                            human_size(m.size),
                            if m.exists {
                                " — already installed"
                            } else {
                                ""
                            }
                        );
                    }
                }
                let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
                let mut last_pct = u64::MAX;
                let installed = install::install(
                    &mods_dir,
                    &plan,
                    if_exists,
                    &mut |step| {
                        if json || !tty {
                            return;
                        }
                        if let install::Step::Unpacking(p) = step {
                            let pct = (p.done * 100).checked_div(p.total).unwrap_or(0);
                            if pct != last_pct {
                                last_pct = pct;
                                eprint!(
                                    "\r  unpacking {pct:>3}%  {} / {}   ",
                                    human_size(p.done),
                                    human_size(p.total)
                                );
                            }
                        }
                    },
                    &cancel,
                );
                if !json && tty && last_pct != u64::MAX {
                    eprintln!();
                }
                let installed =
                    installed.with_context(|| format!("installing {}", file.display()))?;
                if installed.is_empty() {
                    if !json {
                        println!("  skipped (already installed)");
                    }
                    continue;
                }
                let r = install::register(&mut game, &installed, file, placement)?;
                if !json {
                    for m in &installed {
                        let what = if m.replaced { "Updated" } else { "Installed" };
                        let kept = if m.kept_settings.is_empty() {
                            String::new()
                        } else {
                            format!(" (kept: {})", m.kept_settings.join(", "))
                        };
                        println!("  {what} {}{kept}", m.folder);
                    }
                    if let Some(b) = &r.backup {
                        println!(
                            "  Load order saved. Undo the load-order change with `megamix undo` (backup {}).",
                            b.id
                        );
                    }
                }
                results.push(json!({ "file": file, "installed": installed, "backup": r.backup.as_ref().map(|b| &b.id) }));
            }
            if json {
                print_json(&results);
            }
        }
        Cmd::Search {
            query,
            source,
            sort,
            kind,
            page,
        } => remote_cmd::search(&query, source, sort, kind, page, json)?,
        Cmd::Info { r#mod } => remote_cmd::info(&r#mod, json)?,
        Cmd::Link { r#mod, page, file } => {
            let game = open_game(g)?;
            remote_cmd::link(&game, &r#mod, &page, file, json)?;
        }
        Cmd::Unlink { r#mod } => {
            let game = open_game(g)?;
            let folder = game.mods[game.find(&r#mod)?].folder.clone();
            let removed = megamix_core::records::remove(&game.dir, &folder)?;
            if json {
                print_json(&json!({ "folder": folder, "unlinked": removed }));
            } else if removed {
                println!("{folder} is no longer linked to a mod page.");
            } else {
                println!("{folder} wasn't linked to a mod page.");
            }
        }
        Cmd::Sort { dry_run } => {
            let mut game = open_game(g)?;
            let plan = megamix_core::sort::plan(&game)?;
            if !json {
                for m in &plan.moved {
                    println!("{:>3} → {:<3} {}  ({})", m.from, m.to, m.name, m.reason);
                }
                for w in &plan.warnings {
                    println!("warning: {w}");
                }
            }
            if !plan.changed {
                if json {
                    print_json(&json!({ "changed": false, "order": plan.order }));
                } else {
                    println!("The load order is already sorted.");
                }
            } else if dry_run {
                if json {
                    print_json(&plan);
                } else {
                    println!("(dry run: nothing changed; run without --dry-run to apply)");
                }
            } else {
                warn_if_running(json);
                let r = megamix_core::sort::apply(&mut game, &plan)?;
                report_save(&r, json, &plan);
            }
        }
        Cmd::Pin { r#mod, place } => {
            let game = open_game(g)?;
            let folder = game.mods[game.find(&r#mod)?].folder.clone();
            let pin = match place {
                PinArg::First => megamix_core::sort::Pin::First,
                PinArg::Last => megamix_core::sort::Pin::Last,
                PinArg::None => megamix_core::sort::Pin::None,
            };
            megamix_core::sort::set_pin(&game.dir, &folder, pin)?;
            if json {
                print_json(&json!({ "folder": folder, "pin": pin }));
            } else {
                println!(
                    "{folder}: {}. Run `megamix sort` to apply.",
                    match pin {
                        megamix_core::sort::Pin::First => "kept at the top when sorting",
                        megamix_core::sort::Pin::Last => "kept at the bottom when sorting",
                        megamix_core::sort::Pin::None => "not pinned",
                    }
                );
            }
        }
        Cmd::Remove { mods } => {
            let mut game = open_game(g)?;
            warn_if_running(json);
            let r = megamix_core::remove::remove(&mut game, &mods)?;
            if json {
                print_json(&json!({ "removed": mods, "backup": r.backup.as_ref().map(|b| &b.id) }));
            } else {
                println!(
                    "Removed {}. It's kept for {} days: `megamix undo` brings it back.",
                    r.backup
                        .as_ref()
                        .map(|b| b.reason.trim_start_matches("remove ").to_owned())
                        .unwrap_or_default(),
                    megamix_core::remove::KEEP_DAYS
                );
            }
        }
        Cmd::Removed { purge } => {
            let game = open_game(g)?;
            let mods_dir = game.mods_dir()?;
            if purge {
                let freed = megamix_core::remove::purge(&mods_dir, None);
                if json {
                    print_json(&json!({ "freed": freed }));
                } else {
                    println!(
                        "Deleted removed mods for good ({} freed).",
                        human_size(freed)
                    );
                }
            } else {
                let kept = megamix_core::remove::kept(&mods_dir);
                if json {
                    print_json(&kept);
                } else if kept.is_empty() {
                    println!("No removed mods are kept.");
                } else {
                    for k in &kept {
                        println!(
                            "{}  {}  ({})",
                            k.id,
                            k.folders.join(", "),
                            human_size(k.size)
                        );
                    }
                    println!(
                        "They're deleted after {} days, or now with `megamix removed --purge`.",
                        megamix_core::remove::KEEP_DAYS
                    );
                }
            }
        }
        Cmd::Settings { r#mod } => {
            let game = open_game(g)?;
            let m = &game.mods[game.find(&r#mod)?];
            let cfg = m
                .config
                .as_ref()
                .with_context(|| format!("{} has no readable config.toml", m.folder))?;
            let list = megamix_core::modsettings::list(cfg);
            if json {
                print_json(&list);
            } else if list.is_empty() {
                println!("{} has no settings of its own.", m.folder);
            } else {
                for s in &list {
                    for c in s.comment.lines() {
                        println!("  # {c}");
                    }
                    let shown = if matches!(s.kind, megamix_core::modsettings::Kind::Text) {
                        format!("{:?}", s.value)
                    } else {
                        s.value.clone()
                    };
                    println!("{} = {shown}", s.path());
                }
            }
        }
        Cmd::Set { r#mod, key, value } => {
            let mut game = open_game(g)?;
            warn_if_running(json);
            let i = game.find(&r#mod)?;
            let folder = game.mods[i].folder.clone();
            let cfg = game.mods[i]
                .config
                .as_mut()
                .with_context(|| format!("{folder} has no readable config.toml"))?;
            megamix_core::modsettings::set(cfg, &key, &value)?;
            let r = game.save(&format!("set {key} of {folder}"))?;
            report_save(
                &r,
                json,
                json!({ "mod": folder, "key": key, "value": value }),
            );
        }
        Cmd::Updates => {
            let game = open_game(g)?;
            remote_cmd::updates(&game, json)?;
        }
        Cmd::Update { mods, all } => {
            let mut game = open_game(g)?;
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let c = cancel.clone();
                let _ =
                    ctrlc::set_handler(move || c.store(true, std::sync::atomic::Ordering::Relaxed));
            }
            if mods.is_empty() && !all {
                bail!("name the mods to update, or use --all");
            }
            remote_cmd::update(&mut game, &mods, &cancel, json)?;
        }
        Cmd::List => {
            let game = open_game(g)?;
            let infos = game.infos()?;
            if json {
                print_json(&infos);
            } else if infos.is_empty() {
                println!("No mods installed in {}.", game.mods_dir()?.display());
            } else {
                for i in &infos {
                    let state = match (&i.error, i.loaded, i.enabled) {
                        (Some(_), _, _) => "BROKEN",
                        (None, true, _) => "on",
                        (None, false, true) => "NOT LOADED",
                        (None, false, false) => "off",
                    };
                    let pos = i
                        .priority
                        .map(|p| format!("{p:>3}"))
                        .unwrap_or_else(|| "  -".into());
                    let ver = i
                        .version
                        .as_deref()
                        .map(|v| format!(" v{v}"))
                        .unwrap_or_default();
                    let name = if i.name != i.folder {
                        format!("{} [{}]", i.name, i.folder)
                    } else {
                        i.name.clone()
                    };
                    println!("{pos}  {state:<10} {name}{ver}");
                    if let Some(e) = &i.error {
                        println!("               {e}");
                    } else if i.enabled && !i.loaded {
                        println!("               not in priority; run `megamix priority sync`");
                    }
                }
                if !game.loader.enabled()? {
                    println!("\nMod loading is OFF in config.toml (`megamix loader enable`).");
                }
            }
        }
        Cmd::Enable { mods } | Cmd::Disable { mods } if mods.is_empty() => bail!("no mods given"),
        cmd @ (Cmd::Enable { .. } | Cmd::Disable { .. }) => {
            let (mods, on) = match cmd {
                Cmd::Enable { mods } => (mods, true),
                Cmd::Disable { mods } => (mods, false),
                _ => unreachable!(),
            };
            let mut game = open_game(g)?;
            let mut folders = Vec::new();
            for m in &mods {
                let i = game.find(m)?;
                folders.push(game.mods[i].folder.clone());
                game.set_enabled(m, on)?;
            }
            warn_if_running(json);
            let verb = if on { "enable" } else { "disable" };
            let r = game.save(&format!("{verb} {}", folders.join(", ")))?;
            report_save(&r, json, json!({ verb: folders }));
        }
        Cmd::Priority(p) => {
            let mut game = open_game(g)?;
            let (reason, result) = match p {
                PriorityCmd::Show => {
                    let order: Vec<_> = game.ordered()?.iter().map(|m| m.folder.clone()).collect();
                    if json {
                        print_json(
                            &json!({ "priority": game.priority()?, "effective_order": order }),
                        );
                    } else {
                        for (n, f) in order.iter().enumerate() {
                            println!("{:>3}  {f}", n + 1);
                        }
                    }
                    return Ok(ExitCode::SUCCESS);
                }
                PriorityCmd::Up { r#mod, steps } => mv(&mut game, &r#mod, Move::Up(steps))?,
                PriorityCmd::Down { r#mod, steps } => mv(&mut game, &r#mod, Move::Down(steps))?,
                PriorityCmd::Top { r#mod } => mv(&mut game, &r#mod, Move::Top)?,
                PriorityCmd::Bottom { r#mod } => mv(&mut game, &r#mod, Move::Bottom)?,
                PriorityCmd::To { r#mod, position } => {
                    if position == 0 {
                        bail!("positions start at 1");
                    }
                    mv(&mut game, &r#mod, Move::To(position))?
                }
                PriorityCmd::Sync { bottom } => {
                    let place = if bottom {
                        NewModPlacement::Bottom
                    } else {
                        NewModPlacement::Top
                    };
                    let added = game.sync_priority(place)?;
                    if !json && !added.is_empty() {
                        println!("Added: {}", added.join(", "));
                    }
                    ("sync priority".to_owned(), json!({ "added": added }))
                }
                PriorityCmd::Prune => {
                    let gone = game.prune_priority()?;
                    if !json && !gone.is_empty() {
                        println!("Removed: {}", gone.join(", "));
                    }
                    ("prune priority".to_owned(), json!({ "removed": gone }))
                }
            };
            warn_if_running(json);
            let r = game.save(&reason)?;
            report_save(&r, json, result);
        }
        Cmd::Conflicts { files } => {
            let game = open_game(g)?;
            let c = conflicts::find(&game)?;
            if json {
                print_json(&json!({ "conflicts": c, "pairs": conflicts::pairs(&c) }));
            } else if c.is_empty() {
                println!("No conflicts between loaded mods.");
            } else if files {
                for x in &c {
                    println!(
                        "{}\n    used: {}\n    ignored: {}",
                        x.path,
                        x.winner,
                        x.overridden.join(", ")
                    );
                }
            } else {
                for p in conflicts::pairs(&c) {
                    println!("{} overrides {} ({} file(s))", p.winner, p.loser, p.files);
                }
                println!(
                    "\n{} file(s) in total. `megamix conflicts --files` lists them.",
                    c.len()
                );
            }
        }
        Cmd::Doctor => {
            let report = doctor::run(g.game.as_deref());
            if json {
                print_json(&report);
            } else {
                for c in &report.checks {
                    let tag = match c.level {
                        Level::Ok => "  ok ",
                        Level::Info => " info",
                        Level::Warn => " WARN",
                        Level::Error => "ERROR",
                    };
                    println!("[{tag}] {}", c.summary);
                    for d in &c.details {
                        println!("        {d}");
                    }
                    if let Some(f) = &c.fix {
                        for (n, line) in f.lines().enumerate() {
                            println!("        {}{line}", if n == 0 { "fix: " } else { "" });
                        }
                    }
                }
            }
            if report.worst() == Level::Error {
                return Ok(ExitCode::FAILURE);
            }
        }
        Cmd::Backup(b) => {
            let mut game = open_game(g)?;
            match b {
                BackupCmd::List => {
                    let all = backup::list(&game.dir)?;
                    if json {
                        print_json(&all);
                    } else if all.is_empty() {
                        println!("No backups yet.");
                    } else {
                        println!("Backups in {}:", backup::backups_dir(&game.dir).display());
                        for b in all {
                            println!("  {}  {}", b.id, b.reason);
                        }
                    }
                }
                BackupCmd::Create { reason } => {
                    let b = backup::create(&game, &reason)?;
                    if json {
                        print_json(&b);
                    } else {
                        println!("Backed up {} file(s) to {}", b.files.len(), b.dir.display());
                    }
                }
                BackupCmd::Restore { id } => {
                    warn_if_running(json);
                    let r = backup::restore(&mut game, &id)?;
                    print_restore(&r, json);
                }
            }
        }
        Cmd::Undo => {
            let mut game = open_game(g)?;
            warn_if_running(json);
            match backup::undo(&mut game)? {
                Some(r) => print_restore(&r, json),
                None if json => print_json(&json!({ "result": null })),
                None => println!("Nothing to undo."),
            }
        }
        Cmd::Loadout(l) => {
            let mut game = open_game(g)?;
            match l {
                LoadoutCmd::List => {
                    let all = loadout::list(&game.dir)?;
                    if json {
                        print_json(&all);
                    } else if all.is_empty() {
                        println!("No loadouts. Save one with `megamix loadout save <name>`.");
                    } else {
                        for l in all {
                            println!(
                                "  {}  ({} of {} mods enabled)",
                                l.name,
                                l.enabled.len(),
                                l.order.len()
                            );
                        }
                    }
                }
                LoadoutCmd::Save { name } => {
                    let l = loadout::save(&game, &name)?;
                    if json {
                        print_json(&l);
                    } else {
                        println!(
                            "Saved loadout `{}` ({} mods enabled).",
                            l.name,
                            l.enabled.len()
                        );
                    }
                }
                LoadoutCmd::Apply { name } => {
                    let l = loadout::load(&game.dir, &name)?;
                    let rep = loadout::apply(&mut game, &l)?;
                    warn_if_running(json);
                    if !json {
                        if !rep.missing.is_empty() {
                            println!("Not installed anymore: {}", rep.missing.join(", "));
                        }
                        if !rep.not_in_loadout.is_empty() {
                            println!(
                                "Not in this loadout (disabled): {}",
                                rep.not_in_loadout.join(", ")
                            );
                        }
                    }
                    let r = game.save(&format!("apply loadout {name}"))?;
                    report_save(&r, json, &rep);
                }
                LoadoutCmd::Delete { name } => {
                    loadout::delete(&game.dir, &name)?;
                    if json {
                        print_json(&json!({ "deleted": name }));
                    } else {
                        println!("Deleted loadout `{name}`.");
                    }
                }
            }
        }
        Cmd::Armfix(a) => armfix_cmd(g, a, json)?,
        Cmd::SteamShortcut(c) => shortcut_cmd(c, json)?,
        Cmd::SelfUpdate { check } => self_update_cmd(check, json)?,
        Cmd::Loader(l) => {
            let mut game = open_game(g)?;
            let reason = match l {
                LoaderCmd::Enable => {
                    game.loader.set_enabled(true);
                    "enable mod loading"
                }
                LoaderCmd::Disable => {
                    game.loader.set_enabled(false);
                    "disable mod loading"
                }
                LoaderCmd::Console { on } => {
                    game.loader.set_console(on);
                    if on { "show console" } else { "hide console" }
                }
            };
            warn_if_running(json);
            let r = game.save(reason)?;
            report_save(&r, json, reason);
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn mv(game: &mut Game, m: &str, how: Move) -> Result<(String, serde_json::Value)> {
    let folder = game.mods[game.find(m)?].folder.clone();
    let pos = game.move_mod(m, how)?;
    Ok((
        format!("move {folder} to {pos}"),
        json!({ "mod": folder, "position": pos }),
    ))
}

fn print_restore(r: &backup::RestoreReport, json: bool) {
    if json {
        print_json(r);
        return;
    }
    println!(
        "Restored backup {} ({}): {} file(s) changed.",
        r.restored.id,
        r.restored.reason,
        r.written.len()
    );
    if !r.skipped.is_empty() {
        println!(
            "Skipped (mod no longer installed): {}",
            r.skipped.join(", ")
        );
    }
    println!(
        "The previous state was saved as backup {}.",
        r.safety_backup.id
    );
}

fn armfix_cmd(g: &Global, a: ArmfixCmd, json: bool) -> Result<()> {
    use megamix_core::armfix::{self, Need, State};
    // `status` still says whether this PC needs the fix without a game.
    if matches!(a, ArmfixCmd::Status)
        && armfix::need() == Need::NotNeeded
        && let Err(e) = open_game(g)
    {
        if json {
            print_json(&serde_json::json!({ "need": Need::NotNeeded, "state": null }));
        } else {
            println!("This PC: x86, the fix isn't needed");
            println!("(Game folder: {e})");
        }
        return Ok(());
    }
    let game = open_game(g)?;
    match a {
        ArmfixCmd::Status => {
            let st = armfix::status(&game);
            if json {
                print_json(&st);
                return Ok(());
            }
            println!(
                "This PC: {}",
                match st.need {
                    Need::Recommended =>
                        "ARM64 Linux, the fix is recommended for mods that hook rendering",
                    Need::Experimental => "Windows on ARM, the fix is untested here (experimental)",
                    Need::NotNeeded => "x86, the fix isn't needed",
                }
            );
            match &st.state {
                State::NotInstalled => {
                    println!("Game folder: no dxgi.dll / d3d11.dll (not installed)")
                }
                State::Installed { by_us: true } => {
                    println!(
                        "Game folder: DXVK {} x64 installed by the manager",
                        armfix::DXVK_VERSION
                    )
                }
                State::Installed { by_us: false } => println!(
                    "Game folder: DXVK {} x64 is there (put there by hand; left alone)",
                    armfix::DXVK_VERSION
                ),
                State::Other { files } => {
                    println!("Game folder: other DLLs: {} (left alone)", files.join(", "))
                }
            }
            if !st.affected_mods.is_empty() {
                println!(
                    "Enabled mods that need it on ARM64: {}",
                    st.affected_mods.join(", ")
                );
            }
            if st.need == Need::Recommended && st.state == State::NotInstalled {
                println!("Install it with: megamix armfix apply");
            }
        }
        ArmfixCmd::Apply { experimental } => {
            if doctor::game_running() {
                anyhow::bail!("close the game first (its DLLs are in use)");
            }
            if armfix::need() == Need::Experimental && experimental && !json {
                eprintln!(
                    "note: untested on Windows on ARM; needs a Vulkan driver. `megamix armfix remove` undoes it."
                );
            }
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let c = cancel.clone();
                let _ =
                    ctrlc::set_handler(move || c.store(true, std::sync::atomic::Ordering::Relaxed));
            }
            let client = megamix_core::net::Client::new();
            let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
            let mut last = 0;
            let r = armfix::apply(
                &client,
                &game,
                experimental,
                &mut |p| {
                    if tty && !json && p.total > 0 {
                        let pct = p.done * 100 / p.total;
                        if pct != last {
                            last = pct;
                            eprint!("\rDownloading DXVK {}… {pct}%", armfix::DXVK_VERSION);
                        }
                    }
                },
                &cancel,
            );
            if tty && !json {
                eprintln!();
            }
            let r = r?;
            if json {
                print_json(&r);
            } else if r.already {
                println!(
                    "DXVK {} x64 is already in the game folder; nothing to do.",
                    armfix::DXVK_VERSION
                );
            } else {
                println!(
                    "Added DXVK {} x64 dxgi.dll and d3d11.dll to the game folder.",
                    armfix::DXVK_VERSION
                );
                if !r.backed_up.is_empty() {
                    println!(
                        "Moved the old {} to .megamix/armfix/backup/ (`megamix armfix remove` puts them back).",
                        r.backed_up.join(" and ")
                    );
                }
            }
        }
        ArmfixCmd::Remove { force } => {
            if doctor::game_running() {
                anyhow::bail!("close the game first (its DLLs are in use)");
            }
            let r = armfix::remove(&game, force)?;
            if json {
                print_json(&r);
            } else if r.removed.is_empty() && r.restored.is_empty() {
                println!("Nothing to remove.");
            } else {
                if !r.removed.is_empty() {
                    println!("Removed {}.", r.removed.join(" and "));
                }
                if !r.restored.is_empty() {
                    println!("Put back the original {}.", r.restored.join(" and "));
                }
            }
        }
    }
    Ok(())
}

fn shortcut_cmd(c: ShortcutCmd, json: bool) -> Result<()> {
    use megamix_core::shortcut;
    let root = megamix_core::steam::steam_roots()
        .into_iter()
        .find(|r| r.join("userdata").is_dir())
        .context("Steam isn't installed for this user (no userdata folder found)")?;
    let close = match &c {
        ShortcutCmd::Add { close_steam, .. } | ShortcutCmd::Remove { close_steam } => *close_steam,
        ShortcutCmd::Status => false,
    };
    let restart = close && shortcut::steam_running();
    if restart {
        if !json {
            println!("Closing Steam (this can take a minute or two)…");
        }
        // Steam under x86 emulation on ARM (PB-OS) takes well over a minute.
        shortcut::close_steam(&root, std::time::Duration::from_secs(240))?;
    }
    let res = (|| -> Result<()> {
        match c {
            ShortcutCmd::Status => {
                let accs = shortcut::status(&root);
                if json {
                    print_json(&accs);
                } else if accs.is_empty() {
                    println!("Not in Steam. Add it with: megamix steam-shortcut add --close-steam");
                } else {
                    println!("In Steam for account(s): {}", accs.join(", "));
                }
            }
            ShortcutCmd::Add { exe, icon, art, .. } => {
                let exe = match exe {
                    Some(e) => e,
                    None => default_gui_exe()?,
                };
                let exe =
                    std::fs::canonicalize(&exe).with_context(|| format!("{}", exe.display()))?;
                let data = dirs_data();
                let icon = icon.or_else(|| {
                    data.as_ref()
                        .map(|d| d.join("icons/hicolor/256x256/apps/megamix-mod-manager.png"))
                        .filter(|p| p.is_file())
                });
                let art = art.or_else(|| {
                    data.as_ref()
                        .map(|d| d.join("megamix-mod-manager/steam"))
                        .filter(|p| p.is_dir())
                });
                let added = shortcut::add(&root, &exe, icon.as_deref(), art.as_deref())?;
                if json {
                    print_json(&added);
                } else {
                    for a in &added {
                        println!(
                            "{} the shortcut for Steam account {}{}.",
                            if a.updated { "Updated" } else { "Added" },
                            a.account,
                            if a.art > 0 {
                                " with library artwork"
                            } else {
                                ""
                            }
                        );
                    }
                    println!(
                        "It's in your Steam library as \"{}\" (also in Game Mode).",
                        shortcut::NAME
                    );
                }
            }
            ShortcutCmd::Remove { .. } => {
                let accs = shortcut::remove(&root)?;
                if json {
                    print_json(&accs);
                } else if accs.is_empty() {
                    println!("It wasn't in Steam.");
                } else {
                    println!("Removed from Steam account(s) {}.", accs.join(", "));
                }
            }
        }
        Ok(())
    })();
    if restart {
        if !json {
            println!("Starting Steam again…");
        }
        if let Err(e) = shortcut::start_steam(&root) {
            eprintln!("warning: {e}");
        }
    }
    res
}

fn dirs_data() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }
}

/// megamix-gui next to this program (or one folder up: the Windows
/// installer puts megamix.exe in a `bin` subfolder).
fn default_gui_exe() -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let name = if cfg!(windows) {
        "megamix-gui.exe"
    } else {
        "megamix-gui"
    };
    let dir = me.parent().context("no folder")?;
    [dir.join(name), dir.join("..").join(name)]
        .into_iter()
        .find(|p| p.is_file())
        .with_context(|| format!("{name} isn't next to megamix; pass --exe <path>"))
}

fn self_update_cmd(check_only: bool, json: bool) -> Result<()> {
    use megamix_core::selfupdate::{self, Method};
    let client = megamix_core::net::Client::new();
    let rel = selfupdate::check(&client, true)?;
    let Some(rel) = rel else {
        if json {
            print_json(&serde_json::json!({ "current": selfupdate::CURRENT, "update": null }));
        } else {
            println!("You have the newest version ({}).", selfupdate::CURRENT);
        }
        return Ok(());
    };
    if json && check_only {
        print_json(
            &serde_json::json!({ "current": selfupdate::CURRENT, "update": rel, "method": selfupdate::method() }),
        );
        return Ok(());
    }
    if !json {
        println!(
            "New version: {} (you have {}).",
            rel.version,
            selfupdate::CURRENT
        );
        println!("  {}", rel.page_url);
    }
    if check_only {
        return Ok(());
    }
    if selfupdate::method() == Method::Manual {
        anyhow::bail!(
            "this copy can't update itself (portable, Flatpak or a read-only folder); download it from {}",
            rel.page_url
        );
    }
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let c = cancel.clone();
        let _ = ctrlc::set_handler(move || c.store(true, std::sync::atomic::Ordering::Relaxed));
    }
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let mut last = 101;
    let r = selfupdate::install(
        &client,
        &rel,
        &mut |p| {
            if tty && !json && p.total > 0 {
                let pct = p.done * 100 / p.total;
                if pct != last {
                    last = pct;
                    eprint!("\rDownloading… {pct}%");
                }
            }
        },
        &cancel,
    );
    if tty && !json {
        eprintln!();
    }
    let r = r?;
    if json {
        print_json(&r);
    } else if r.installer_started {
        println!("The installer is running; it finishes in the background.");
    } else {
        println!(
            "Updated to {}. Restart the app to use the new version.",
            rel.version
        );
    }
    Ok(())
}
