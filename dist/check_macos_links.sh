#!/usr/bin/env bash
# Fail if an Apple silicon binary links anything that is not part of macOS.
# Homebrew, libomp and libstdc++ are the ones that would make the program
# depend on a library the machine does not already have.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: dist/check_macos_links.sh <binary>..." >&2
    exit 2
fi

fail=0
for bin in "$@"; do
    [[ -f "$bin" ]] || { echo "ERROR: $bin missing"; fail=1; continue; }
    echo "==> $bin"
    otool -L "$bin"
    # The first line is the binary's own install name.
    while read -r lib _; do
        case "$lib" in
            /usr/lib/*|/System/*) ;;
            *)
                echo "ERROR: $bin links $lib"
                fail=1
                ;;
        esac
    done < <(otool -L "$bin" | tail -n +2)

    # Apple's App Store scan (guideline 2.5.1) rejects any binary that
    # *imports* these private CoreGraphics symbols, whether they are
    # called or not. winit 0.30.13 imported them; the crates-io patch
    # in Cargo.toml removes the import. This is the check that the
    # store package is not shipping them again.
    if nm -u "$bin" | grep -E '_CGS(SetWindowBackgroundBlurRadius|MainConnectionID)'; then
        echo "ERROR: $bin imports a private CGS symbol (App Store 2.5.1)"
        fail=1
    fi
done

exit "$fail"
