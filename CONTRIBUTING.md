# Contributing to Moss Terminal

Thanks for wanting to help. Moss Terminal is a fork of kitty with an embedded
Rust engine, so it has two rulesets stacked on top of each other: **kitty's**
(for anything under the kitty tree) and **ours** (for `engine/` and the
tooling). Please read the workflow section before touching anything, because
the most common first mistake — editing `kitty/` — silently destroys your work.

- [Getting a working tree](#1-getting-a-working-tree)
- [Building](#2-building)
- [The patch-series workflow](#3-the-patch-series-workflow--read-this)
- [Coding standards](#4-coding-standards)
- [Running the tests](#5-running-the-tests)
- [Commits, sign-off, pull requests](#6-commits-sign-off-pull-requests)

---

## 1. Getting a working tree

The repository does **not** contain the 220 MB kitty source tree. It is
generated, `.gitignore`d, and never committed. `kitty-patches/` and
`kitty-overlay/` are the source of truth.

```bash
git clone https://github.com/Moritz230127/Moss-Terminal.git
cd moss-terminal

curl -LO https://github.com/kovidgoyal/kitty/releases/download/v0.48.1/kitty-0.48.1.tar.xz
echo 'aadb428e20ad678c0a7969c0a80c46f391b49addeb7fda57b06a14a4d102fb1d  kitty-0.48.1.tar.xz' | sha256sum -c -
tar xf kitty-0.48.1.tar.xz

# keep a pristine copy — you will need it to regenerate patches
cp -a kitty-0.48.1 kitty-pristine

./scripts/moss-apply-patches.sh kitty-0.48.1
mv kitty-0.48.1 kitty
```

`kitty-pristine/` is also gitignored. Keeping it around is strongly recommended;
without it, regenerating a patch means re-downloading the tarball.

Build requirements: stable Rust (`cargo`, `rustc`), `python3 >= 3.8`, a C
compiler, `go`, `pkg-config`, `make`, plus kitty's own build dependencies — the
full list is in [README.en.md](./README.en.md#requirements).

## 2. Building

```bash
# Engine — the order of these two commands matters.
cd engine
cargo build --release                        # (1) CLI binary   → target/release/moss
cargo build --release --no-default-features  # (2) embedded lib → target/release/libmoss.so
```

Step (2) **must** run after step (1). The default feature set (`cli`) links
`rodio`/ALSA and the raw-mode and process-exit machinery that a REPL needs and an
in-process library must never have. Both builds write into the same
`target/release/`, so running them in the other order leaves an ALSA-linked
`libmoss.so` that then gets loaded into your terminal emulator.

```bash
cd ../kitty
make debug                    # patched kitty
MOSS_ENABLED=0 make debug     # vanilla kitty from the same tree (every hook a no-op)

cd ..
./scripts/moss-start.sh       # run from the source tree, installing nothing
```

## 3. The patch-series workflow — read this

> **Never edit `kitty/` directly and expect it to survive.**
> `scripts/moss-sync-kitty.sh --adopt` replaces that directory wholesale, and it
> is not tracked by git. Anything you change there is lost on the next sync.

Changes to the kitty side go in exactly one of two places:

| Kind of change | Where it goes |
|----------------|---------------|
| Modifying an existing upstream kitty file | a patch in `kitty-patches/` |
| Adding a brand-new file to the kitty tree | a file in `kitty-overlay/` |

`kitty-overlay/` mirrors the kitty tree layout (`kitty-overlay/kitty/moss_hook.c`
lands at `kitty/moss_hook.c`), and overlay files are copied *after* all patches
apply. Adding a new file there needs no patch — except that a new C source file
must also be registered with kitty's build, which is itself a patch to
`setup.py`.

### Editing an overlay file

Edit `kitty-overlay/<path>` directly, then re-apply so your build tree picks it
up:

```bash
cp kitty-overlay/kitty/moss_integration.py kitty/kitty/moss_integration.py
# or, from scratch — copy the pristine tree first, never patch it in place:
#   rm -rf kitty && cp -a kitty-pristine kitty && ./scripts/moss-apply-patches.sh kitty
cd kitty && make debug && ./test.py --module moss
```

### Editing a patched upstream file

1. Make the change in the working tree (`kitty/kitty/screen.c`).
2. Regenerate that patch against the pristine tree. The series uses plain
   unified diffs with `a/` and `b/` prefixes and **no** git headers, so:

   ```bash
   diff -u kitty-pristine/kitty/screen.c kitty/kitty/screen.c \
     | sed -e '1s|^--- .*|--- a/kitty/screen.c|' \
           -e '2s|^+++ .*|+++ b/kitty/screen.c|' \
     > kitty-patches/01-screen-c.patch
   ```

3. Prove the whole series still applies to a clean tree, with zero fuzz:

   ```bash
   rm -rf /tmp/kitty-verify && cp -a kitty-pristine /tmp/kitty-verify
   ./scripts/moss-apply-patches.sh /tmp/kitty-verify --check
   ```

   `--check` is a dry run: it reports `ok:` per patch and changes nothing. A
   `CONFLICT:` line names the offending patch file.

### Adding a new patch

Name it `NN-<file>.patch` following the existing numbering — the script applies
`kitty-patches/*.patch` in shell glob order, so the number *is* the ordering.
Keep each patch scoped to one upstream file; that is what makes rebasing onto a
new kitty release tractable.

### Keep the series small

The series is currently 10 patches, roughly 146 added lines. Every line is a line
someone has to rebase when kitty changes. If a change can live in
`kitty-overlay/` or in `engine/` instead of in a patch, put it there.

### Following a new kitty release

```bash
./scripts/moss-sync-kitty.sh --version 0.49.0 --check-only  # do the patches apply?
./scripts/moss-sync-kitty.sh --version 0.49.0               # fetch, patch, build, test
./scripts/moss-sync-kitty.sh --version 0.49.0 --adopt       # only after a green run
```

Everything before `--adopt` happens in `.sync-work/` and leaves the repository
untouched. When you bump the baseline, update in the same PR: the pinned version
and sha256 in `README.en.md`, `CONTRIBUTING.md` and
`.github/workflows/ci.yml`, and a `CHANGELOG.md` entry.

## 4. Coding standards

### Rust (`engine/`)

These are hard gates; CI fails on any of them:

```bash
cd engine
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- Zero clippy warnings. Not "mostly zero" — the flag is `-D warnings`.
- No new dependency without a reason in the PR description. The engine
  deliberately links no system libraries beyond libc in its `libmoss.so` form;
  a crate that drags in a C library must be behind the `cli` feature.
- Anything reachable from `engine/src/ffi/` runs **inside the kitty process on
  the render thread's call path**. It must not block, must not touch the tty,
  must not install signal handlers, must not call `exit()`, and must not panic
  across the FFI boundary. Errors return status codes; they do not unwind.
- Feature-gate CLI-only machinery behind `cli` so `--no-default-features` stays
  clean.

### kitty side (`kitty-patches/`, `kitty-overlay/`)

kitty's own contract, from its `local-agent.md`, applies verbatim:

- Build with `make debug` — not `go build`, not `./setup.py test`.
- `./test.py` must pass in full, and `./test.py --module moss` with it.
- **Any user-visible kitty change needs an entry in `docs/changelog.rst`.**
  Ours lives in `kitty-patches/10-changelog-rst.patch`; extend that patch rather
  than adding a new one.
- Match the surrounding style of each language; don't mix idioms across the
  C / Python / Go boundaries.
- Explicit error handling, no silent failures, no empty catch blocks.
- Every C hook must compile to a no-op when `MOSS_ENABLED` is not defined. The
  vanilla build is a supported configuration and a test lever, not an accident.
- Keep the Python side's injection path going through the `Screen`'s VT parser.
  Writing bytes to the tty directly bypasses kitty's own state tracking.

### Python (`scripts/`, `tests/`)

Standard library only. `scripts/moss-setup.py` and the e2e suites must run on a
stock `python3 >= 3.8` with nothing installed. User-facing strings in
`moss-setup.py` are bilingual (Chinese / English) — match that.

### Documentation

The Chinese docs (`README.md`, `部署与使用指南.md`, `项目概要.md`,
`文件清单.md`, `验收报告.md`, `specs/`) are canonical for the details;
`README.en.md` is the English entry point. If you change behaviour, update both
the Chinese doc that covers it and `README.en.md` if it is user-visible. Do not
let the two drift — three drift bugs have already been filed against this repo.

## 5. Running the tests

Run everything below before opening a PR that touches the corresponding area.

```bash
# 1. Engine unit tests + lints
cd engine
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test

# 2. kitty's own suite, and the Moss module inside it
cd ../kitty
./test.py
./test.py --module moss

# 3. End-to-end, from the repo root, against libmoss.so
cd ..
python3 tests/e2e_mock_llm.py               # context assembly, multi-turn, isolation, cancel
python3 tests/e2e_answer_leak.py            # answers must not leak into the next increment

# 4. End-to-end that require kitty's own interpreter
kitty/kitty/launcher/kitty +launch tests/e2e_run_command_embedded.py
kitty/kitty/launcher/kitty +runpy "exec(open('tests/e2e_kitty_capture.py').read())"
```

Notes:

- The e2e suites in (3) and (4) need `engine/target/release/libmoss.so` to exist;
  build it with `cargo build --release --no-default-features` first. Override the
  path with `MOSS_E2E_LIB=/path/to/libmoss.so` or `MOSS_ENGINE_LIB=...`.
- The two suites in (4) must run under kitty's Python because they exercise the
  real C hook (`e2e_kitty_capture.py`) and kitty's process-wide `SIGCHLD` reaping
  (`e2e_run_command_embedded.py`). Running them with the system `python3` does not
  test what they are for.
- Every suite sandboxes itself by pointing `MOSS_HOME` at a temp directory
  (honoured by the embedded FFI path, `engine/src/ffi/runtime.rs` — the standalone
  `moss` CLI always uses XDG paths) and
  talks only to a mock LLM on `127.0.0.1`. No API key, no network, no host state
  is touched. If you add a suite, keep that property — CI relies on it.
- A regression fix without a test that fails before it will not be merged.

## 6. Commits, sign-off, pull requests

### Commit messages

Follow the existing history: a short subject line stating the *effect*, a blank
line, then a body of bullet points explaining what changed and why, and a final
line recording the verification you ran.

```
修复验收审计确认的增量泄漏：捕获静音延伸到注入完成

审计以实验证实：busy 在 finish() 后立即清零，而 kitty 30ms 定时器仍在注入
回答字节……

引擎 510 全绿 + clippy 零告警；5+14+5+2 项 e2e 全过；kitty moss 模块全过
```

Chinese and English subjects are both fine — write in whichever language you
think in, and be specific. "fix bug" is not a commit message. Wrap the body at
72–80 columns. Reference issues as `#123`. Never mix an unrelated refactor into
a fix.

### Developer Certificate of Origin

All contributions are accepted under the
[Developer Certificate of Origin 1.1](https://developercertificate.org/). Sign
off every commit:

```bash
git commit -s          # appends: Signed-off-by: Your Name <you@example.com>
git rebase --signoff   # to add it to commits you already made
```

By signing off you certify that you wrote the contribution or otherwise have the
right to submit it under the project's licence, and that you are willing for it
to be distributed under that licence.

### Licensing of your contribution

This matters more than usual here, because the repository is dual-licensed by
path (see [NOTICE.md](./NOTICE.md)):

- Contributions to `engine/` are accepted under the **MIT** licence.
- Contributions to `kitty-patches/`, `kitty-overlay/`, `scripts/`, `packaging/`,
  `tests/`, `specs/` and the docs are accepted under **GPL-3.0-or-later**.
- Do not copy GPL-licensed code (including kitty's own) into `engine/`. That
  would force the engine to be relicensed and break the arrangement described in
  `NOTICE.md`.
- Do not copy code you do not have the right to submit, from anywhere, including
  from an LLM that reproduced it verbatim.

### Pull requests

Fill in [the PR template](./.github/PULL_REQUEST_TEMPLATE.md). Specifically:

- One logical change per PR.
- State which of the test commands in §5 you actually ran, and paste the
  relevant tail of the output. "Should be fine" is not a test result.
- If you touched `kitty-patches/`, include the output of
  `scripts/moss-apply-patches.sh <pristine-tree> --check`.
- If the change is user-visible, add a `CHANGELOG.md` entry under
  `## [Unreleased]`, and — for kitty-side changes — extend
  `kitty-patches/10-changelog-rst.patch`.
- Do not commit `kitty/`, `engine/target/`, `.sync-work/`, `kitty.prev/`,
  `*.orig`, `*.rej`, `__pycache__/` or anything from `~/.config/moss/`. In
  particular, **never commit a `.env` file or an API key** — `.gitignore` covers
  the obvious cases, but check `git diff --cached` before you push.

### Reporting security issues

Do not open a public issue. See [SECURITY.md](./SECURITY.md).
