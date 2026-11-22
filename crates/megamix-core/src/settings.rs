//! User settings shared by the CLI and GUI, stored as JSON in the user
//! config folder (`~/.config/megamix/settings.json`,
//! `%APPDATA%\megamix\settings.json`).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Manually chosen game folder; `None` = detect through Steam.
    pub game_dir: Option<PathBuf>,
    /// GUI scale factor override; `None` = system default.
    pub ui_scale: Option<f32>,
    /// GUI colours: "dark", "light", or `None` = follow the system
    /// (dark under Steam Game Mode, which has no system theme).
    pub theme: Option<String>,
    /// Folder the GUI's install browser was last in.
    pub last_install_dir: Option<PathBuf>,
    /// Which copy to show when a mod is on both sites; `None` = DivaModArchive.
    pub prefer_site: Option<crate::remote::Site>,
    /// Check GitHub for new versions of the app; `None` = yes.
    #[serde(default)]
    pub app_updates: Option<bool>,
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("megamix").join("settings.json"))
}

impl Settings {
    /// Load settings; missing or unreadable files give defaults.
    pub fn load() -> Self {
        path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let p = path().ok_or_else(|| Error::Other("no config folder on this system".into()))?;
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        atomic_write(&p, &serde_json::to_vec_pretty(self).expect("serialisable"))
    }
}
