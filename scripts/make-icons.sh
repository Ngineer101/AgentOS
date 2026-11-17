#!/usr/bin/env bash
# Regenerate every app icon and logo from the SVGs in assets/icon/ (macOS only:
# uses Quick Look to render, sips to resize, iconutil, and scripts/icon-tool.swift).
#
#   assets/icon/app-icon.svg        source artwork, full bleed (iOS masks the corners)
#   assets/icon/app-icon-macos.svg  the same on Apple's macOS icon grid (rounded, shadowed)
#   assets/icon/logo.svg            the mark alone, transparent, for use inside the apps
#
# Writes:
#   ios/AgentHost/Assets.xcassets/AppIcon.appiconset   1024 px, opaque
#   ios/AgentHost/Assets.xcassets/Logo.imageset        96 pt at 1x/2x/3x
#   desktop/assets/AppIcon.icns                        app bundle icon (install.sh)
#   desktop/assets/app-icon.png                        512 px: Dock icon at runtime, Linux launcher
#   desktop/assets/logo.png                            256 px, shown in the app
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/assets/icon"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

tool() { swift "$root/scripts/icon-tool.swift" "$@"; }

ql() { # svg -> 1024 px PNG at $2
  local base; base="$(basename "$1")"
  qlmanage -t -s 1024 -o "$tmp" "$1" >/dev/null 2>&1
  mv "$tmp/$base.png" "$2"
  [[ "$(sips -g pixelWidth "$2" | awk '/pixelWidth/{print $2}')" == 1024 ]] \
    || { echo "rendering $1 failed" >&2; exit 1; }
}

# Quick Look renders onto white, losing transparency: render over black and over white
# and recover the alpha from the difference.
render() { # svg -> $tmp/<name>.png with transparency
  local name; name="$(basename "$1" .svg)"
  for bg in black white; do
    # A huge rect first in the drawing, so it covers any viewBox.
    # (perl -0 reads the whole file, so only the first </defs> matches).
    BG="$bg" perl -0pe 's#</defs>#</defs><rect x="-4096" y="-4096" width="10240" height="10240" fill="$ENV{BG}"/>#' \
      "$1" >"$tmp/$name-$bg.svg"
    ql "$tmp/$name-$bg.svg" "$tmp/$name-$bg.png"
  done
  tool matte "$tmp/$name-black.png" "$tmp/$name-white.png" "$tmp/$name.png"
}
resize() { sips -z "$2" "$2" "$1" --out "$3" >/dev/null; }

echo "==> rendering SVGs"
render "$src/app-icon.svg"
render "$src/app-icon-macos.svg"
render "$src/logo.svg"

echo "==> iOS app icon and logo"
xc="$root/ios/AgentHost/Assets.xcassets"
mkdir -p "$xc/AppIcon.appiconset" "$xc/Logo.imageset"
tool flatten "$tmp/app-icon.png" "$xc/AppIcon.appiconset/AppIcon-1024.png"
cat >"$xc/Contents.json" <<'J'
{ "info" : { "author" : "xcode", "version" : 1 } }
J
cat >"$xc/AppIcon.appiconset/Contents.json" <<'J'
{
  "images" : [
    { "filename" : "AppIcon-1024.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
J
for s in 1 2 3; do resize "$tmp/logo.png" $((96 * s)) "$xc/Logo.imageset/Logo@${s}x.png"; done
cat >"$xc/Logo.imageset/Contents.json" <<'J'
{
  "images" : [
    { "filename" : "Logo@1x.png", "idiom" : "universal", "scale" : "1x" },
    { "filename" : "Logo@2x.png", "idiom" : "universal", "scale" : "2x" },
    { "filename" : "Logo@3x.png", "idiom" : "universal", "scale" : "3x" }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
J

echo "==> desktop icons and logo"
out="$root/desktop/assets"
mkdir -p "$out"
set_="$tmp/AppIcon.iconset"
mkdir -p "$set_"
for s in 16 32 128 256 512; do
  resize "$tmp/app-icon-macos.png" "$s" "$set_/icon_${s}x${s}.png"
  resize "$tmp/app-icon-macos.png" $((s * 2)) "$set_/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$set_" -o "$out/AppIcon.icns"
resize "$tmp/app-icon-macos.png" 512 "$out/app-icon.png"
resize "$tmp/logo.png" 256 "$out/logo.png"
echo "==> done"
