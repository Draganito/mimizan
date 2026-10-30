#!/usr/bin/env bash
# Sign the Apple silicon darkroom for the Mac App Store and wrap it in a pkg.
#
# The unsigned zip from dist/make_release_macos.sh is a different bundle and
# stays unsigned. This one embeds the store profile and the sandbox
# entitlements, so it only sees folders the user picked in the system dialog.
#
# Run on macOS after the dist-profile build. The programs are taken from
# target/dist. Passwords and files come from the environment, never from
# the repository.
#
#   MAC_DISTRIBUTION_P12              Apple Distribution, PKCS#12
#   MAC_DISTRIBUTION_P12_PASSWORD
#   MAC_INSTALLER_P12                 Mac Installer Distribution, PKCS#12
#   MAC_INSTALLER_P12_PASSWORD
#   MAC_APPSTORE_PROFILE              the Mac App Store provisioning profile
#   MAC_BUILD_NUMBER                  CFBundleVersion, default 1

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "ERROR: Mac App Store signing runs on macOS" >&2
    exit 1
fi

for v in MAC_DISTRIBUTION_P12 MAC_DISTRIBUTION_P12_PASSWORD MAC_INSTALLER_P12 MAC_INSTALLER_P12_PASSWORD MAC_APPSTORE_PROFILE; do
    if [[ -z "${!v:-}" ]]; then
        echo "ERROR: $v is empty" >&2
        exit 1
    fi
done
for f in "$MAC_DISTRIBUTION_P12" "$MAC_INSTALLER_P12" "$MAC_APPSTORE_PROFILE"; do
    [[ -f "$f" ]] || { echo "ERROR: missing $f" >&2; exit 1; }
done

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_DIR/target}"
CLI="$TARGET_DIR/dist/mimizan"
GUI="$TARGET_DIR/dist/mimizan-gui"
ENTITLEMENTS="$PROJECT_DIR/dist/macos-entitlements.plist"
BUILD_NUMBER="${MAC_BUILD_NUMBER:-1}"

[[ -x "$CLI" && -x "$GUI" ]] || { echo "ERROR: build the programs first (dist/make_release_macos.sh)" >&2; exit 1; }
[[ -f "$ENTITLEMENTS" ]] || { echo "ERROR: $ENTITLEMENTS missing" >&2; exit 1; }

VERSION=$("$CLI" --version | awk '{print $2}')
[[ -n "$VERSION" ]] || { echo "ERROR: could not read version" >&2; exit 1; }
echo "==> version $VERSION build $BUILD_NUMBER"

STAGE=$(mktemp -d)
KEYCHAIN="$STAGE/appstore.keychain-db"
KEYCHAIN_PASSWORD=$(openssl rand -base64 24)
OLD_KEYCHAIN=$(security default-keychain | sed 's/^ *"//;s/" *$//')

cleanup() {
    security default-keychain -s "$OLD_KEYCHAIN" >/dev/null 2>&1 || true
    security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
    rm -rf "$STAGE"
}
trap cleanup EXIT

security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security set-keychain-settings -lut 3600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security default-keychain -s "$KEYCHAIN"
security list-keychains -d user -s "$KEYCHAIN" "$OLD_KEYCHAIN"

import_p12() {
    security import "$1" -k "$KEYCHAIN" -P "$2" -A -t cert -f pkcs12 \
        -T /usr/bin/codesign -T /usr/bin/productbuild -T /usr/bin/productsign
}
import_p12 "$MAC_DISTRIBUTION_P12" "$MAC_DISTRIBUTION_P12_PASSWORD"
import_p12 "$MAC_INSTALLER_P12" "$MAC_INSTALLER_P12_PASSWORD"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN"

echo "==> identities"
security find-identity -v "$KEYCHAIN"

DIST_ID=$(security find-identity -v -p codesigning "$KEYCHAIN" | sed -n 's/.*"\(Apple Distribution:.*\)"/\1/p' | head -n 1)
INSTALLER_ID=$(security find-identity -v "$KEYCHAIN" | sed -n 's/.*"\(3rd Party Mac Developer Installer:.*\)"/\1/p' | head -n 1)
if [[ -z "$DIST_ID" || -z "$INSTALLER_ID" ]]; then
    echo "ERROR: the Apple Distribution or Mac Installer identity is not in the keychain" >&2
    exit 1
fi
echo "==> app: $DIST_ID"
echo "==> pkg: $INSTALLER_ID"

APP="$STAGE/Mimizan Lab.app"
"$PROJECT_DIR/dist/assemble_macos_app.sh" "$CLI" "$GUI" "$APP" "$VERSION"
plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$APP/Contents/Info.plist"
cp "$MAC_APPSTORE_PROFILE" "$APP/Contents/embedded.provisionprofile"

sign() {
    codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" --sign "$DIST_ID" "$1"
}
sign "$APP/Contents/MacOS/mimizan"
sign "$APP/Contents/MacOS/mimizan-gui"
sign "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

mkdir -p "$PROJECT_DIR/dist"
PKG="$PROJECT_DIR/dist/mimizan-${VERSION}-aarch64-macos-appstore.pkg"
rm -f "$PKG"
productbuild --component "$APP" /Applications --sign "$INSTALLER_ID" --product "$APP/Contents/Info.plist" "$PKG"
echo "==> $PKG"
