#!/usr/bin/env bash
# Apple silicon release archive for mimizan.
#
# Result (always under dist/, not committed):
#   dist/mimizan-<version>-aarch64-macos.zip
#     Mimizan Lab.app  (double click starts mimizan-gui; no Terminal)
#     README.md, LICENSE
#
# The two programs link only the frameworks that ship with macOS.
# cameras/ and look/ sit in Contents/Resources. MacOS holds the two programs.
#
# Usage:  dist/make_release_macos.sh [--skip-build] [--store]
#
# --store builds the window for the Mac App Store instead (the workflow
# macos-store.yml, then dist/sign_macos_appstore.sh): the `store` feature,
# which sells the TIFF and the negative through the App Store, for
# macOS 12, where its StoreKit begins. That build is no download and no
# zip is made of it. Without the flag nothing is sold and nothing is
# locked, and the script refuses a window that carries the purchase.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_DIR/target}"
CLI="$TARGET_DIR/dist/mimizan"
GUI="$TARGET_DIR/dist/mimizan-gui"

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "ERROR: this archive is the Apple silicon build (uname -m is $(uname -m))"
    exit 1
fi

BUILD=1
STORE=0
for arg in "$@"; do
    case "$arg" in
        --skip-build) BUILD=0 ;;
        --store) STORE=1 ;;
        *) echo "ERROR: unknown argument $arg"; exit 2 ;;
    esac
done

if [[ "$BUILD" == 1 ]]; then
    if [[ "$STORE" == 1 ]]; then
        echo "==> cargo build --profile dist, the Mac App Store window"
        (cd "$PROJECT_DIR" && MACOSX_DEPLOYMENT_TARGET=12.0 \
            cargo build --profile dist -p mimizan-cli -p mimizan-gui --features mimizan-gui/store)
    else
        echo "==> cargo build --profile dist"
        (cd "$PROJECT_DIR" && cargo build --profile dist -p mimizan-cli -p mimizan-gui)
    fi
fi
for b in "$CLI" "$GUI"; do
    [[ -x "$b" ]] || { echo "ERROR: $b missing"; exit 1; }
    echo "==> binary: $b ($(du -h "$b" | cut -f1))"
done

"$PROJECT_DIR/dist/check_macos_links.sh" "$CLI" "$GUI"

# The purchase is in the Mac App Store window and in no other. StoreKit
# among the window's frameworks is the sign of it.
if grep -q 'StoreKit\.framework' <<<"$(otool -L "$GUI")"; then
    [[ "$STORE" == 1 ]] || { echo "ERROR: $GUI carries the App Store purchase; the download is built without it"; exit 1; }
else
    [[ "$STORE" == 0 ]] || { echo "ERROR: $GUI was built without the App Store purchase"; exit 1; }
fi

VERSION=$("$CLI" --version | awk '{print $2}')
[[ -n "$VERSION" ]] || { echo "ERROR: could not read version"; exit 1; }
echo "==> version $VERSION"

# Smoke test: the synthetic bench must run without any input files and
# report finite numbers (512 is the smallest size the wedge insets allow).
SMOKE=$("$CLI" measure synth --scene wedge --size 512 --json) || { echo "ERROR: measure synth failed"; exit 1; }
if grep -qiE '"rmse[a-z_]*":\s*(null|"?NaN"?)' <<<"$SMOKE"; then
    echo "ERROR: measure synth reported NaN"; exit 1
fi

if [[ "$STORE" == 1 ]]; then
    echo "==> the Mac App Store window is built; dist/sign_macos_appstore.sh makes its package"
    exit 0
fi

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
NAME="mimizan-$VERSION-aarch64-macos"
mkdir -p "$STAGE/$NAME"
"$PROJECT_DIR/dist/assemble_macos_app.sh" "$CLI" "$GUI" "$STAGE/$NAME/Mimizan Lab.app" "$VERSION"
cp "$PROJECT_DIR/README.md" "$PROJECT_DIR/LICENSE" "$STAGE/$NAME/"

DIST="$PROJECT_DIR/dist"
mkdir -p "$DIST"
rm -f "$DIST"/mimizan-*-aarch64-macos.zip
ZIP="$DIST/$NAME.zip"
ditto -c -k --keepParent "$STAGE/$NAME" "$ZIP"
echo "==> $ZIP ($(du -h "$ZIP" | cut -f1))"
