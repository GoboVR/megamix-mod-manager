//! Installing and updating mods from DivaModArchive and GameBanana:
//! download (see [`crate::download`]) → plan → install → register, and
//! update checks for mods installed that way.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use crate::download::{self, Progress};
use crate::error::{Error, Result};
use crate::install::{self, IfExists, Installed};
use crate::mods::{Game, NewModPlacement, SaveReport};
use crate::net::Client;
use crate::records::{self, RemoteFileInfo, Source};
use crate::remote::{self, ModRef, RemoteFile, RemoteMod, Site};

/// Progress of [`install_remote`].
#[derive(Debug, Clone)]
pub enum Step<'a> {
    Downloading(&'a Progress),
    Installing(install::Step<'a>),
}

#[derive(Debug, Serialize)]
pub struct RemoteInstalled {
    pub installed: Vec<Installed>,
    /// Backup id of the load order change.
    pub backup: Option<String>,
}

/// The file to install for a reference: the one it names, or the only
/// file. `None` when the mod has several and the user must choose.
/// With no files on the site, a single directly downloadable external link
/// is used.
pub fn pick_file(m: &RemoteMod, r: &ModRef) -> Result<Option<RemoteFile>> {
    if let Some(f) = r.file
        && f != remote::EXTERNAL_FILE_ID
    {
        return m
            .files
            .iter()
            .find(|x| x.id == f)
            .cloned()
            .map(Some)
            .ok_or_else(|| Error::Other(format!("“{}” has no file {f} (removed?)", m.name)));
    }
    let external: Vec<RemoteFile> = m.external.iter().filter_map(|l| l.as_file(m)).collect();
    if m.files.is_empty() && external.is_empty() {
        return Err(Error::Other(match m.external.first() {
            Some(l) => format!(
                "“{}” is hosted on {}, which can't be downloaded automatically; open {} in a browser, then install the downloaded file",
                m.name,
                l.host(),
                l.url
            ),
            None => format!("“{}” has no files to download", m.name),
        }));
    }
    Ok(match (m.files.len(), external.len()) {
        (1, _) if r.file.is_none() || external.is_empty() => Some(m.files[0].clone()),
        (0, 1) => external.into_iter().next(),
        _ => None,
    })
}

pub fn source_of(m: &RemoteMod) -> Source {
    match m.site {
        Site::Dma => Source::DivaModArchive { id: m.id },
        Site::GameBanana => Source::GameBanana { id: m.id },
    }
}

/// Download one file of a mod into the mods folder's download area.
pub fn download_file(
    client: &Client,
    mods_dir: &Path,
    m: &RemoteMod,
    file: &RemoteFile,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<download::Download> {
    let job = format!("{}-{}-{}", m.site.short(), m.id, file.id);
    // Unpacked size is unknown until the archive is here; mod archives
    // (audio, video, textures) rarely shrink much, so plan for 1.5×.
    let room = file.size / 2 * 3;
    download::download(client, mods_dir, &job, file, room, progress, cancel)
}

/// Mods in `plan` that would replace a folder that did NOT come from this
/// mod page (installed by hand, or from another mod). Ask before replacing.
pub fn foreign_conflicts(game_dir: &Path, plan: &install::Plan, m: &RemoteMod) -> Vec<String> {
    let recs = records::load(game_dir);
    let src = source_of(m);
    plan.mods
        .iter()
        .filter(|p| p.exists)
        .filter(|p| recs.get(&p.folder).is_none_or(|r| r.source != src))
        .map(|p| p.folder.clone())
        .collect()
}

/// After [`install::install`]: add the mods to the load order and record
/// that they came from this mod page and file.
pub fn register_remote(
    game: &mut Game,
    installed: &[Installed],
    m: &RemoteMod,
    file: &RemoteFile,
    placement: NewModPlacement,
) -> Result<SaveReport> {
    let external = file.id == remote::EXTERNAL_FILE_ID;
    let info = RemoteFileInfo {
        id: file.id,
        name: file.name.clone(),
        size: file.size,
        updated: match m.site {
            Site::Dma => m.updated,
            Site::GameBanana if external => m.updated,
            Site::GameBanana => file.added,
        },
        external,
    };
    install::register_with(
        game,
        installed,
        source_of(m),
        Some(info),
        m.version.clone(),
        placement,
    )
}

/// Download one file of a mod and install it (CLI).
#[allow(clippy::too_many_arguments)]
pub fn install_remote(
    client: &Client,
    game: &mut Game,
    m: &RemoteMod,
    file: &RemoteFile,
    if_exists: IfExists,
    placement: NewModPlacement,
    update_folder: Option<&str>,
    progress: &mut dyn FnMut(Step),
    cancel: &AtomicBool,
) -> Result<RemoteInstalled> {
    let mods_dir = game.mods_dir()?;
    let dl = download_file(
        client,
        &mods_dir,
        m,
        file,
        &mut |p| progress(Step::Downloading(p)),
        cancel,
    )?;
    let mut plan = install::plan(&mods_dir, &dl.path)?;
    if let Some(folder) = update_folder {
        retarget_update(&mut plan, folder);
    }
    let installed = install::install(
        &mods_dir,
        &plan,
        if_exists,
        &mut |s| progress(Step::Installing(s)),
        cancel,
    )?;
    drop(dl); // delete the download (also on the error paths above)
    if installed.is_empty() {
        return Ok(RemoteInstalled {
            installed,
            backup: None,
        });
    }
    let r = register_remote(game, &installed, m, file, placement)?;
    Ok(RemoteInstalled {
        installed,
        backup: r.backup.map(|b| b.id),
    })
}

/// For an update: install into the mod's current folder even if the new
/// archive names its folder differently (only when it holds one mod).
pub fn retarget_update(plan: &mut install::Plan, folder: &str) {
    if plan.mods.len() == 1 {
        plan.mods[0].folder = folder.to_owned();
        plan.mods[0].exists = true;
    }
}

/// Link a mod installed by hand to its page, so it gets update checks.
/// The installed copy is taken to be the page's current version; `file`
/// says which download it is (default: the newest/only file).
pub fn link_mod(
    game_dir: &Path,
    folder: &str,
    m: &RemoteMod,
    file: Option<&RemoteFile>,
) -> Result<()> {
    let file = match file {
        Some(f) => Some(f.clone()),
        None => m
            .files
            .first()
            .cloned()
            .or_else(|| m.external.iter().find_map(|l| l.as_file(m))),
    };
    let external = file
        .as_ref()
        .is_some_and(|f| f.id == remote::EXTERNAL_FILE_ID);
    let info = file.map(|f| RemoteFileInfo {
        id: f.id,
        name: f.name.clone(),
        size: f.size,
        updated: match m.site {
            Site::GameBanana if !external => f.added.max(1),
            _ => m.updated,
        },
        external,
    });
    let old = records::load(game_dir).remove(folder);
    let rec = records::Record {
        source: source_of(m),
        installed_at: old
            .as_ref()
            .map(|r| r.installed_at.clone())
            .unwrap_or_else(|| chrono::Local::now().to_rfc3339()),
        version: old.and_then(|r| r.version).or_else(|| m.version.clone()),
        file: info,
    };
    records::set(game_dir, [(folder.to_owned(), rec)])
}

/// A newer file for an installed mod.
#[derive(Debug, Clone, Serialize)]
pub struct Update {
    /// Installed mod folder.
    pub folder: String,
    pub remote: ModRef,
    pub name: String,
    pub installed_version: Option<String>,
    pub new_version: Option<String>,
    /// The file to install, if it's clear which one (else the user picks).
    pub file: Option<RemoteFile>,
    pub updated: i64,
}

/// Results of [`check_updates`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateCheck {
    pub updates: Vec<Update>,
    /// Mods that couldn't be checked: (folder, error).
    pub errors: Vec<(String, String)>,
    /// Mods installed from a site that were checked.
    pub checked: usize,
}

/// Where an installed mod came from, as a reference (site installs only).
pub fn installed_ref(rec: &records::Record) -> Option<ModRef> {
    match rec.source {
        Source::DivaModArchive { id } => Some(ModRef {
            site: Site::Dma,
            id,
            file: None,
        }),
        Source::GameBanana { id } => Some(ModRef {
            site: Site::GameBanana,
            id,
            file: None,
        }),
        Source::File { .. } => None,
    }
}

/// Compare one installed mod with its page.
pub fn update_for(folder: &str, rec: &records::Record, m: &RemoteMod) -> Option<Update> {
    let r = installed_ref(rec)?;
    let info = rec.file.as_ref()?;
    let file = match m.site {
        Site::Dma => {
            if m.updated <= info.updated {
                return None;
            }
            // Same slot if it still exists, else the only file.
            m.files
                .iter()
                .find(|f| f.id == info.id && m.files.len() > 1)
                .or_else(|| (m.files.len() == 1).then(|| &m.files[0]))
                .cloned()
        }
        Site::GameBanana if info.external => {
            // Installed from an external link: the page's update date is
            // all there is to go on.
            if m.updated <= info.updated {
                return None;
            }
            let direct: Vec<_> = m.external.iter().filter_map(|l| l.as_file(m)).collect();
            (m.files.is_empty() && direct.len() == 1).then(|| direct[0].clone())
        }
        Site::GameBanana => {
            let newest = m.files.iter().max_by_key(|f| f.added)?;
            if newest.added <= info.updated || newest.id == info.id {
                return None;
            }
            (m.files.len() == 1 || newest.name == info.name).then(|| newest.clone())
        }
    };
    Some(Update {
        folder: folder.to_owned(),
        remote: r,
        name: m.name.clone(),
        installed_version: rec.version.clone(),
        new_version: m.version.clone(),
        file,
        updated: m.updated,
    })
}

/// Check every mod installed from a site. Requests go out one at a time
/// (paced by the client) and are cached briefly, so this is polite and
/// can run in the background. `only`: limit to these folders.
pub fn check_updates(
    client: &Client,
    game_dir: &Path,
    only: Option<&[String]>,
    cancel: &AtomicBool,
) -> UpdateCheck {
    let mut out = UpdateCheck::default();
    let mods_dir = Game::open(game_dir).ok().and_then(|g| g.mods_dir().ok());
    for (folder, rec) in records::load(game_dir) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if only.is_some_and(|o| !o.contains(&folder)) {
            continue;
        }
        let Some(r) = installed_ref(&rec) else {
            continue;
        };
        if mods_dir.as_ref().is_some_and(|d| !d.join(&folder).is_dir()) {
            continue; // removed by hand
        }
        out.checked += 1;
        match remote::details(client, r, false) {
            Ok(f) => out.updates.extend(update_for(&folder, &rec, &f.value)),
            Err(e) => out.errors.push((folder, e.to_string())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::Record;
    use crate::remote::ModKind;

    fn rm(site: Site, updated: i64, files: Vec<RemoteFile>) -> RemoteMod {
        RemoteMod {
            site,
            id: 5,
            name: "M".into(),
            authors: vec![],
            category: String::new(),
            kind: ModKind::Song,
            description: String::new(),
            images: vec![],
            thumbnail: None,
            files,
            downloads: None,
            likes: 0,
            updated,
            version: Some("2".into()),
            page_url: String::new(),
            explicit: false,
            details_loaded: true,
            also_on: None,
            external: vec![],
            requires: vec![],
            notices: None,
        }
    }

    fn f(id: u64, name: &str, added: i64) -> RemoteFile {
        RemoteFile {
            id,
            name: name.into(),
            size: 1,
            url: String::new(),
            md5: None,
            added,
        }
    }

    fn rec(source: Source, id: u64, name: &str, updated: i64) -> Record {
        Record {
            source,
            installed_at: String::new(),
            version: Some("1".into()),
            file: Some(RemoteFileInfo {
                id,
                name: name.into(),
                size: 1,
                updated,
                external: false,
            }),
        }
    }

    #[test]
    fn dma_updates_by_post_time() {
        let r = rec(Source::DivaModArchive { id: 5 }, 0, "a.zip", 100);
        assert!(update_for("A", &r, &rm(Site::Dma, 100, vec![f(0, "a.zip", 100)])).is_none());
        let u = update_for("A", &r, &rm(Site::Dma, 200, vec![f(0, "a.zip", 200)])).unwrap();
        assert_eq!(u.file.unwrap().id, 0);
        // Several files: the same slot.
        let u = update_for(
            "A",
            &r,
            &rm(Site::Dma, 200, vec![f(0, "a.zip", 200), f(1, "b.zip", 200)]),
        )
        .unwrap();
        assert_eq!(u.file.unwrap().id, 0);
    }

    #[test]
    fn external_installs_update_by_page_date() {
        let mut r = rec(
            Source::GameBanana { id: 5 },
            remote::EXTERNAL_FILE_ID,
            "M.zip",
            100,
        );
        r.file.as_mut().unwrap().external = true;
        let mut m = rm(Site::GameBanana, 100, vec![]);
        m.external = vec![remote::ExternalLink::new(
            "https://drive.google.com/file/d/X/view",
            "pack",
        )];
        assert!(update_for("A", &r, &m).is_none());
        m.updated = 200;
        let u = update_for("A", &r, &m).unwrap();
        assert_eq!(u.file.unwrap().id, remote::EXTERNAL_FILE_ID);
    }

    #[test]
    fn linking_records_the_current_version() {
        let t = crate::mods::tests::fake_game(&[], &[("Hand Made", "enabled = true\n")]);
        let m = rm(Site::GameBanana, 300, vec![f(11, "mod.zip", 250)]);
        link_mod(t.path(), "Hand Made", &m, None).unwrap();
        let recs = records::load(t.path());
        let r = &recs["Hand Made"];
        assert_eq!(r.source, Source::GameBanana { id: 5 });
        assert!(update_for("Hand Made", r, &m).is_none());
        let newer = rm(
            Site::GameBanana,
            400,
            vec![f(12, "mod.zip", 400), f(11, "mod.zip", 250)],
        );
        assert!(update_for("Hand Made", r, &newer).is_some());
        assert!(records::remove(t.path(), "Hand Made").unwrap());
        assert!(records::load(t.path()).is_empty());
    }

    #[test]
    fn gb_updates_by_newer_file() {
        let r = rec(Source::GameBanana { id: 5 }, 10, "mod.zip", 100);
        assert!(
            update_for(
                "A",
                &r,
                &rm(Site::GameBanana, 100, vec![f(10, "mod.zip", 100)])
            )
            .is_none()
        );
        let u = update_for(
            "A",
            &r,
            &rm(
                Site::GameBanana,
                300,
                vec![f(11, "mod.zip", 300), f(10, "mod.zip", 100)],
            ),
        )
        .unwrap();
        assert_eq!(u.file.unwrap().id, 11);
        // A different-named newer file next to the old one: user picks.
        let u = update_for(
            "A",
            &r,
            &rm(
                Site::GameBanana,
                300,
                vec![f(12, "addon.zip", 300), f(10, "mod.zip", 100)],
            ),
        )
        .unwrap();
        assert!(u.file.is_none());
        // File installs never have updates.
        let r = Record {
            source: Source::File {
                file_name: "x".into(),
                size: 1,
            },
            ..r
        };
        assert!(
            update_for(
                "A",
                &r,
                &rm(Site::GameBanana, 300, vec![f(11, "mod.zip", 300)])
            )
            .is_none()
        );
    }
}
