#!/usr/bin/env bash
# Steam library artwork for the non-Steam shortcut, made from the app icon:
#   assets/steam/capsule.png  600x900   portrait library tile
#   assets/steam/wide.png     920x430   landscape tile
#   assets/steam/hero.png     1920x620  banner on the game page
#   assets/steam/logo.png     transparent title drawn over the banner
# Run after scripts/make-icons.sh. Needs ImageMagick 7 and a bold font.
set -euo pipefail
cd "$(dirname "$0")/.."
icon=assets/icon-source.png
[ -f "$icon" ] || icon=assets/icon-512.png
font=${FONT:-$(fc-match -f '%{file}' 'Noto Sans:bold')}
out=assets/steam
mkdir -p "$out"
title="Mega Mix+"
sub="Mod Manager"
bg() { # w h -> background with a soft glow
  magick -size "$1x$2" gradient:'#0d1b2a-#1b263b' \
    \( -size "$1x$2" radial-gradient:'#00b4ff55-#00000000' \) -compose over -composite "$3"
}

bg 600 900 "$out/capsule.png"
magick "$out/capsule.png" \( "$icon" -resize 380x380 \) -gravity north -geometry +0+150 -composite \
  -font "$font" -fill white -gravity south -pointsize 78 -annotate +0+190 "$title" \
  -fill '#7fd8ff' -pointsize 52 -annotate +0+120 "$sub" "$out/capsule.png"

bg 920 430 "$out/wide.png"
magick "$out/wide.png" \( "$icon" -resize 300x300 \) -gravity west -geometry +70+0 -composite \
  -font "$font" -fill white -gravity west -pointsize 76 -annotate +400-40 "$title" \
  -fill '#7fd8ff' -pointsize 50 -annotate +404+45 "$sub" "$out/wide.png"

bg 1920 620 "$out/hero.png"
magick "$out/hero.png" \( "$icon" -resize 520x520 -alpha set -channel A -evaluate multiply 0.35 +channel \) \
  -gravity east -geometry +160+0 -composite "$out/hero.png"

magick -size 1000x300 xc:none -font "$font" -gravity center \
  -fill white -stroke '#0d1b2a' -strokewidth 3 -pointsize 120 -annotate +0-50 "$title" \
  -fill '#7fd8ff' -stroke none -pointsize 72 -annotate +0+80 "$sub" -trim +repage "$out/logo.png"

echo "Steam artwork written to $out/"
