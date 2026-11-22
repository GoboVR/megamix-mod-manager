#!/usr/bin/env bash
# Make every icon size the app uses from one source image.
#   scripts/make-icons.sh my-icon.png    (square PNG, 1024x1024 or at least 512x512)
#   scripts/make-icons.sh my-icon.svg
# Writes assets/icon-{16..512}.png, assets/icon.ico (Windows .exe, installer)
# and assets/icon.svg (only when the source is an SVG).
set -euo pipefail
src=${1:?usage: scripts/make-icons.sh <square .png or .svg>}
cd "$(dirname "$0")/.."
case "$src" in /*) ;; *) src="$OLDPWD/$src" ;; esac
[ -f "$src" ] || { echo "no such file: $src" >&2; exit 1; }
if [[ "$src" == *.svg ]]; then
  [ "$src" -ef assets/icon.svg ] || cp "$src" assets/icon.svg
  for s in 16 24 32 48 64 128 256 512; do rsvg-convert -w $s -h $s "$src" -o assets/icon-$s.png; done
else
  read -r w h < <(magick identify -format '%w %h\n' "$src")
  [ "$w" = "$h" ] || echo "warning: the image isn't square (${w}x${h}); it will be stretched" >&2
  [ "$w" -ge 512 ] || echo "warning: ${w}px is small; 1024px looks best" >&2
  for s in 16 24 32 48 64 128 256 512; do
    magick "$src" -background none -resize ${s}x${s}! -strip assets/icon-$s.png
  done
  # A PNG source can't make an SVG; the Linux menu uses the PNGs instead.
  rm -f assets/icon.svg
fi
magick assets/icon-{16,24,32,48,64,128,256}.png assets/icon.ico
echo "Icons updated in assets/. Rebuild to use them."
