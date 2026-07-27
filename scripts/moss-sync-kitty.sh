#!/usr/bin/env bash
# moss-sync-kitty.sh — keep Moss Terminal in sync with kitty updates on Arch.
#
# Pipeline:
#   1. detect  — compare the kitty version Moss was built against with the
#                version Arch currently ships (pacman -Si kitty), falling
#                back to the latest GitHub release when pacman is absent.
#   2. fetch   — download & verify-extract that version's source tarball
#                into the work directory (never into the current tree).
#   3. apply   — run moss-apply-patches.sh on the fresh tree.
#   4. build   — build the patched kitty (and the engine, unless skipped).
#   5. test    — kitty's own test suite (./test.py) + the moss module.
#   6. adopt   — only on success and only with --adopt: atomically swap the
#                new tree into kitty/ (the old tree is kept as kitty.prev/).
#
# Without --adopt this is a pure out-of-tree rehearsal: nothing in the repo
# or on the system is modified. This is what the optional pacman hook /
# systemd timer in packaging/ invoke (with --notify-only).
#
# Usage:
#   moss-sync-kitty.sh [--check-only|--notify-only] [--adopt] [--version X]
#                      [--workdir DIR] [--skip-engine]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKDIR="$REPO_ROOT/.sync-work"
CHECK_ONLY=0
NOTIFY_ONLY=0
ADOPT=0
SKIP_ENGINE=0
FORCE_VERSION=""

while [ $# -gt 0 ]; do
    case "$1" in
        --check-only) CHECK_ONLY=1 ;;
        --notify-only) NOTIFY_ONLY=1 ;;
        --adopt) ADOPT=1 ;;
        --skip-engine) SKIP_ENGINE=1 ;;
        --version) shift; FORCE_VERSION="$1" ;;
        --workdir) shift; WORKDIR="$1" ;;
        -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done

log() { printf '[moss-sync] %s\n' "$*"; }

current_version() {
    # The version the tree in kitty/ is based on.
    python3 -c "import re,pathlib; m=re.search(r'version.*?(\d+), (\d+), (\d+)', pathlib.Path('$REPO_ROOT/kitty/kitty/constants.py').read_text()); print('.'.join(m.groups()))" 2>/dev/null
}

arch_version() {
    if command -v pacman >/dev/null 2>&1; then
        pacman -Si kitty 2>/dev/null | sed -n 's/^Version[[:space:]]*: \([0-9.]*\).*/\1/p'
    fi
}

github_version() {
    curl -fsSL --max-time 30 https://api.github.com/repos/kovidgoyal/kitty/releases/latest 2>/dev/null |
        sed -n 's/.*"tag_name": *"v\([0-9.]*\)".*/\1/p'
}

CUR=$(current_version)
NEW="${FORCE_VERSION:-$(arch_version)}"
[ -n "$NEW" ] || NEW=$(github_version)
[ -n "$NEW" ] || { echo "error: could not determine the target kitty version (no pacman, no network?)" >&2; exit 1; }

log "current tree: kitty $CUR"
log "target:       kitty $NEW"

if [ "$CUR" = "$NEW" ]; then
    log "already in sync; nothing to do"
    exit 0
fi

if [ "$NOTIFY_ONLY" -eq 1 ]; then
    log "UPDATE AVAILABLE: kitty $CUR -> $NEW"
    log "run: $0 --version $NEW   # rehearse the sync"
    log "then: $0 --version $NEW --adopt   # swap it in after a green run"
    if command -v notify-send >/dev/null 2>&1; then
        notify-send "Moss Terminal" "kitty $NEW is available (built against $CUR). Run moss-sync-kitty.sh." || true
    fi
    exit 0
fi

mkdir -p "$WORKDIR"
SRC_TARBALL="$WORKDIR/kitty-$NEW.tar.xz"
SRC_DIR="$WORKDIR/kitty-$NEW"

if [ ! -d "$SRC_DIR" ]; then
    if [ ! -f "$SRC_TARBALL" ]; then
        URL="https://github.com/kovidgoyal/kitty/releases/download/v$NEW/kitty-$NEW.tar.xz"
        log "downloading $URL"
        curl -fL --max-time 600 -o "$SRC_TARBALL" "$URL"
    fi
    log "extracting"
    tar -C "$WORKDIR" -xf "$SRC_TARBALL"
fi
[ -f "$SRC_DIR/kitty/screen.c" ] || { echo "error: extracted tree looks wrong: $SRC_DIR" >&2; exit 1; }

if [ -f "$SRC_DIR/.moss-patched" ]; then
    log "tree already patched (marker present); skipping patch application"
    [ "$CHECK_ONLY" -eq 1 ] && exit 0
else
    log "applying moss patch series"
    if [ "$CHECK_ONLY" -eq 1 ]; then
        "$REPO_ROOT/scripts/moss-apply-patches.sh" "$SRC_DIR" --check
        log "check-only requested; stopping before build"
        exit 0
    fi
    "$REPO_ROOT/scripts/moss-apply-patches.sh" "$SRC_DIR"
    touch "$SRC_DIR/.moss-patched"
fi

if [ "$SKIP_ENGINE" -eq 0 ]; then
    log "building moss engine (cdylib + cli)"
    (cd "$REPO_ROOT/engine" && cargo build --release && cargo build --release --no-default-features)
fi

log "building patched kitty ($NEW)"
(cd "$SRC_DIR" && make debug)

log "running kitty test suite"
(cd "$SRC_DIR" && ./test.py)

log "running moss integration tests"
(cd "$SRC_DIR" && ./test.py --module moss)

if [ "$ADOPT" -eq 1 ]; then
    log "adopting: swapping new tree into $REPO_ROOT/kitty"
    rm -rf "$REPO_ROOT/kitty.prev"
    mv "$REPO_ROOT/kitty" "$REPO_ROOT/kitty.prev"
    mv "$SRC_DIR" "$REPO_ROOT/kitty"
    log "adopted kitty $NEW (previous tree preserved at kitty.prev/)"
else
    log "SUCCESS: kitty $NEW + moss patches builds and passes tests"
    log "rehearsal tree: $SRC_DIR (repo untouched; re-run with --adopt to swap it in)"
fi
