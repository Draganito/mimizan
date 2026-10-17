#!/usr/bin/env bash
# Lay out the Apple silicon darkroom as an app bundle.
#
#   Mimizan Lab.app/Contents/MacOS/mimizan-gui   what a double click starts
#   Mimizan Lab.app/Contents/MacOS/mimizan       the command line
#   Mimizan Lab.app/Contents/MacOS/cameras
#   Mimizan Lab.app/Contents/MacOS/look
#
# The programs look for cameras and look next to themselves. A double click
# on the bundle does not open Terminal.
#
# Usage: dist/assemble_macos_app.sh <mimizan> <mimizan-gui> <dest.app> <version>

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
mkdir -p "$MACOS" "$DEST/Contents"
cp "$CLI" "$GUI" "$MACOS/"
chmod 755 "$MACOS/mimizan" "$MACOS/mimizan-gui"
cp -R "$PROJECT_DIR/cameras" "$PROJECT_DIR/look" "$MACOS/"

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
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF

plutil -lint "$DEST/Contents/Info.plist" >/dev/null
test -x "$MACOS/mimizan-gui"
test -x "$MACOS/mimizan"
test -f "$MACOS/cameras/nikon_z_f.json"
test -f "$MACOS/look/neutral.json"
echo "==> $DEST"
