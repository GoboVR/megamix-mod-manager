//! Core library for the Project DIVA Mega Mix+ mod manager.
//!
//! Shared by the `megamix` CLI and the GUI.

pub mod archive;
pub mod armfix;
pub mod backup;
pub mod binvdf;
pub mod compat;
pub mod config;
pub mod conflicts;
pub mod dma;
pub mod doctor;
pub mod download;
pub mod error;
pub mod gamebanana;
pub mod get;
pub mod install;
pub mod launch;
pub mod loadout;
pub mod mods;
pub mod modsettings;
pub mod net;
pub mod records;
pub mod remote;
pub mod remove;
pub mod selfupdate;
pub mod settings;
pub mod shortcut;
pub mod sort;
pub mod steam;
pub mod vdf;

pub use error::{Error, Result};
