<!-- 中文版本 / Chinese version: [README.md](./README.md) -->

*English · [中文](./README.md)*

# Moss Terminal

**A kitty fork with an AI engine loaded inside the terminal process — type `》your question` at any shell prompt and get a streamed answer that already knows what you just ran.**

No split panes, no side window, no daemon, no copy-pasting your scrollback into a chat box. The AI sees the commands and output of *this* window because it is running *in* this window's terminal emulator.

---

## Demo

<!-- TODO(maintainer): record a terminal session and drop the file at docs/assets/demo.gif,
     then replace this comment block with:  ![Moss Terminal demo](docs/assets/demo.gif)
     Suggested capture: `asciinema rec`, then `agg demo.cast docs/assets/demo.gif`.
     Script it as: run a failing `gcc main.c`, then type 》why did this fail. -->

```console
$ gcc main.c
main.c: In function ‘main’:
main.c:3:5: error: expected ‘;’ before ‘return’

$ 》why did this fail
  ⋯ thinking (green) ⋯
  The compiler is telling you that line 2 of main.c is missing its
  terminating semicolon. Add one after the printf call:
      printf("hi\n");
$ █
```

---

## Features

- **`》` is the whole UI** — type `》question`, press Enter. No hotkey, no mode switch, no separate app.
- **Free context** — the engine already holds this window's commands and their output. You never paste anything.
- **Incremental multi-turn** — each question ships only the terminal text produced *since the last answer*, while the full conversation history is kept server-side in the engine.
- **Per-window isolation** — every kitty window is an independent session with its own context and its own `conversation.db`. Three windows, three unrelated conversations, zero cross-talk.
- **Streamed, colour-coded output** — reasoning in green, tool calls in blue, the answer in the default colour, written straight into the current window. `Ctrl+C` cancels at any point.
- **Zero daemons, zero sockets** — the engine is a `.so` that the kitty process `dlopen`s. Nothing to start, nothing to keep alive, nothing to leak.
- **Tools, memory, knowledge base** — the engine can run shell commands (opt-in, see [SECURITY.md](./SECURITY.md)), read files, keep long-term memory, and search an archived-turn FTS index.
- **Multi-provider** — any OpenAI-compatible API. Ships with a zero-key public endpoint so it works before you configure anything (read the privacy note below).
- **Stays in sync with upstream kitty** — the fork is a 10-patch series (~146 lines) plus 4 new files, re-applied and re-tested against each new kitty release by `scripts/moss-sync-kitty.sh`.

---

## How it works

```
┌──────────────────────────────────────────────────────────────────────┐
│  the kitty process — one process, no children, no daemon             │
│                                                                      │
│  Layer 1 · kitty C core   (10 patches + moss_hook.c / moss_hook.h)   │
│     screen_linefeed / prompt marking / OSC 7  →  moss_on_* hooks     │
│     OSC 7717 (Moss control sequence)          →  moss_handle_osc     │
│     dlopen($MOSS_ENGINE_LIB, else libmoss.so); missing lib = no-op   │
│                       │  direct function-pointer calls, non-blocking │
│  Layer 2 · kitty Python  (kitty/moss_integration.py)                 │
│     ctypes bindings; a 30 ms timer drains engine output and injects  │
│     it through the Screen's own VT parser; newline sentinel unblocks │
│     the waiting shell                                                │
│                       │  ctypes FFI — 13 exported moss_* symbols     │
│  Layer 3 · Moss Engine  (Rust: `moss` CLI + `libmoss.so` cdylib)     │
│     per-window sessions · incremental context · tokio · streaming    │
│     tools / knowledge base / memory / providers / cache tiering      │
└──────────────────────────────────────────────────────────────────────┘
```

The `》` round trip: shell integration (zsh `accept-line` widget, fish `\r`
binding, bash `command_not_found_handle` — all active only when
`MOSS_TERMINAL=1`) intercepts the line, base64-encodes it into an
`OSC 7717;ask;<b64>` sequence, and blocks in a silent `read`. kitty's VT parser
routes that to the engine; answer bytes stream back into the screen; a final
newline sentinel releases the `read` and the prompt returns.

Build with `MOSS_ENABLED=0` and every C hook compiles to an empty function —
that build is plain, unmodified kitty.

---

## Install

### Requirements

Build: `cargo` + `rustc` (stable), `python3 >= 3.8`, a C compiler (`gcc`/`clang`), `go` (kitty's kittens), `pkg-config`, `make`.
Runtime/link (kitty's own): cairo, dbus, freetype2, fontconfig, harfbuzz, lcms2, libgl, libpng, librsync, libx11, libxcursor, libxkbcommon(+-x11), libxi, openssl, wayland, xxhash, zlib.
The engine needs no system libraries beyond libc for the embedded `libmoss.so`; the `moss` CLI additionally links `alsa-lib`.

On Arch:

```bash
sudo pacman -S --needed rust go python cairo dbus freetype2 fontconfig harfbuzz \
    lcms2 libgl libpng librsync libx11 libxcursor libxkbcommon libxkbcommon-x11 \
    libxi openssl simde wayland wayland-protocols xxhash zlib alsa-lib base-devel
```

`simde` is a build-only header dependency: `kitty/simd-string-impl.h` includes
`<simde/x86/avx2.h>` unconditionally on x86-64 and ARM64 targets and kitty does
not vendor it. Without it the kitty compile stage fails with a bare
"No such file or directory" and no hint.

Moss Terminal installs under its own name (`moss-terminal`) and its own prefix.
It never replaces or conflicts with a stock `kitty` install, and it ships its own
patched shell integration rather than using the `kitty-shell-integration` package.

### From source (recommended today)

```bash
git clone https://github.com/Moritz230127/Moss-Terminal.git
cd moss-terminal

# The kitty source tree is NOT vendored — materialise it first (see below)
./scripts/moss-apply-patches.sh /path/to/kitty-0.48.1   # then move/symlink it to ./kitty

python3 scripts/moss-setup.py            # interactive installer
python3 scripts/moss-setup.py --dry-run  # preview only: builds nothing, writes nothing
```

`moss-setup.py` runs seven stages: dependency check → build the engine twice →
build kitty (`python3 setup.py linux-package`) → install into
`~/.local/share/moss-terminal` (+ `~/.local/bin/moss` and
`~/.local/bin/moss-terminal`) → configure an AI provider → write a desktop entry
→ print a summary of every path it touched. `--prefix DIR` changes the install
prefix; `--yes` auto-confirms the review step (it never invents an API key).

Then run `moss-terminal`, or pick "Moss Terminal" from your application menu.

### The `kitty/` tree is not in this repository

The published repo deliberately does **not** vendor the 220 MB kitty working
tree: it is 100 % regenerable from `kitty-patches/` + `kitty-overlay/`, which are
the source of truth. To materialise it:

```bash
# 1. fetch the pinned upstream release (verify the checksum!)
curl -LO https://github.com/kovidgoyal/kitty/releases/download/v0.48.1/kitty-0.48.1.tar.xz
echo 'aadb428e20ad678c0a7969c0a80c46f391b49addeb7fda57b06a14a4d102fb1d  kitty-0.48.1.tar.xz' | sha256sum -c -
tar xf kitty-0.48.1.tar.xz

# 2. apply the Moss patch series + overlay files
./scripts/moss-apply-patches.sh kitty-0.48.1 --check   # dry run: does it still apply?
./scripts/moss-apply-patches.sh kitty-0.48.1           # for real

# 3. adopt it as the build tree
mv kitty-0.48.1 kitty
```

Or let `scripts/moss-sync-kitty.sh` do all of it (see [Upgrades](#upgrades)).

### Arch Linux / AUR

The packaging lives in [`packaging/aur/`](./packaging/aur/): `PKGBUILD` (release
tarball), `PKGBUILD-git` (VCS variant), `.SRCINFO`, and `moss-terminal.install`.

> **Not submitted to the AUR yet**, so `paru -S moss-terminal` does not work
> today. Build the package locally from this repository instead:
>
> ```bash
> cd packaging/aur
> sed -i 's|OWNER|your-github-user|g' PKGBUILD PKGBUILD-git README.md .SRCINFO
> makepkg -si            # release build; or: makepkg -si -p PKGBUILD-git
> ```
>
> Read [packaging/aur/README.md](./packaging/aur/README.md) first — it documents
> the three values (`OWNER`, `pkgver`, `sha256sums`) that must be substituted
> before the package builds or can be submitted, and why `pkgver` is *not* the
> `engine/Cargo.toml` crate version.

Once the package is on the AUR the usual `paru -S moss-terminal` /
`yay -S moss-terminal` will apply. Until then, the from-source instructions
above are the supported path.

---

## Configuration

Everything lives in XDG directories.

```bash
moss config paths        # print every path the engine uses
moss paths               # same
```

`MOSS_HOME=<dir>` relocates the whole tree under `<dir>/{config,data,cache,state,…}`.
Note that this override is implemented on the **embedded** (FFI) path only —
`engine/src/ffi/runtime.rs` — so it affects `libmoss.so` inside kitty, which is
what the test suites use to sandbox themselves. The standalone `moss` CLI always
uses the XDG directories, so `moss config paths` reports the XDG locations
regardless of `MOSS_HOME`.

| What | Where |
|------|-------|
| Config | `~/.config/moss/config.jsonc` |
| API keys | `~/.config/moss/.env` (mode `0600`) |
| Personas | `~/.config/moss/prompts/` |
| Logs | `~/.cache/moss/logs/` |
| Conversations | the engine's data dir (see `moss config paths`) |

### `config.jsonc`

```jsonc
{
  "active_provider": "deepseek",
  "providers": [
    {
      "id": "opencode",
      "display_name": "opencode Zen",
      "base_url": "https://opencode.ai/zen/v1",
      "models": ["big-pickle"],
      "default_model": "big-pickle"
      // public free endpoint, no api_key needed
    },
    {
      "id": "deepseek",
      "display_name": "DeepSeek",
      "base_url": "https://api.deepseek.com",
      "api_key": "$env:MOSS_DEEPSEEK_API_KEY",
      "models": ["deepseek-chat", "deepseek-reasoner"],
      "default_model": "deepseek-chat"
    }
  ],
  "prompt": { "active_persona": "" }
}
```

`api_key` supports `$env:VAR` indirection, resolved from the process environment
first and `~/.config/moss/.env` second:

```bash
mkdir -p ~/.config/moss
printf 'MOSS_DEEPSEEK_API_KEY="sk-..."\n' >> ~/.config/moss/.env
chmod 600 ~/.config/moss/.env
```

Reload config without restarting kitty — run this inside any Moss window:

```bash
printf '\033]7717;reload-config\007'
```

Idle sessions pick the new config up on their next question; an in-flight stream
finishes on the old one.

> **Privacy:** with no provider configured, questions go to the built-in
> zero-key public endpoint (`opencode.ai`) — **your terminal context is sent
> there**. The engine prints an inline notice the first time this happens.
> Configure your own provider before asking anything sensitive. See
> [SECURITY.md](./SECURITY.md).

---

## Usage

There is no keybinding to learn. The trigger is the character `》`
(U+300B RIGHT DOUBLE ANGLE BRACKET), typed at the start of a shell line.

| Action | How |
|--------|-----|
| Ask | `》why did that fail` then Enter |
| Cancel a running answer | `Ctrl+C` |
| New session | open a new kitty window/tab |
| Reload config | `printf '\033]7717;reload-config\007'` |
| Run vanilla kitty instead | run your system `kitty`, or build with `MOSS_ENABLED=0` |

Shell support: **zsh** and **fish** intercept the line *before* the shell parses
it, so the question text reaches the engine byte-for-byte. **bash** is
intercepted via `command_not_found_handle`, i.e. *after* parsing — quoting,
globs and extra whitespace are mangled, and a compound line such as
`》q; rm -rf x` has its tail executed by bash. Prefer zsh or fish for anything
non-trivial; see [SECURITY.md](./SECURITY.md#security-model).

zsh and bash apply a 300 s watchdog (`read -t 300`) so a hung network cannot
strand your prompt. fish's `read` has no timeout — use `Ctrl+C` there.

---

## Upgrades

`scripts/moss-sync-kitty.sh` re-applies the patch series onto a new kitty
release, builds it, and runs both test suites — entirely outside the repository
until you say otherwise.

```bash
./scripts/moss-sync-kitty.sh --notify-only        # only report that a newer kitty exists
./scripts/moss-sync-kitty.sh --check-only         # do the patches still apply? no build
./scripts/moss-sync-kitty.sh                      # fetch → patch → build → test, in .sync-work/
./scripts/moss-sync-kitty.sh --version 0.49.0     # pin the target version explicitly
./scripts/moss-sync-kitty.sh --skip-engine        # kitty-only change, reuse the built engine
./scripts/moss-sync-kitty.sh --adopt              # after a green run: atomically swap into kitty/
```

`--adopt` is the *only* mode that modifies `kitty/`, and only after download,
patching, build, `./test.py` and `./test.py --module moss` have all succeeded.
The previous tree is kept as `kitty.prev/`. On a patch conflict the script names
the exact `.patch` file so you can rebase it.

Optional, never-installed-automatically automation lives in `packaging/`: a
pacman hook that flags a pending kitty upgrade, and a systemd **user** timer that
runs `--notify-only` daily. See [packaging/README.md](./packaging/README.md).

---

## Development

```bash
# Engine — ORDER MATTERS.
cd engine
cargo build --release                       # (1) CLI binary: target/release/moss
cargo build --release --no-default-features # (2) embedded cdylib: target/release/libmoss.so
```

Step (2) must come second: it re-links `libmoss.so` without the audio /
raw-mode / process-exit machinery that the CLI needs. Reversing the order leaves
an ALSA-linked `libmoss.so` inside your terminal process.

```bash
# kitty (after materialising kitty/ as described above)
cd kitty && make debug
MOSS_ENABLED=0 make debug          # vanilla kitty, all hooks compiled out

# Run from the source tree without installing anything
./scripts/moss-start.sh
```

### Tests

```bash
cd engine
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings     # must be zero warnings
cargo test                                    # 513 tests at the time of writing

cd ../kitty
./test.py                                     # kitty's full suite
./test.py --module moss                       # the Moss integration module

# End-to-end, from the repo root
python3 tests/e2e_mock_llm.py                 # full loop vs. a local mock LLM, sandboxed
python3 tests/e2e_answer_leak.py              # answers must not leak into the next increment
kitty/kitty/launcher/kitty +launch tests/e2e_run_command_embedded.py
kitty/kitty/launcher/kitty +runpy "exec(open('tests/e2e_kitty_capture.py').read())"
```

The last two need kitty's own Python interpreter (they exercise the real C hook
and kitty's process-wide child reaping). All e2e suites redirect every embedded
engine path through `MOSS_HOME` into a temp dir and talk only to `127.0.0.1`.

See [CONTRIBUTING.md](./CONTRIBUTING.md) for the patch-series workflow — **never
edit `kitty/` directly.**

---

## Project layout

```
engine/          Moss Engine (Rust): `moss` CLI + embedded libmoss.so    [MIT]
kitty-patches/   10 patches against upstream kitty (~146 lines)          [GPL-3.0]
kitty-overlay/   4 new files copied into the kitty tree                  [GPL-3.0]
scripts/         moss-setup.py, moss-start.sh, moss-sync-kitty.sh,
                 moss-apply-patches.sh, make-source-tarball.sh
packaging/       optional pacman hook + systemd user timer (never auto-installed)
packaging/aur/   PKGBUILD, PKGBUILD-git, .SRCINFO, moss-terminal.install
tests/           4 end-to-end suites driving libmoss.so
specs/           requirements specification (Chinese)
kitty/           generated build tree — gitignored, regenerable, NOT distributed
```

Chinese documentation: [README.md](./README.md) ·
[项目概要.md](./项目概要.md) (architecture) ·
[部署与使用指南.md](./部署与使用指南.md) (deploy & use) ·
[文件清单.md](./文件清单.md) (file-by-file) ·
[验收报告.md](./验收报告.md) (acceptance report) ·
[specs/需求规格.md](./specs/需求规格.md).

---

## Licence

Moss Terminal as distributed is **GPL-3.0-or-later**, because it is a derivative
of kitty. The `engine/` directory taken on its own is **MIT**.

Read [NOTICE.md](./NOTICE.md) for the exact per-path breakdown —
[`LICENSE`](./LICENSE) (GPL-3.0) and [`LICENSE.MIT`](./LICENSE.MIT) are the
licence texts.

## Acknowledgements

- **[kitty](https://github.com/kovidgoyal/kitty)** by Kovid Goyal — the terminal
  core this project is built on. Moss Terminal is an unofficial fork; please do
  not report its bugs upstream.
- **Miyu / Moss Engine** by SHORiN-KiWATA — the Rust engine in `engine/`
  originates from it.
- **DeepSeek-Reasonix** — the layered context-cache strategy took its cues from
  Reasonix's approach. No Reasonix code is included here.

---

<sub>**`Moritz230127/Moss-Terminal` is a placeholder** occurring in `CHANGELOG.md`,
`README.en.md`, `CONTRIBUTING.md`, `SECURITY.md`,
`.github/ISSUE_TEMPLATE/{config.yml,bug_report.yml}` and
`packaging/aur/{PKGBUILD,PKGBUILD-git,.SRCINFO,README.md}`. The ones in
`config.yml` render as links on GitHub's New Issue page and will 404 —
including the security-advisory link — until substituted, so do this in the
first commit after creating the repository:
`grep -rl 'Moritz230127/Moss-Terminal' . --exclude-dir={.git,kitty,target,.sync-work} | xargs sed -i 's|Moritz230127/Moss-Terminal|youruser/yourrepo|g'`
then `grep -rn 'OWNER/\|example.invalid' . --exclude-dir={.git,kitty,target,.sync-work}` must print nothing
(`moritz001@163.com` in `SECURITY.md` needs a real address or removal too).</sub>
