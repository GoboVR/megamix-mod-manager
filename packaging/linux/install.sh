#!/bin/sh
# Mega Mix+ Mod Manager installer for Linux (x86_64 and ARM64).
#
#   sh install.sh               install or update (for this user only)
#   sh install.sh --uninstall   remove it again (your mods are not touched)
#   sh install.sh --yes         don't ask questions (answers yes)
#   sh install.sh --no-steam    don't offer to add it to Steam
#
# Run it from an unpacked release folder, or on its own: then it downloads
# the latest release for this computer. Everything goes into your home folder:
#   ~/.local/bin/megamix, ~/.local/bin/megamix-gui
#   ~/.local/share/applications/megamix-mod-manager.desktop (app menu)
#   ~/.local/share/icons/hicolor/…/megamix-mod-manager.png
# Nothing needs root, except installing missing system libraries if you agree.
set -eu

REPO="GoboVR/megamix-mod-manager"
APP_ID="megamix-mod-manager"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS_DIR="$DATA_DIR/applications"
ICON_DIR="$DATA_DIR/icons/hicolor"

YES=0
STEAM=ask
MODE=install
for a in "$@"; do
  case "$a" in
    --uninstall|uninstall) MODE=uninstall ;;
    -y|--yes) YES=1 ;;
    --no-steam) STEAM=no ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown option: $a (try --help)" >&2; exit 2 ;;
  esac
done

say()  { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'Warning: %s\n' "$*" >&2; }
die()  { printf 'Error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

ask() { # ask "question" -> 0 for yes
  [ "$YES" = 1 ] && return 0
  [ -r /dev/tty ] || return 1
  printf '%s [Y/n] ' "$1"
  read -r reply </dev/tty || return 1
  case "$reply" in [nN]*) return 1 ;; *) return 0 ;; esac
}

# ---- uninstall ------------------------------------------------------------

if [ "$MODE" = uninstall ]; then
  HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || echo .)
  for u in "$HERE/uninstall.sh" "$DATA_DIR/$APP_ID/uninstall.sh"; do
    if [ -f "$u" ]; then
      if [ "$YES" = 1 ]; then exec sh "$u" --yes; else exec sh "$u"; fi
    fi
  done
  die "uninstall.sh not found; download it from https://github.com/$REPO"
fi

# ---- what system is this? -------------------------------------------------

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) die "this computer's CPU ($(uname -m)) isn't supported. Builds exist for x86_64 and ARM64." ;;
esac

DISTRO_ID=unknown
DISTRO_LIKE=
DISTRO_NAME=Linux
if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  DISTRO_ID="${ID:-unknown}"
  DISTRO_LIKE="${ID_LIKE:-}"
  DISTRO_NAME="${PRETTY_NAME:-${NAME:-Linux}}"
fi

# Read-only systems (SteamOS, PB-OS, Bazzite, Fedora Atomic, …): no packages.
IMMUTABLE=0
if [ -e /run/ostree-booted ] || have steamos-readonly; then
  IMMUTABLE=1
fi
case "$DISTRO_ID $DISTRO_LIKE" in
  *steamos*|*bazzite*|*chimeraos*|*holoiso*) IMMUTABLE=1 ;;
esac

PKG=none
if [ "$IMMUTABLE" = 0 ]; then
  if have pacman; then PKG=pacman
  elif have apt-get; then PKG=apt
  elif have dnf; then PKG=dnf
  elif have zypper; then PKG=zypper
  fi
fi

step "Installing for $DISTRO_NAME ($ARCH)"

# ---- get the program files ------------------------------------------------

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || echo .)
SRC=
TMP=
cleanup() { if [ -n "$TMP" ]; then rm -rf "$TMP"; fi; }
trap cleanup EXIT INT TERM

if [ -x "$HERE/megamix" ] && [ -x "$HERE/megamix-gui" ]; then
  SRC="$HERE"
  say "Using the files next to this script."
else
  have curl || have wget || die "need curl or wget to download the program."
  # Download into the home folder, not /tmp (RAM-backed on SteamOS/PB-OS).
  TMP=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/megamix-install.XXXXXX" 2>/dev/null) \
    || { mkdir -p "${XDG_CACHE_HOME:-$HOME/.cache}"; TMP=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/megamix-install.XXXXXX"); }
  URL="https://github.com/$REPO/releases/latest/download/megamix-linux-$ARCH.tar.gz"
  say "Downloading $URL"
  if have curl; then
    curl -fL --retry 3 -o "$TMP/megamix.tar.gz" "$URL" || die "download failed."
  else
    wget -q -O "$TMP/megamix.tar.gz" "$URL" || die "download failed."
  fi
  tar -xzf "$TMP/megamix.tar.gz" -C "$TMP" || die "the download is damaged."
  SRC=$(find "$TMP" -name megamix-gui -type f -exec dirname {} \; | head -n 1)
  [ -n "$SRC" ] || die "the download doesn't contain the program."
fi

# ---- system libraries -----------------------------------------------------

# The window needs these (most desktops already have them).
LDCONFIG=$(command -v ldconfig 2>/dev/null || true)
for p in /sbin/ldconfig /usr/sbin/ldconfig; do
  [ -z "$LDCONFIG" ] && [ -x "$p" ] && LDCONFIG=$p
done
lib_ok() {
  if [ -n "$LDCONFIG" ] && "$LDCONFIG" -p 2>/dev/null | grep -q "$1"; then return 0; fi
  for d in /usr/lib /usr/lib64 /lib /lib64 "/usr/lib/$ARCH-linux-gnu" "/lib/$ARCH-linux-gnu"; do
    ls "$d/$1"* >/dev/null 2>&1 && return 0
  done
  return 1
}

MISSING=
for lib in libfontconfig.so.1 libfreetype.so.6 libudev.so.1 libxkbcommon.so.0 libEGL.so.1; do
  lib_ok "$lib" || MISSING="$MISSING $lib"
done
if ! lib_ok libwayland-client.so.0 && ! lib_ok libX11.so.6; then
  MISSING="$MISSING libwayland-client.so.0"
fi

pkgs_for() { # library -> package name for this package manager
  case "$PKG:$1" in
    pacman:libfontconfig*) echo fontconfig ;;
    pacman:libfreetype*) echo freetype2 ;;
    pacman:libudev*) echo systemd-libs ;;
    pacman:libxkbcommon*) echo libxkbcommon ;;
    pacman:libEGL*) echo libglvnd ;;
    pacman:libwayland*) echo wayland ;;
    apt:libfontconfig*) echo libfontconfig1 ;;
    apt:libfreetype*) echo libfreetype6 ;;
    apt:libudev*) echo libudev1 ;;
    apt:libxkbcommon*) echo libxkbcommon0 ;;
    apt:libEGL*) echo libegl1 ;;
    apt:libwayland*) echo libwayland-client0 ;;
    dnf:libfontconfig*) echo fontconfig ;;
    dnf:libfreetype*) echo freetype ;;
    dnf:libudev*) echo systemd-libs ;;
    dnf:libxkbcommon*) echo libxkbcommon ;;
    dnf:libEGL*) echo libglvnd-egl ;;
    dnf:libwayland*) echo libwayland-client ;;
    zypper:libfontconfig*) echo libfontconfig1 ;;
    zypper:libfreetype*) echo libfreetype6 ;;
    zypper:libudev*) echo libudev1 ;;
    zypper:libxkbcommon*) echo libxkbcommon0 ;;
    zypper:libEGL*) echo libEGL1 ;;
    zypper:libwayland*) echo libwayland-client0 ;;
  esac
}

if [ -n "$MISSING" ]; then
  step "Some system libraries are missing:$MISSING"
  PKGS=
  for l in $MISSING; do PKGS="$PKGS $(pkgs_for "$l")"; done
  PKGS=$(echo "$PKGS" | xargs)
  if [ "$PKG" = none ] || [ -z "$PKGS" ]; then
    if [ "$IMMUTABLE" = 1 ]; then
      warn "this is a read-only system, so nothing is installed. The command line tool works anyway; if the window doesn't open, tell us your system."
    else
      warn "install them with your package manager. The command line tool works without them."
    fi
  else
    case "$PKG" in
      pacman) CMD="pacman -S --needed --noconfirm $PKGS" ;;
      apt) CMD="apt-get install -y $PKGS" ;;
      dnf) CMD="dnf install -y $PKGS" ;;
      zypper) CMD="zypper --non-interactive install $PKGS" ;;
    esac
    if [ "$(id -u)" = 0 ]; then SUDO=; elif have sudo; then SUDO=sudo; elif have doas; then SUDO=doas; else SUDO=; fi
    if ask "Install them now with: ${SUDO:+$SUDO }$CMD ?"; then
      # shellcheck disable=SC2086
      ${SUDO:+$SUDO} $CMD || warn "installing failed; the window may not open until they're installed."
    else
      say "Skipped. Install them later with: ${SUDO:+$SUDO }$CMD"
    fi
  fi
fi

# ---- copy files -----------------------------------------------------------

step "Copying the program to $BIN_DIR"
mkdir -p "$BIN_DIR" "$APPS_DIR"
# Copy to a temporary name, then rename: works while the old one is running.
for b in megamix megamix-gui; do
  cp "$SRC/$b" "$BIN_DIR/.$b.new"
  chmod 755 "$BIN_DIR/.$b.new"
  mv -f "$BIN_DIR/.$b.new" "$BIN_DIR/$b"
done

for s in 64 128 256 512; do
  if [ -f "$SRC/icons/icon-$s.png" ]; then
    mkdir -p "$ICON_DIR/${s}x${s}/apps"
    cp "$SRC/icons/icon-$s.png" "$ICON_DIR/${s}x${s}/apps/$APP_ID.png"
  fi
done
if [ -f "$SRC/icons/icon.svg" ]; then
  mkdir -p "$ICON_DIR/scalable/apps"
  cp "$SRC/icons/icon.svg" "$ICON_DIR/scalable/apps/$APP_ID.svg"
fi

# App menu entry. It also handles one-click install links (divamodmanager:).
cat > "$APPS_DIR/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Mega Mix+ Mod Manager
GenericName=Mod Manager
Comment=Install and manage mods for Hatsune Miku: Project DIVA Mega Mix+
Exec="$BIN_DIR/megamix-gui" %u
Icon=$APP_ID
Terminal=false
Categories=Game;Utility;
Keywords=diva;miku;megamix;mods;divamodloader;
MimeType=x-scheme-handler/divamodmanager;
EOF
# The app's own "register link handler" file would be a duplicate now.
rm -f "$APPS_DIR/$APP_ID-links.desktop"
have update-desktop-database && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
if [ -f "$SRC/uninstall.sh" ]; then
  mkdir -p "$DATA_DIR/$APP_ID"
  cp "$SRC/uninstall.sh" "$DATA_DIR/$APP_ID/uninstall.sh"
  chmod 755 "$DATA_DIR/$APP_ID/uninstall.sh"
fi
if [ -d "$SRC/steam" ]; then
  mkdir -p "$DATA_DIR/$APP_ID/steam"
  cp "$SRC"/steam/*.png "$DATA_DIR/$APP_ID/steam/"
fi
have gtk-update-icon-cache && gtk-update-icon-cache -q -t "$ICON_DIR" 2>/dev/null || true

# ---- one-click links ------------------------------------------------------

if have xdg-mime; then
  CURRENT=$(xdg-mime query default x-scheme-handler/divamodmanager 2>/dev/null || true)
  if [ -z "$CURRENT" ] || [ "$CURRENT" = "$APP_ID.desktop" ] || [ "$CURRENT" = "$APP_ID-links.desktop" ]; then
    xdg-mime default "$APP_ID.desktop" x-scheme-handler/divamodmanager 2>/dev/null || true
    say "One-click install buttons on DivaModArchive and GameBanana now open this app."
  elif ask "One-click install links open $CURRENT now. Use Mega Mix+ Mod Manager instead?"; then
    xdg-mime default "$APP_ID.desktop" x-scheme-handler/divamodmanager 2>/dev/null || true
    say "One-click install links now open this app."
  fi
fi

# ---- Steam / Game Mode ----------------------------------------------------

STEAM_FOUND=0
for d in "$DATA_DIR/Steam" "$HOME/.local/share/Steam" "$HOME/.steam/steam" \
         "$HOME/.var/app/com.valvesoftware.Steam/.local/share/Steam"; do
  [ -d "$d/userdata" ] && STEAM_FOUND=1
done
if [ "$STEAM" = ask ] && [ "$STEAM_FOUND" = 1 ]; then
  step "Steam"
  if "$BIN_DIR/megamix" steam-shortcut status 2>/dev/null | grep -q "^In Steam"; then
    WANT="Update the Mega Mix+ Mod Manager shortcut in Steam?"
  else
    WANT="Add Mega Mix+ Mod Manager to Steam, so it shows up in your library and in Game Mode?"
  fi
  if pgrep -x steam >/dev/null 2>&1; then
    WANT="$WANT (Steam will close and start again)"
  fi
  if ask "$WANT"; then
    "$BIN_DIR/megamix" steam-shortcut add --close-steam \
      || warn "couldn't add it to Steam. Try later with: megamix steam-shortcut add --close-steam"
  else
    say "Skipped. Add it later with: megamix steam-shortcut add --close-steam"
  fi
fi

# ---- PATH -----------------------------------------------------------------

case ":$PATH:" in
  *":$BIN_DIR:"*) ON_PATH=1 ;;
  *) ON_PATH=0 ;;
esac
if [ "$ON_PATH" = 0 ]; then
  step "Adding $BIN_DIR to your PATH"
  LINE="export PATH=\"$BIN_DIR:\$PATH\""
  # ~/.profile always; the others only if they exist.
  for rc in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
    case "$rc" in
      */.profile) ;;
      *) [ -f "$rc" ] || continue ;;
    esac
    if ! grep -qs "$BIN_DIR" "$rc"; then
      printf '\n# Added by the Mega Mix+ Mod Manager installer\n%s\n' "$LINE" >> "$rc"
      say "  updated $rc"
    fi
  done
  if have fish; then
    FISH_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d"
    mkdir -p "$FISH_DIR"
    printf 'fish_add_path -g "%s"\n' "$BIN_DIR" > "$FISH_DIR/megamix.fish"
    say "  updated $FISH_DIR/megamix.fish"
  fi
  say "Open a new terminal for the 'megamix' command to work."
fi

# ---- check the game -------------------------------------------------------

step "Checking the game"
if ! "$BIN_DIR/megamix" doctor >"${TMPDIR:-/tmp}/.megamix-doctor.$$" 2>&1; then :; fi
sed 's/^/  /' "${TMPDIR:-/tmp}/.megamix-doctor.$$"
if grep -q "could not find" "${TMPDIR:-/tmp}/.megamix-doctor.$$"; then
  say "  (If the game is installed somewhere unusual, set its folder in the app's Settings.)"
fi
rm -f "${TMPDIR:-/tmp}/.megamix-doctor.$$"

step "Done!"
say "Start 'Mega Mix+ Mod Manager' from your app menu, or run: megamix-gui"
say "Command line: megamix --help"
say "To remove it: sh $DATA_DIR/$APP_ID/uninstall.sh"
