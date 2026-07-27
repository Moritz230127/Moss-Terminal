#!/usr/bin/env bash
# moss-start.sh — run Moss Terminal from a source checkout (dev use).
#
# There is no daemon and nothing to build here: the Rust engine is a cdylib
# (engine/target/release/libmoss.so) that kitty dlopen()s into its own
# process, located via the MOSS_ENGINE_LIB env var. This script just points
# that env var at the source-tree build and execs the `make debug` kitty
# launcher (kitty/kitty/launcher/kitty) in its place.
#
# For a real install (compiled release kitty + engine, ~/.local/bin wrapper,
# desktop entry, provider config) use scripts/moss-setup.py instead. This
# script is only for iterating on a source checkout.
#
# Usage:
#   scripts/moss-start.sh [kitty args...]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIBMOSS="$REPO_ROOT/engine/target/release/libmoss.so"
LAUNCHER="$REPO_ROOT/kitty/kitty/launcher/kitty"

if [ -f "$LIBMOSS" ]; then
    export MOSS_ENGINE_LIB="$LIBMOSS"
else
    echo "warning: engine library not found at $LIBMOSS" >&2
    echo "         run: (cd engine && cargo build --release --no-default-features)" >&2
    echo "         continuing without it -- kitty will start but 》 questions will fail" >&2
fi

if [ ! -x "$LAUNCHER" ]; then
    echo "error: kitty launcher not found or not executable: $LAUNCHER" >&2
    echo "       run: (cd kitty && make debug)" >&2
    exit 1
fi

# NOTE: do not set MOSS_TERMINAL here -- kitty sets it for its own children
# (kitty/kitty/child.py), scripts must not set it themselves.
exec "$LAUNCHER" "$@"
