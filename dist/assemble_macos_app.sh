#!/usr/bin/env bash
# Lay out the Apple silicon darkroom as an app bundle.
#
#   Mimizan Lab.app/Contents/MacOS/mimizan-gui   what a double click starts
#   Mimizan Lab.app/Contents/MacOS/mimizan       the command line
#   Mimizan Lab.app/Contents/Resources/cameras
#   Mimizan Lab.app/Contents/Resources/look
#   Mimizan Lab.app/Contents/Resources/AppIcon.icns
#
# cameras and look sit in Resources. MacOS holds only the two programs, which
# is what a store signature accepts. The programs look next to themselves,
# then in ../Resources. A double click on the bundle does not open Terminal.
# The icon is the letter M.
#
# Usage: dist/assemble_macos_app.sh <mimizan> <mimizan-gui> <dest.app> <version>
#
# This bundle is not sandboxed. The Mac App Store build is signed with
# dist/macos-entitlements.plist; that build only sees folders the user
# picked in the system dialog.

set -euo pipefail

if [[ $# -ne 4 ]]; then
    echo "usage: dist/assemble_macos_app.sh <mimizan> <mimizan-gui> <dest.app> <version>" >&2
    exit 2
fi

CLI=$1
GUI=$2
DEST=$3
VERSION=$4
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[[ -x "$CLI" && -x "$GUI" ]] || { echo "ERROR: both programs must be executable"; exit 1; }
[[ -n "$VERSION" ]] || { echo "ERROR: version is empty"; exit 1; }

rm -rf "$DEST"
MACOS="$DEST/Contents/MacOS"
RESOURCES="$DEST/Contents/Resources"
mkdir -p "$MACOS" "$RESOURCES"
cp "$CLI" "$GUI" "$MACOS/"
chmod 755 "$MACOS/mimizan" "$MACOS/mimizan-gui"
cp -R "$PROJECT_DIR/cameras" "$PROJECT_DIR/look" "$RESOURCES/"

cat > "$DEST/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Mimizan Lab</string>
	<key>CFBundleDisplayName</key>
	<string>Mimizan Lab</string>
	<key>CFBundleIdentifier</key>
	<string>ch.bojovic.mimizanlab</string>
	<key>CFBundleVersion</key>
	<string>${VERSION}</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundleExecutable</key>
	<string>mimizan-gui</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.photography</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>ITSAppUsesNonExemptEncryption</key>
	<false/>
</dict>
</plist>
EOF

plutil -lint "$DEST/Contents/Info.plist" >/dev/null

# iconutil wants one PNG per size, including the @2x names.
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET" "$DEST/Contents/Resources"
cp "$PROJECT_DIR/dist/icons/mimizan-16.png" "$ICONSET/icon_16x16.png"
cp "$PROJECT_DIR/dist/icons/mimizan-32.png" "$ICONSET/icon_16x16@2x.png"
cp "$PROJECT_DIR/dist/icons/mimizan-32.png" "$ICONSET/icon_32x32.png"
cp "$PROJECT_DIR/dist/icons/mimizan-64.png" "$ICONSET/icon_32x32@2x.png"
cp "$PROJECT_DIR/dist/icons/mimizan-128.png" "$ICONSET/icon_128x128.png"
cp "$PROJECT_DIR/dist/icons/mimizan-256.png" "$ICONSET/icon_128x128@2x.png"
cp "$PROJECT_DIR/dist/icons/mimizan-256.png" "$ICONSET/icon_256x256.png"
cp "$PROJECT_DIR/dist/icons/mimizan-512.png" "$ICONSET/icon_256x256@2x.png"
cp "$PROJECT_DIR/dist/icons/mimizan-512.png" "$ICONSET/icon_512x512.png"
cp "$PROJECT_DIR/dist/icons/mimizan-1024.png" "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$DEST/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$ICONSET")"

test -x "$MACOS/mimizan-gui"
test -x "$MACOS/mimizan"
test ! -e "$MACOS/cameras"
test ! -e "$MACOS/look"
test -f "$RESOURCES/cameras/nikon_z_f.json"
test -f "$RESOURCES/look/neutral.json"
test -s "$DEST/Contents/Resources/AppIcon.icns"
test "$(plutil -extract CFBundleIconFile raw "$DEST/Contents/Info.plist")" = "AppIcon"
echo "==> $DEST"
