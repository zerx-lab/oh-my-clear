#!/bin/sh
# Regenerates every raster derived from the brand SVGs in this directory. Run it after
# changing app-icon.svg or mark.svg and commit the outputs.
#
# Needs resvg, python3 (the .ico) and macOS `iconutil` (the .icns).
#
# Outputs:
# - apps/oh-my-clear/resources/macos/AppIcon.icns: app-icon.svg as drawn (Apple's 824/1024
#   tile with its shadow margin);
# - apps/oh-my-clear/resources/windows/oh-my-clear.ico and
#   apps/oh-my-clear/resources/linux/share/icons/hicolor/*/apps/dev.zerx.oh-my-clear.png:
#   the tile cropped to 904/1024, since taskbars and launchers add no margin of their own;
# - crates/omc-ui/assets/brand/window-icon.png: the 128 px Linux icon for X11 `_NET_WM_ICON`;
# - crates/omc-ui/assets/brand/mark.png: mark.svg at 64 px for the 32 px overview mark
#   (GPUI rasterises an SVG `img` at its intrinsic 1024 px).
set -eu

brand=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$brand/../.." && pwd)
res="$root/apps/oh-my-clear/resources"
ui="$root/crates/omc-ui/assets/brand"
app_id=dev.zerx.oh-my-clear
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

sed 's/viewBox="0 0 1024 1024"/viewBox="60 60 904 904"/' "$brand/app-icon.svg" >"$tmp/tile.svg"
grep -q 'viewBox="60 60 904 904"' "$tmp/tile.svg" || {
    echo "render.sh: app-icon.svg no longer has viewBox=\"0 0 1024 1024\"; update the crop" >&2
    exit 1
}

# macOS
mkdir -p "$res/macos" "$tmp/AppIcon.iconset"
for size in 16 32 128 256 512; do
    resvg -w "$size" -h "$size" "$brand/app-icon.svg" "$tmp/AppIcon.iconset/icon_${size}x${size}.png"
    double=$((size * 2))
    resvg -w "$double" -h "$double" "$brand/app-icon.svg" "$tmp/AppIcon.iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns -o "$res/macos/AppIcon.icns" "$tmp/AppIcon.iconset"

# Linux (freedesktop hicolor theme)
for size in 16 24 32 48 64 128 256 512; do
    dir="$res/linux/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    resvg -w "$size" -h "$size" "$tmp/tile.svg" "$dir/$app_id.png"
done

# Windows: every size the shell asks for at 100–200 % scaling, stored as PNG entries
# (Vista+), so the 256 px one stays small.
mkdir -p "$res/windows"
set --
for size in 16 20 24 32 40 48 64 256; do
    resvg -w "$size" -h "$size" "$tmp/tile.svg" "$tmp/ico-$size.png"
    set -- "$@" "$tmp/ico-$size.png"
done
python3 - "$res/windows/oh-my-clear.ico" "$@" <<'PY'
import struct
import sys

out, pngs = sys.argv[1], sys.argv[2:]
images = [open(path, "rb").read() for path in pngs]
header = struct.pack("<HHH", 0, 1, len(images))
offset = len(header) + 16 * len(images)
entries = b""
for png in images:
    width, height = struct.unpack(">II", png[16:24])
    entries += struct.pack("<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
with open(out, "wb") as f:
    f.write(header + entries + b"".join(images))
PY

# In-app
mkdir -p "$ui"
cp "$res/linux/share/icons/hicolor/128x128/apps/$app_id.png" "$ui/window-icon.png"
resvg -w 64 -h 64 "$brand/mark.svg" "$ui/mark.png"
