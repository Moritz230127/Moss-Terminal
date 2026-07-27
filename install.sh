#!/usr/bin/env bash
# Moss Terminal one-click installer.
#
#   curl -fsSL https://raw.githubusercontent.com/Moritz230127/Moss-Terminal/main/install.sh | bash
#
# What it does (user-local, no root, nothing outside ~/.local and $XDG dirs):
#   1. checks build dependencies (offers the exact pacman command on Arch)
#   2. downloads the pinned release source tarball + upstream kitty tarball,
#      verifies both sha256, applies the Moss patch series to kitty
#   3. builds engine + kitty and installs to ~/.local/share/moss-terminal
#      via scripts/moss-setup.py (launchers in ~/.local/bin)
#   4. opens the TUI configuration (moss-setup --configure) when on a TTY
#
# Arch users who prefer a pacman-managed package: pass --aur to build and
# install packaging/aur/PKGBUILD with makepkg -si instead.
#
# This file is intentionally NOT inside the release tarball: the tarball's
# sha256 is pinned below, which would be self-referential otherwise.

set -euo pipefail

REPO="Moritz230127/Moss-Terminal"
VERSION="0.1.0"
TARBALL_SHA256="2725128418bb1d7b2156eee371ba19ca503b82e622ec272cd3764465aac9af0b"
KITTY_VERSION="0.48.1"
KITTY_SHA256="aadb428e20ad678c0a7969c0a80c46f391b49addeb7fda57b06a14a4d102fb1d"
# Symbols Nerd Font Mono (OFL): kitty's build embeds it; fetched explicitly so
# the build does not depend on the font being installed system-wide.
FONT_SHA256="f0f624d9b474bea1662cf7e862d44aebe1ae1f6c7f9cb7a0ca5d0e5ac9561c60"

BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
say()  { printf '%s\n' "${BOLD}[moss-install]${RESET} $*"; }
ok()   { printf '%s\n' "${GREEN}[ok]${RESET} $*"; }
warn() { printf '%s\n' "${YELLOW}[warn]${RESET} $*"; }
die()  { printf '%s\n' "${RED}[error]${RESET} $*" >&2; exit 1; }

# When piped into bash, stdin is the script itself — prompts must use the
# terminal directly. Degrade to non-interactive when no TTY exists at all.
TTY_IN="/dev/tty"; INTERACTIVE=1
[ -r "$TTY_IN" ] && [ -t 1 ] || { INTERACTIVE=0; }
ask_yn() { # ask_yn "question" -> 0 yes / 1 no; default no
    [ "$INTERACTIVE" = 1 ] || return 1
    local a=''
    printf '%s' "$1 (y/N) " > "$TTY_IN"
    IFS= read -r a < "$TTY_IN" || true
    [ "$a" = y ] || [ "$a" = Y ]
}

MODE="local"
for arg in "$@"; do case "$arg" in
    --aur) MODE="aur" ;;
    --help|-h) sed -n '2,18p' "$0" 2>/dev/null || true; exit 0 ;;
    *) die "unknown option: $arg" ;;
esac; done

say "Moss Terminal v${VERSION} — 一键安装 / one-click install (${MODE} mode)"

# ── 1. dependencies ─────────────────────────────────────────────────────────
NEEDED=(curl tar python3 gcc pkg-config cargo go)
MISSING=()
for c in "${NEEDED[@]}"; do command -v "$c" >/dev/null 2>&1 || MISSING+=("$c"); done
if [ "${#MISSING[@]}" -gt 0 ]; then
    warn "缺少构建依赖 / missing build tools: ${MISSING[*]}"
    if command -v pacman >/dev/null 2>&1; then
        PKGS="base-devel curl rust go python pkgconf alsa-lib cairo simde harfbuzz libpng librsync lcms2 xxhash wayland libx11 libxkbcommon fontconfig openssl dbus"
        say "Arch 安装命令 / install with:"
        printf '    sudo pacman -S --needed %s\n' "$PKGS"
        if ask_yn "现在执行上述 pacman 命令? / run it now (needs sudo)?"; then
            # shellcheck disable=SC2086
            sudo pacman -S --needed $PKGS < "$TTY_IN" || die "pacman failed"
        else
            die "请先安装依赖后重试 / install the dependencies first, then re-run"
        fi
    else
        die "请先安装依赖后重试 / install them first (rust, go, python3, gcc, pkg-config, kitty build libs)"
    fi
fi
ok "构建依赖齐备 / build tools present"

# ── 2. workspace + downloads ────────────────────────────────────────────────
WORK=$(mktemp -d "${TMPDIR:-/tmp}/moss-install.XXXXXX")
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
cd "$WORK"

SRC_URL="https://github.com/${REPO}/releases/download/v${VERSION}/moss-terminal-${VERSION}.tar.gz"
KITTY_URL="https://github.com/kovidgoyal/kitty/releases/download/v${KITTY_VERSION}/kitty-${KITTY_VERSION}.tar.xz"

say "下载源码包 / downloading source tarball"
curl -fL --proto '=https' -o src.tar.gz "$SRC_URL"
printf '%s  src.tar.gz\n' "$TARBALL_SHA256" | sha256sum -c - >/dev/null || die "源码包校验失败 / source tarball checksum mismatch"
ok "moss-terminal-${VERSION}.tar.gz sha256 verified"

say "下载 kitty ${KITTY_VERSION} / downloading upstream kitty"
curl -fL --proto '=https' -o kitty.tar.xz "$KITTY_URL"
printf '%s  kitty.tar.xz\n' "$KITTY_SHA256" | sha256sum -c - >/dev/null || die "kitty 源码包校验失败 / kitty tarball checksum mismatch"
ok "kitty-${KITTY_VERSION}.tar.xz sha256 verified"

tar xf src.tar.gz
SRCDIR="$WORK/moss-terminal-${VERSION}"
[ -d "$SRCDIR" ] || die "unexpected tarball layout"

if [ "$MODE" = aur ]; then
    # ── pacman-managed path ─────────────────────────────────────────────────
    command -v makepkg >/dev/null 2>&1 || die "--aur 需要 makepkg / requires makepkg (Arch)"
    say "makepkg -si（将请求 sudo 安装）/ building the pacman package"
    BUILDDIR="$WORK/aurbuild"; mkdir "$BUILDDIR"
    cp "$SRCDIR"/packaging/aur/PKGBUILD "$SRCDIR"/packaging/aur/moss-terminal.install "$BUILDDIR/"
    cp src.tar.gz "$BUILDDIR/moss-terminal-${VERSION}.tar.gz"
    cp kitty.tar.xz "$BUILDDIR/kitty-${KITTY_VERSION}.tar.xz"
    ( cd "$BUILDDIR" && makepkg -si --noconfirm < "${TTY_IN}" )
    ok "已通过 pacman 安装 / installed via pacman"
    BIN_HINT="moss-terminal"
else
    # ── user-local path ─────────────────────────────────────────────────────
    say "物料化 kitty 源码树（打补丁）/ materialising the patched kitty tree"
    tar xf kitty.tar.xz
    "$SRCDIR/scripts/moss-apply-patches.sh" "$WORK/kitty-${KITTY_VERSION}" >/dev/null
    say "下载内嵌字体 / fetching the bundled Symbols Nerd Font"
    curl -fL --proto '=https' -o SymbolsNerdFontMono-Regular.ttf         "https://github.com/${REPO}/releases/download/v${VERSION}/SymbolsNerdFontMono-Regular.ttf"
    printf '%s  SymbolsNerdFontMono-Regular.ttf
' "$FONT_SHA256" | sha256sum -c - >/dev/null         || die "字体校验失败 / font checksum mismatch"
    mkdir -p "$WORK/kitty-${KITTY_VERSION}/fonts"
    mv SymbolsNerdFontMono-Regular.ttf "$WORK/kitty-${KITTY_VERSION}/fonts/"
    mv "$WORK/kitty-${KITTY_VERSION}" "$SRCDIR/kitty"
    ok "补丁序列应用完成 / patch series applied"

    say "编译并安装（引擎两次 cargo build + kitty linux-package，需要几分钟）/ building"
    python3 "$SRCDIR/scripts/moss-setup.py" --yes --skip-provider-wizard
    BIN_HINT="moss-terminal   (二进制在 ~/.local/bin / binaries in ~/.local/bin)"
fi

# ── 4. configuration TUI ────────────────────────────────────────────────────
say "安装完成 / install complete"
MOSS_SETUP="$HOME/.local/bin/moss-setup"; command -v moss-setup >/dev/null 2>&1 && MOSS_SETUP=moss-setup
if [ "$INTERACTIVE" = 1 ] && ask_yn "现在打开 TUI 配置界面（选择 provider/模型/密钥）? / open the config TUI now?"; then
    "$MOSS_SETUP" --configure < "$TTY_IN" > "$TTY_IN" 2>&1 || warn "TUI 退出异常，可稍后运行 moss-setup --configure / re-run later"
else
    say "稍后可运行 / configure later with:  moss-setup --configure"
fi

ok "启动 / launch:  $BIN_HINT"
ok "用法 / usage:   在提示符输入 》你的问题 / type 》your question at the prompt"
