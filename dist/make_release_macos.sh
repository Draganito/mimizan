#!/usr/bin/env bash
# Apple silicon release archive for mimizan.
#
# Result (always under dist/, not committed):
#   dist/mimizan-<version>-aarch64-macos.zip
#     mimizan, mimizan-gui, cameras/, look/, docs
#
# The two programs link only the frameworks that ship with macOS.
#
# Usage:  dist/make_release_macos.sh [--skip-build]

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_DIR/target}"
CLI="$TARGET_DIR/dist/mimizan"
GUI="$TARGET_DIR/dist/mimizan-gui"

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "ERROR: this archive is the Apple silicon build (uname -m is $(uname -m))"
    exit 1
fi

if [[ "${1:-}" != "--skip-build" ]]; then
    echo "==> cargo build --profile dist"
    (cd "$PROJECT_DIR" && cargo build --profile dist -p mimizan-cli -p mimizan-gui)
fi
for b in "$CLI" "$GUI"; do
    [[ -x "$b" ]] || { echo "ERROR: $b missing"; exit 1; }
    echo "==> binary: $b ($(du -h "$b" | cut -f1))"
done

"$PROJECT_DIR/dist/check_macos_links.sh" "$CLI" "$GUI"

VERSION=$("$CLI" --version | awk '{print $2}')
[[ -n "$VERSION" ]] || { echo "ERROR: could not read version"; exit 1; }
echo "==> version $VERSION"

"$CLI" measure synth --scene wedge --size 256 >/dev/null || { echo "ERROR: measure synth failed"; exit 1; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
NAME="mimizan-$VERSION-aarch64-macos"
mkdir -p "$STAGE/$NAME/docs"
cp "$CLI" "$GUI" "$STAGE/$NAME/"
cp -r "$PROJECT_DIR/cameras" "$PROJECT_DIR/look" "$STAGE/$NAME/"
cp "$PROJECT_DIR/README.md" "$PROJECT_DIR/DETAILED.md" "$PROJECT_DIR/LICENSE" "$STAGE/$NAME/"
cp "$PROJECT_DIR/docs/SPEC.md" "$PROJECT_DIR/docs/RESULTS.md" "$PROJECT_DIR/docs/gui.png" "$STAGE/$NAME/docs/"

DIST="$PROJECT_DIR/dist"
mkdir -p "$DIST"
rm -f "$DIST"/mimizan-*-aarch64-macos.zip
ZIP="$DIST/$NAME.zip"
ditto -c -k --keepParent "$STAGE/$NAME" "$ZIP"
echo "==> $ZIP ($(du -h "$ZIP" | cut -f1))"
