#!/usr/bin/env bash
# moss-apply-patches.sh — turn a pristine kitty source tree into a Moss
# Terminal tree by applying the patch series and copying the overlay files.
#
# Usage: moss-apply-patches.sh <kitty-source-dir> [--check]
#   --check  dry-run: report whether every patch applies, change nothing.
#
# Exit codes: 0 ok, 1 usage error, 2 patch conflict (details on stderr).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PATCH_DIR="$REPO_ROOT/kitty-patches"
OVERLAY_DIR="$REPO_ROOT/kitty-overlay"

usage() { echo "usage: $0 <kitty-source-dir> [--check]" >&2; exit 1; }

[ $# -ge 1 ] || usage
TARGET="$1"; shift
CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1
[ -f "$TARGET/kitty/screen.c" ] || { echo "error: $TARGET does not look like a kitty source tree" >&2; exit 1; }

conflicts=0
for p in "$PATCH_DIR"/*.patch; do
    name=$(basename "$p")
    if patch -p1 -d "$TARGET" --dry-run --silent < "$p" >/dev/null 2>&1; then
        if [ "$CHECK" -eq 0 ]; then
            patch -p1 -d "$TARGET" --silent < "$p"
            echo "applied: $name"
        else
            echo "ok:      $name"
        fi
    else
        echo "CONFLICT: $name — upstream changed this area; manual rebase needed:" >&2
        patch -p1 -d "$TARGET" --dry-run < "$p" 2>&1 | sed 's/^/    /' >&2 || true
        conflicts=$((conflicts + 1))
    fi
done

if [ "$CHECK" -eq 0 ] && [ "$conflicts" -eq 0 ]; then
    (cd "$OVERLAY_DIR" && find . -type f -print0) | while IFS= read -r -d '' f; do
        mkdir -p "$TARGET/$(dirname "$f")"
        cp "$OVERLAY_DIR/$f" "$TARGET/$f"
        echo "overlay: ${f#./}"
    done
fi

if [ "$conflicts" -gt 0 ]; then
    echo "$conflicts patch(es) failed to apply" >&2
    exit 2
fi
echo "done: $TARGET is now a Moss Terminal source tree"
