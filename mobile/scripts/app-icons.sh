#!/bin/sh
# App icon, Android adaptive foreground and splash from the brand files (docs/brand). Needs
# rsvg-convert. Run from mobile/.
set -eu
brand=../docs/brand
rsvg-convert -w 1024 -h 1024 "$brand/tetrad-avatar.svg" -o assets/icon.png
# The avatar without its black square: the adaptive icon draws its own background.
sed 's|<rect width="64" height="64"/>||' "$brand/tetrad-avatar.svg" |
  rsvg-convert -w 512 -h 512 -o assets/android-icon-foreground.png
sed 's|<rect width="64" height="64"/>||' "$brand/tetrad-avatar.svg" |
  rsvg-convert -w 1024 -h 1024 -o assets/splash-icon.png
rsvg-convert -w 48 -h 48 "$brand/tetrad-avatar.svg" -o assets/favicon.png
