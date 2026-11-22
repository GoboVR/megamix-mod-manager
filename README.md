<p align="center">
  <img src="assets/icon-256.png" alt="Mega Mix+ Mod Manager icon" width="160">
</p>

<h1 align="center">Mega Mix+ Mod Manager</h1>

[![CI](https://img.shields.io/github/actions/workflow/status/GoboVR/megamix-mod-manager/ci.yml?branch=main&label=CI&logo=github)](https://github.com/GoboVR/megamix-mod-manager/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/GoboVR/megamix-mod-manager?label=release)](https://github.com/GoboVR/megamix-mod-manager/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/GoboVR/megamix-mod-manager/total?label=downloads)](https://github.com/GoboVR/megamix-mod-manager/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[![Linux: x86_64 | ARM64](https://img.shields.io/badge/Linux-x86__64%20%7C%20ARM64-FCC624?logo=linux&logoColor=black)](#linux-any-distro-x86_64-and-arm64)
[![Windows: x64 | ARM64](https://img.shields.io/badge/Windows-x64%20%7C%20ARM64-0078D4)](#windows-x64-and-arm64)
[![Steam Deck and handhelds](https://img.shields.io/badge/Steam%20Deck%20%26%20handhelds-controller%20ready-1A9FFF?logo=steam&logoColor=white)](#linux-any-distro-x86_64-and-arm64)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-CE422B?logo=rust)](#building-from-source)
[![Made with Slint](https://img.shields.io/badge/UI-Slint-2379F4)](https://slint.dev)
[![AI-assisted](https://img.shields.io/badge/AI--assisted-Claude-D97757)](#ai-disclosure)

A mod manager for **Hatsune Miku: Project DIVA Mega Mix+** on PC, for mods that use
[DivaModLoader](https://github.com/blueskythlikesclouds/DivaModLoader). It works on
**Linux** (x86_64 and ARM64, including Steam Deck, SteamOS and handhelds like the AYN Thor
running PB-OS) and **Windows** (x64 and ARM64). It comes with a window you can use with a
mouse, touch or a controller, and a command line tool called `megamix`.

- Turn mods on and off and change their load order. You can also let **Auto-sort** order them for you.
- Install mods from **DivaModArchive** (preferred) and **GameBanana**, from one-click install
  buttons on those sites, or from `.zip`, `.7z` and `.rar` files.
- Big song packs install safely. Downloads go to the game's drive (never to RAM-backed `/tmp`),
  resume after a dropped connection, and are checked before they're unpacked.
- Update checks for mods installed from the sites, plus **Update all**.
- Before you install, it warns about **missing requirements** (for example ExPatch or New Classics)
  and **conflicts** with mods you already have, using the same notices DivaModArchive shows.
- **Conflicts** page: which files more than one mod replaces, and which mod wins.
- **Loadouts**: save and switch between sets of mods.
- Edit a mod's own settings, remove mods (they're kept for 30 days so you can undo), and
  **undo** any change. The config files are backed up before every change.
- **Doctor**: checks that DivaModLoader is installed and the configs are fine. On Linux it also
  checks the Steam launch options mods need.
- A **Play** button that starts the game through Steam and brings the manager back when you quit.
- **Updates itself**: when a new version is out, an **Update app** button appears. It downloads
  the new version from this repo's releases, checks it, and restarts (turn it off under Settings).

## Install

Downloads are on the [Releases page](https://github.com/GoboVR/megamix-mod-manager/releases/latest).

### Windows (x64 and ARM64)

1. Download the installer for your PC:
   - `MegaMixModManager-…-Setup-x64.exe` for most PCs (Intel / AMD)
   - `MegaMixModManager-…-Setup-arm64.exe` for Windows on ARM (Snapdragon laptops and similar)

   Not sure? Open **Settings → System → About** and look at "System type".
2. Run it. It installs for your user only, so it doesn't need admin rights. Windows may show
   "Windows protected your PC" because the installer isn't signed. Click **More info → Run anyway**.
3. The installer:
   - adds **Mega Mix+ Mod Manager** to the Start menu
   - lets you type `megamix` in any **new** Command Prompt or PowerShell window
   - makes the one-click install buttons on DivaModArchive and GameBanana open this app.
     You can untick that if you use another mod manager.
   - can add it to Steam for Big Picture mode (off by default)

To remove it: **Settings → Apps → Installed apps → Mega Mix+ Mod Manager → Uninstall**. Your
mods are not touched.

### Linux (any distro, x86_64 and ARM64)

Open a terminal and run:

```sh
curl -fsSL https://github.com/GoboVR/megamix-mod-manager/releases/latest/download/install.sh | sh
```

Or download `megamix-linux-x86_64.tar.gz` (or `-aarch64` for ARM), unpack it and run
`sh install.sh` inside the folder.

The script installs everything into your home folder (`~/.local/bin`), so it also works on
read-only systems like SteamOS and PB-OS:
- it detects your distro, and if a library the window needs is missing it offers to install it
  (pacman, apt, dnf or zypper; never on read-only systems)
- it adds **Mega Mix+ Mod Manager** to your app menu, with an icon
- it adds `~/.local/bin` to your PATH if needed (bash, zsh and fish), so `megamix` works in a terminal
- it makes one-click install links open the app (it asks first if another program handles them)
- it offers to **add the app to Steam** as a non-Steam game, with library artwork, so it shows up
  in **Game Mode** (Steam closes and starts again for this)
- it finishes by running `megamix doctor` to check your game

Run the same command again to update (or use **Update app** inside it).

To remove it, run `sh ~/.local/share/megamix-mod-manager/uninstall.sh` (or `sh install.sh --uninstall`).
It also offers to take it out of Steam. Add `--purge` to delete its settings and download cache
too. Your game, mods, mod backups and loadouts are never touched.

**Steam launch options (Linux / Steam Deck / Proton):** DivaModLoader only loads if the game's
launch options in Steam contain:

```
WINEDLLOVERRIDES="dinput8.dll=n,b" %command%
```

(Right-click the game in Steam → Properties → Launch Options.) `megamix doctor` and the Doctor
page check this and show the exact line to use.

**Steam Deck / Game Mode:** say yes when `install.sh` asks to add it to Steam (or run
`megamix steam-shortcut add --close-steam` later). In Game Mode it opens full screen with the dark
theme and works with the controller.

### ARM64 Linux (PB-OS, FEX/Proton on ARM)

On ARM64 Linux the game uses a hybrid ARM64EC `dxgi.dll`, and mods that hook the game's
rendering (such as **LyricsPatch**) can crash it with an "illegal instruction" error. The fix that
works is to put the official **x64 DXVK** `dxgi.dll` and `d3d11.dll`
([DXVK releases](https://github.com/doitsujin/dxvk/releases), `x64` folder) in the game folder.

The manager can do this for you: **Doctor → ARM fix → Add the fix** (or `megamix armfix apply`).
It downloads DXVK 3.1.1 from its GitHub release, checks it, and copies only those two files. Files
with the same names that are already there are backed up. **Remove** (`megamix armfix remove`)
puts everything back. If you already copied DXVK there yourself, the manager leaves your files alone.

On **Windows on ARM** the same fix *might* help, but it's untested and needs a working Vulkan
driver, so it's only offered as an experiment (`megamix armfix apply --experimental`). On x86 PCs
it isn't needed and isn't offered.

## Using the app

Start **Mega Mix+ Mod Manager** from the Start menu or app menu. It finds the game through Steam
(including extra library folders and SD cards). If it can't find it, set the folder under **Settings**.

- **Mods**: click a mod's switch to turn it on or off, and use the arrows (or pick it up with X on a
  controller) to move it. The mod at the top wins when two mods change the same file.
- **Get mods**: search DivaModArchive and GameBanana, look at pictures and requirements, and install.
- **Install from file**: pick a downloaded `.zip` / `.7z` / `.rar`.
- **Undo last change** (bottom left) goes back one step each time.

Controller: D-pad to move, **A** select/toggle, **B** back, **X** pick up and move a mod,
**Y** extra action, **LB/RB** switch pages, **Start** undo, **Select** play.

## Command line

```
megamix list                      mods in load order
megamix enable  "Mod name"        (or the folder name)
megamix disable "Mod name"
megamix priority top "Mod name"   also: up, down, bottom, to <n>
megamix sort --dry-run            show what Auto-sort would change
megamix search "miku" --type song
megamix info dma:307              details, requirements and conflicts
megamix install dma:307           also gb:<id>, a mod page URL, or a .zip/.7z/.rar file
megamix updates                   check for updates
megamix update --all
megamix remove "Mod name"         kept 30 days; `megamix undo` brings it back
megamix conflicts
megamix loadout save "Songs only" / apply "Songs only"
megamix doctor
megamix undo
megamix play
megamix armfix status             ARM64 fix for rendering mods (see above)
megamix steam-shortcut add --close-steam   show it in Steam / Game Mode
megamix self-update               update this app (--check to only look)
```

Add `--json` for script-friendly output, and `--game <folder>` (or `MEGAMIX_GAME`) to use a
different game folder. `megamix help <command>` explains each command.

## Building from source

Needs Rust 1.95 or newer. On Linux you also need `pkg-config` and the udev headers
(`libudev-dev` / `systemd-devel` / `systemd-libs`).

```sh
cargo build --release          # target/release/megamix and megamix-gui
cargo test --workspace
```

`scripts/package-linux.sh` packs a Linux release, and `packaging/windows/megamix.iss` is the
Inno Setup script for the Windows installers. GitHub Actions builds and tests all four targets on
every push and makes the release files for tags.

## AI disclosure

Much of this program's code was written with an AI assistant (Anthropic's Claude), then reviewed
and tested by the author on real hardware: an x86_64 Linux laptop, an ARM64 AYN Thor running PB-OS,
and an x64 Windows PC.

## License

MIT or Apache-2.0, at your option (see `LICENSE-MIT` and `LICENSE-APACHE`).
The window uses [Slint](https://slint.dev) under its royalty-free license.

Not affiliated with SEGA, Crypton Future Media, DivaModArchive or GameBanana.
