#!/usr/bin/env bash
# make-source-tarball.sh — build a clean, reproducible source tarball of the
# publishable Moss Terminal tree.
#
# What goes in:
#     engine/  kitty-patches/  kitty-overlay/  scripts/  packaging/  specs/
#     tests/   docs/ (if present)  .github/
#     the top-level docs listed in TOPLEVEL_DOCS below, LICENSE, LICENSE.MIT,
#     .editorconfig, .gitattributes, .gitignore
#
# What is deliberately left out:
#     kitty/          — 220 MB generated build tree; regenerate it with
#                       scripts/moss-apply-patches.sh from the pinned upstream
#                       tarball plus kitty-patches/ + kitty-overlay/.
#     kitty.prev/     — the previous tree kept by moss-sync-kitty.sh --adopt
#     .sync-work/     — sync rehearsal scratch space (holds a kitty tarball)
#     engine/target/  — cargo build output
#     engine/pics/    — 13 MB of screenshots inherited from the engine's
#                       upstream project; nothing in the tree references them
#                       (see 文件清单.md). Pass --with-pics to keep them.
#     dist/           — our own output directory
#     .git/           — VCS metadata
#     __pycache__/  *.pyc  *.orig  *.rej  *.bak  .env  core dumps
#     top-level *.md that is not in TOPLEVEL_DOCS — the repository also holds
#                       internal development scaffolding (*提示词.md, 实施指南.md,
#                       工具规则.md, 全面测试异常清单.md). Those are notes to the
#                       people building this, not product documentation, and
#                       have no business in a release tarball.
#
# Determinism: the file list is sorted under LC_ALL=C, every entry is stamped
# with SOURCE_DATE_EPOCH (default: the fixed epoch below), ownership is forced
# to 0:0, group/other write bits are cleared, GNU tar format is used and gzip
# is invoked with -n so no timestamp lands in the gzip header. Running this
# twice on the same tree yields byte-identical output.
#
# Usage:
#   scripts/make-source-tarball.sh [--version X.Y.Z] [--output-dir DIR]
#                                  [--name NAME] [--with-pics]
#                                  [--stdout-sha-only]
#
# Exit codes: 0 ok, 1 usage/environment error.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Fixed fallback stamp: 2026-07-26T00:00:00Z, the 0.1.0 release date
# (`date -u -d 2026-07-26T00:00:00Z +%s`). Override with SOURCE_DATE_EPOCH
# (e.g. the tagged commit's date) for a per-release stamp that is still
# reproducible.
: "${SOURCE_DATE_EPOCH:=1785024000}"

VERSION=""
OUTPUT_DIR=""
NAME="moss-terminal"
SHA_ONLY=0
WITH_PICS=0

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; $d' >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) shift; VERSION="${1:?--version needs an argument}" ;;
        --output-dir) shift; OUTPUT_DIR="${1:?--output-dir needs an argument}" ;;
        --name) shift; NAME="${1:?--name needs an argument}" ;;
        --with-pics) WITH_PICS=1 ;;
        --stdout-sha-only) SHA_ONLY=1 ;;
        -h|--help) usage ;;
        *) echo "error: unknown option: $1" >&2; usage ;;
    esac
    shift
done

if [ -z "$VERSION" ]; then
    VERSION="$(git -C "$REPO_ROOT" describe --tags --abbrev=0 2>/dev/null | sed 's/^v//' || true)"
fi
if [ -z "$VERSION" ]; then
    # Fall back to the newest released version recorded in CHANGELOG.md.
    VERSION="$(sed -n 's/^## \[\([0-9][0-9.]*\)\].*/\1/p' "$REPO_ROOT/CHANGELOG.md" 2>/dev/null | head -n1 || true)"
fi
[ -n "$VERSION" ] || { echo "error: could not determine a version; pass --version X.Y.Z" >&2; exit 1; }

OUTPUT_DIR="${OUTPUT_DIR:-$REPO_ROOT/dist}"
PREFIX="$NAME-$VERSION"
TARBALL="$OUTPUT_DIR/$PREFIX.tar.gz"

for tool in tar gzip sha256sum find sort; do
    command -v "$tool" >/dev/null 2>&1 || { echo "error: required tool not found: $tool" >&2; exit 1; }
done
tar --version 2>/dev/null | head -n1 | grep -qi 'gnu tar' || {
    echo "error: GNU tar is required (found: $(tar --version 2>&1 | head -n1))" >&2
    exit 1
}

mkdir -p "$OUTPUT_DIR"

# ---------------------------------------------------------------------------
# Collect the file list.
# ---------------------------------------------------------------------------
INCLUDE_DIRS=(engine kitty-patches kitty-overlay scripts packaging specs tests docs .github)
LIST="$(mktemp)"
trap 'rm -f "$LIST" "$LIST.tmp"' EXIT

PICS_PRUNE=(-o -path './engine/pics')
[ "$WITH_PICS" -eq 1 ] && PICS_PRUNE=()

collect() {
    # $1: path relative to REPO_ROOT
    find "$1" \
        \( -path './kitty' -o -path './kitty.prev' -o -path './.sync-work' \
           -o -path './dist' -o -path './.git' \
           -o -path './engine/target' -o -name '__pycache__' \
           ${PICS_PRUNE[@]+"${PICS_PRUNE[@]}"} \) -prune -o \
        \( -type f -o -type l \) \
        ! -name '*.pyc' ! -name '*.orig' ! -name '*.rej' ! -name '*.bak' \
        ! -name '*.bak.*' ! -name '.env' ! -name 'core' ! -name '*.rs.bk' \
        ! -name '.DS_Store' \
        -print
}

cd "$REPO_ROOT"
: > "$LIST.tmp"

for d in "${INCLUDE_DIRS[@]}"; do
    [ -d "$d" ] || continue
    collect "./$d" >> "$LIST.tmp"
done

# Top-level files: docs, licences, dotfiles that belong in a source release.
#
# This is an ALLOWLIST on purpose. Shipping every top-level *.md would drag the
# repository's internal development scaffolding into the release. When you add a
# genuine product document, add it here; the check below tells you if you forgot.
TOPLEVEL_DOCS=(
    README.md
    README.en.md
    CHANGELOG.md
    CONTRIBUTING.md
    SECURITY.md
    NOTICE.md
    部署与使用指南.md
    项目概要.md
    文件清单.md
    验收报告.md
)

for f in "${TOPLEVEL_DOCS[@]}" LICENSE LICENSE.MIT \
         .editorconfig .gitattributes .gitignore; do
    [ -f "./$f" ] || continue
    printf './%s\n' "$f" >> "$LIST.tmp"
done

# Not fatal: a top-level *.md that is neither shipped nor known scaffolding is
# most likely a new product doc that nobody added to TOPLEVEL_DOCS.
for f in ./*.md; do
    [ -f "$f" ] || continue
    base="${f#./}"
    case " ${TOPLEVEL_DOCS[*]} " in *" $base "*) continue ;; esac
    case "$base" in *提示词.md|实施指南.md|工具规则.md|全面测试异常清单.md|install.sh) continue ;; esac
    echo "warning: top-level doc not shipped (add it to TOPLEVEL_DOCS if it should be): $base" >&2
done

LC_ALL=C sort -u "$LIST.tmp" | sed 's|^\./||' > "$LIST"

COUNT=$(wc -l < "$LIST")
[ "$COUNT" -gt 0 ] || { echo "error: refusing to build an empty tarball" >&2; exit 1; }

# Hard guard: nothing from the excluded set may have slipped through.
if LC_ALL=C grep -E '^(kitty|kitty\.prev|dist)/|^\.sync-work/|^engine/target/|/__pycache__/|\.(pyc|orig|rej)$|(^|/)\.env$' "$LIST" >/dev/null; then
    echo "error: excluded paths leaked into the file list:" >&2
    LC_ALL=C grep -E '^(kitty|kitty\.prev|dist)/|^\.sync-work/|^engine/target/|/__pycache__/|\.(pyc|orig|rej)$|(^|/)\.env$' "$LIST" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Build it.
# ---------------------------------------------------------------------------
tar --create \
    --format=gnu \
    --no-recursion \
    --files-from="$LIST" \
    --transform="s,^,$PREFIX/," \
    --owner=0 --group=0 --numeric-owner \
    --mtime="@$SOURCE_DATE_EPOCH" \
    --mode='go-w' \
    --sort=name \
    | gzip -9n > "$TARBALL"

SHA="$(sha256sum "$TARBALL" | cut -d' ' -f1)"
SIZE="$(wc -c < "$TARBALL")"

if [ "$SHA_ONLY" -eq 1 ]; then
    printf '%s\n' "$SHA"
    exit 0
fi

printf '\n'
printf 'tarball : %s\n' "$TARBALL"
printf 'prefix  : %s/\n' "$PREFIX"
printf 'files   : %s\n' "$COUNT"
printf 'size    : %s bytes\n' "$SIZE"
printf 'epoch   : %s (SOURCE_DATE_EPOCH)\n' "$SOURCE_DATE_EPOCH"
printf 'sha256  : %s\n' "$SHA"
printf '\n'
printf 'Verify with:\n'
printf '    echo "%s  %s" | sha256sum -c -\n' "$SHA" "$(basename "$TARBALL")"
printf '\n'
printf 'This tarball does NOT contain the kitty source tree. To build from it:\n'
printf '    tar xf %s && cd %s\n' "$(basename "$TARBALL")" "$PREFIX"
printf '    # fetch the pinned upstream kitty (see README.en.md), then:\n'
printf '    ./scripts/moss-apply-patches.sh /path/to/kitty-X.Y.Z && mv /path/to/kitty-X.Y.Z ./kitty\n'
