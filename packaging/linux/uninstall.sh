#!/bin/sh
# Remove Mega Mix+ Mod Manager (installed with install.sh) for this user.
#
#   sh uninstall.sh            remove the app (asks before touching Steam)
#   sh uninstall.sh --yes      don't ask questions (answers yes)
#   sh uninstall.sh --purge    also delete the app's settings and download cache
#
# A copy lives in ~/.local/share/megamix-mod-manager/uninstall.sh.
# Your game, mods, mod backups and loadouts are never touched.
set -eu

APP_ID="megamix-mod-manager"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}"
APPS_DIR="$DATA_DIR/applications"
ICON_DIR="$DATA_DIR/icons/hicolor"
MARKER='# Added by the Mega Mix+ Mod Manager installer'

YES=0
PURGE=0
for a in "$@"; do
  case "$a" in
    -y|--yes) YES=1 ;;
    --purge) PURGE=1 ;;
    -h|--help)
      echo "Usage: sh uninstall.sh [--yes] [--purge]"
      echo "  --yes    don't ask questions"
      echo "  --purge  also delete settings (~/.config/megamix) and the download cache"
      exit 0 ;;
    *) echo "Unknown option: $a (try --help)" >&2; exit 2 ;;
  esac
done

say()  { printf '%s\n' "$*"; }
warn() { printf 'Warning: %s\n' "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }
ask() {
  [ "$YES" = 1 ] && return 0
  [ -r /dev/tty ] || return 1
  printf '%s [Y/n] ' "$1"
  read -r reply </dev/tty || return 1
  case "$reply" in [nN]*) return 1 ;; *) return 0 ;; esac
}

say "==> Removing Mega Mix+ Mod Manager"

# Steam shortcut (needs the program, so do it first).
if [ -x "$BIN_DIR/megamix" ] && "$BIN_DIR/megamix" steam-shortcut status 2>/dev/null | grep -q "^In Steam"; then
  if ask "Also remove it from Steam? (Steam closes and starts again if it's running)"; then
    "$BIN_DIR/megamix" steam-shortcut remove --close-steam || warn "couldn't remove it from Steam"
  fi
fi

# Close the app if it's open.
if have pkill; then
  pkill -x megamix-gui 2>/dev/null || true
fi

removed=0
rm_f() {
  for f in "$@"; do
    if [ -e "$f" ] || [ -L "$f" ]; then
      rm -rf -- "$f"
      removed=$((removed + 1))
    fi
  done
}

rm_f "$BIN_DIR/megamix" "$BIN_DIR/megamix-gui"
# The installer may have created ~/.local/bin; remove it if nothing else is in it.
rmdir "$BIN_DIR" 2>/dev/null || true
rm_f "$APPS_DIR/$APP_ID.desktop" "$APPS_DIR/$APP_ID-links.desktop"
for s in 64 128 256 512; do rm_f "$ICON_DIR/${s}x${s}/apps/$APP_ID.png"; done
rm_f "$ICON_DIR/scalable/apps/$APP_ID.svg"
rm_f "$DATA_DIR/$APP_ID"
have update-desktop-database && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
have gtk-update-icon-cache && gtk-update-icon-cache -q -t "$ICON_DIR" 2>/dev/null || true

# One-click link handler entry pointing at the removed app.
if [ -f "$CONFIG_DIR/mimeapps.list" ]; then
  sed -i "/^x-scheme-handler\/divamodmanager=$APP_ID\(-links\)\{0,1\}\.desktop;\{0,1\}$/d" "$CONFIG_DIR/mimeapps.list" 2>/dev/null || true
fi

# PATH lines the installer added (the marker line and the one after it).
for rc in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
  if [ -f "$rc" ] && grep -qxF "$MARKER" "$rc"; then
    sed -i "/^$MARKER\$/,+1d" "$rc" 2>/dev/null && say "  cleaned up $rc"
  fi
done
rm_f "$CONFIG_DIR/fish/conf.d/megamix.fish"

if [ "$PURGE" = 1 ] || { [ "$YES" = 0 ] && [ -d "$CONFIG_DIR/megamix" ] && ask "Also delete the app's settings and download cache?"; }; then
  rm_f "$CONFIG_DIR/megamix" "$CACHE_DIR/megamix"
  say "Deleted settings and cache."
fi

if [ "$removed" = 0 ]; then
  say "It wasn't installed (nothing to remove)."
else
  say "Done. Your game, mods, mod backups and loadouts were left alone."
fi
