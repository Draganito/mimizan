#!/usr/bin/env bash
# Builds the release artefacts for mimizan.
#
# Result (always under dist/, not committed):
#   dist/mimizan_<version>_amd64.deb                 both binaries, cameras/, look/,
#                                                     desktop file, docs
#   dist/mimizan-<version>-x86_64-linux.tar.gz       the same as a plain tarball
#
# Usage:  dist/make_release.sh [--skip-build]
#
# One-time prerequisite for the .deb:  cargo install cargo-deb --locked

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_DIR/target}"
CLI="$TARGET_DIR/dist/mimizan"
GUI="$TARGET_DIR/dist/mimizan-gui"

if [[ "${1:-}" != "--skip-build" ]]; then
    echo "==> cargo build --profile dist"
    (cd "$PROJECT_DIR" && cargo build --profile dist -p mimizan-cli -p mimizan-gui)
fi
for b in "$CLI" "$GUI"; do
    [[ -x "$b" ]] || { echo "ERROR: $b missing"; exit 1; }
    echo "==> binary: $b ($(du -h "$b" | cut -f1))"
done

VERSION=$("$CLI" --version | awk '{print $2}')
[[ -n "$VERSION" ]] || { echo "ERROR: could not read version"; exit 1; }
echo "==> version $VERSION"

# Smoke test: the synthetic bench must run without any input files.
"$CLI" measure synth --scene wedge --size 256 >/dev/null || { echo "ERROR: measure synth failed"; exit 1; }

# --- tarball -----------------------------------------------------------------
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
NAME="mimizan-$VERSION-x86_64-linux"
mkdir -p "$STAGE/$NAME/docs"
cp "$CLI" "$GUI" "$STAGE/$NAME/"
cp -r "$PROJECT_DIR/cameras" "$PROJECT_DIR/look" "$STAGE/$NAME/"
cp "$PROJECT_DIR/README.md" "$PROJECT_DIR/LICENSE" "$STAGE/$NAME/"
cp "$PROJECT_DIR/third_party/librtprocess/LICENSE.txt" "$STAGE/$NAME/LICENSE-librtprocess.txt"
cp "$PROJECT_DIR/docs/SPEC.md" "$PROJECT_DIR/docs/RESULTS.md" "$PROJECT_DIR/docs/gui.png" "$STAGE/$NAME/docs/"
cp "$PROJECT_DIR/dist/mimizan-gui.desktop" "$STAGE/$NAME/"
TARBALL="$PROJECT_DIR/dist/$NAME.tar.gz"
rm -f "$PROJECT_DIR"/dist/mimizan-*-x86_64-linux.tar.gz
tar -C "$STAGE" -czf "$TARBALL" "$NAME"
echo "==> $TARBALL ($(du -h "$TARBALL" | cut -f1))"

# --- .deb --------------------------------------------------------------------
if command -v cargo-deb >/dev/null; then
    rm -f "$PROJECT_DIR"/dist/mimizan_*.deb
    DEB=$(cd "$PROJECT_DIR" && cargo deb -p mimizan-cli --profile dist --no-build -o "$PROJECT_DIR/dist/" | tail -1)
    [[ -f "$DEB" ]] || { echo "ERROR: cargo deb produced no package"; exit 1; }
    LISTING=$(dpkg-deb -c "$DEB")
    for f in \
        ./usr/bin/mimizan \
        ./usr/bin/mimizan-gui \
        ./usr/share/mimizan/cameras/nikon_z_f.json \
        ./usr/share/mimizan/look/neutral.json \
        ./usr/share/applications/mimizan-gui.desktop \
        ./usr/share/doc/mimizan/README.md \
        ./usr/share/doc/mimizan/LICENSE.txt \
        ./usr/share/doc/mimizan/docs/SPEC.md \
        ./usr/share/doc/mimizan/docs/RESULTS.md
    do
        grep -q " $f\$" <<<"$LISTING" || { echo "ERROR: $f missing in deb"; exit 1; }
    done
    if grep -qE '\.git/|/target/|testdata/|\.nef|\.dng' <<<"$LISTING"; then
        echo "ERROR: source tree or RAW samples leaked into deb"; exit 1
    fi
    echo "==> $DEB ($(du -h "$DEB" | cut -f1))"
else
    echo "NOTE: cargo-deb not installed, .deb skipped (cargo install cargo-deb --locked)"
fi

echo
echo "Release assets (do not commit):"
ls -lh "$PROJECT_DIR"/dist/mimizan_*.deb "$PROJECT_DIR"/dist/mimizan-*.tar.gz 2>/dev/null || true
