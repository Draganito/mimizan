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
done

exit "$fail"
