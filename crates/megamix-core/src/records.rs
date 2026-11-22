//! Where each installed mod came from (for updates and modpacks).
//! Stored in `<game>/.megamix/installed.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::error::{Error, Result};
use crate::mods::DATA_DIR;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// Installed from a local archive.
    File {
        file_name: String,
        size: u64,
    },
    DivaModArchive {
        id: u64,
    },
    GameBanana {
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub source: Source,
    /// RFC 3339.
    pub installed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Downloaded file (DivaModArchive / GameBanana installs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<RemoteFileInfo>,
}

/// Which file of a mod page was installed, to spot updates later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFileInfo {
    /// GameBanana file id / DivaModArchive file index.
    pub id: u64,
    pub name: String,
    pub size: u64,
    /// Unix time the site last updated the mod (DMA) or added the file (GB).
    pub updated: i64,
    /// Downloaded from an external link (Google Drive …), not the site.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
}

fn path(game_dir: &Path) -> PathBuf {
    game_dir.join(DATA_DIR).join("installed.json")
}

/// All records, by mod folder. Missing or unreadable file = none.
pub fn load(game_dir: &Path) -> BTreeMap<String, Record> {
    fs::read(path(game_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Add or replace records.
pub fn set(game_dir: &Path, records: impl IntoIterator<Item = (String, Record)>) -> Result<()> {
    let mut all = load(game_dir);
    all.extend(records);
    let p = path(game_dir);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    atomic_write(&p, &serde_json::to_vec_pretty(&all).expect("serialisable"))
}

/// Forget where a mod came from.
pub fn remove(game_dir: &Path, folder: &str) -> Result<bool> {
    let mut all = load(game_dir);
    if all.remove(folder).is_none() {
        return Ok(false);
    }
    let p = path(game_dir);
    atomic_write(&p, &serde_json::to_vec_pretty(&all).expect("serialisable"))?;
    Ok(true)
}
