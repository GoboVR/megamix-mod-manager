//! One-click install links (`divamodmanager:…`).
//!
//! - Only one window: the running app listens on a localhost port (written
//!   to a file in the runtime/cache folder). A second start with a link
//!   hands the link over and quits. Received links only ever open a
//!   "Install …?" question, never install by themselves.
//! - Registering this app as the handler for `divamodmanager:` links
//!   (Linux: a hidden .desktop file + xdg-mime; Windows: HKCU\Software\Classes).

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use slint::Weak;

use crate::AppWindow;

pub const SCHEME: &str = "divamodmanager";

/// The port this process listens on (to only remove our own port file).
static OUR_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

fn port_file() -> Option<PathBuf> {
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .map(|d| d.join("megamix").join("gui.port"))
}

/// Hand `link` to an already running window. True if it took it.
pub fn forward(link: &str) -> bool {
    let Some(port) = port_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u16>().ok())
    else {
        return false;
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if writeln!(s, "megamix-link {}", link.trim()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Accept links from later starts; `on_link` runs on the UI thread.
pub fn listen(weak: Weak<AppWindow>, on_link: fn(&AppWindow, String)) {
    let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) else {
        return;
    };
    let Ok(addr) = l.local_addr() else { return };
    if let Some(p) = port_file() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, addr.port().to_string());
    }
    let _ = OUR_PORT.set(addr.port());
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            let read = BufReader::new(&s).take_line(&mut line);
            let Some(link) = read
                .then(|| line.trim().strip_prefix("megamix-link ").map(str::to_owned))
                .flatten()
            else {
                continue;
            };
            if megamix_core::remote::parse_ref(&link).is_none() {
                let _ = writeln!(s, "bad");
                continue;
            }
            let _ = writeln!(s, "ok");
            let _ = weak.upgrade_in_event_loop(move |ui| on_link(&ui, link));
        }
    });
}

/// Remove the port file when quitting (only if it is ours).
pub fn stop_listening() {
    let Some(ours) = OUR_PORT.get() else { return };
    if let Some(p) = port_file()
        && std::fs::read_to_string(&p).is_ok_and(|s| s.trim() == ours.to_string())
    {
        let _ = std::fs::remove_file(p);
    }
}

trait TakeLine {
    fn take_line(self, out: &mut String) -> bool;
}

impl<R: std::io::Read> TakeLine for BufReader<R> {
    /// Read one line of at most 4 KiB.
    fn take_line(self, out: &mut String) -> bool {
        use std::io::Read;
        let mut limited = BufReader::new(self.take(4096));
        limited.read_line(out).is_ok()
    }
}

/// The program now opening `divamodmanager:` links ("" = none), for display.
pub fn current_handler() -> String {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        hkcu.open_subkey(format!("Software\\Classes\\{SCHEME}\\shell\\open\\command"))
            .and_then(|k| k.get_value::<String, _>(""))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-mime")
            .args(["query", "default", &format!("x-scheme-handler/{SCHEME}")])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    }
}

/// Is this program the handler?
pub fn is_handler() -> bool {
    let h = current_handler();
    #[cfg(windows)]
    {
        std::env::current_exe()
            .map(|e| {
                h.to_ascii_lowercase()
                    .contains(&e.to_string_lossy().to_ascii_lowercase())
            })
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        // install.sh's app menu entry handles links too.
        h == DESKTOP_FILE || h == "megamix-mod-manager.desktop"
    }
}

#[cfg(not(windows))]
const DESKTOP_FILE: &str = "megamix-mod-manager-links.desktop";

/// Make this program open `divamodmanager:` links (for this user only).
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(format!("Software\\Classes\\{SCHEME}"))
            .map_err(|e| e.to_string())?;
        key.set_value(
            "",
            &"URL:Mega Mix+ Mod Manager one-click install".to_string(),
        )
        .and_then(|_| key.set_value("URL Protocol", &String::new()))
        .map_err(|e| e.to_string())?;
        let (cmd, _) = key
            .create_subkey("shell\\open\\command")
            .map_err(|e| e.to_string())?;
        cmd.set_value("", &format!("\"{}\" \"%1\"", exe.display()))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if std::path::Path::new("/.flatpak-info").exists() {
            return Err("in the Flatpak version, links are set up by the package".into());
        }
        let apps = dirs::data_dir()
            .ok_or("no data folder")?
            .join("applications");
        std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
        // Desktop Entry spec: quote the path, escape `"`, `` ` ``, `$`, `\`.
        let exe_s = exe.to_string_lossy();
        let mut quoted = String::from("\"");
        for c in exe_s.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Mega Mix+ Mod Manager (one-click install)\nExec={quoted} %u\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/{SCHEME};\n"
        );
        let path = apps.join(DESKTOP_FILE);
        std::fs::write(&path, entry).map_err(|e| format!("{}: {e}", path.display()))?;
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps)
            .status();
        let ok = std::process::Command::new("xdg-mime")
            .args([
                "default",
                DESKTOP_FILE,
                &format!("x-scheme-handler/{SCHEME}"),
            ])
            .status()
            .map_err(|e| format!("couldn't run xdg-mime: {e}"))?;
        if !ok.success() {
            return Err("xdg-mime failed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_valid_links() {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in l.incoming().take(2) {
                let mut s = stream.unwrap();
                let mut line = String::new();
                BufReader::new(&s).take_line(&mut line);
                let link = line
                    .trim()
                    .strip_prefix("megamix-link ")
                    .unwrap_or("")
                    .to_owned();
                let ok = megamix_core::remote::parse_ref(&link).is_some();
                let _ = writeln!(s, "{}", if ok { "ok" } else { "bad" });
            }
        });
        let send = |link: &str| {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            writeln!(s, "megamix-link {link}").unwrap();
            let mut r = String::new();
            BufReader::new(s).read_line(&mut r).unwrap();
            r.trim().to_owned()
        };
        assert_eq!(send("divamodmanager:dma/243"), "ok");
        assert_eq!(send("rm -rf /"), "bad");
    }
}
