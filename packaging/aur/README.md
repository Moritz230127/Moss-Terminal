# packaging/aur — Arch Linux / AUR packaging for Moss Terminal

Everything needed to build `moss-terminal` as a pacman package and to publish
it on the AUR.

| File | What it is |
|---|---|
| `PKGBUILD` | the release package (`moss-terminal`), builds from the project's published release tarball |
| `PKGBUILD-git` | the VCS variant (`moss-terminal-git`), builds from the tracked branch |
| `moss-terminal.install` | pacman lifecycle messages (post_install / post_upgrade / post_remove) |
| `.SRCINFO` | machine-readable metadata for the AUR; **generated**, never hand-edited |

Nothing here is wired into `scripts/moss-setup.py` or any other part of the
repo. This directory is only consumed by `makepkg`.

---

## Before anything else: four placeholders

The project has no git remote and no tags yet, so four values in `PKGBUILD` are
placeholders that **must** be substituted before publishing.

1. **`OWNER`** — the GitHub account or org that will host the repo — and
   **`you@example.com`** on the `Maintainer:`/`Contributor:` lines. Both in one
   go (`moss-terminal.install` contains neither, so it is not in the list):

   ```bash
   sed -i -e 's|OWNER|your-github-user|g' \
          -e 's|you@example\.com|you@your.domain|g' \
          PKGBUILD PKGBUILD-git README.md .SRCINFO
   grep -rn 'OWNER\|you@example\.com' .    # must return nothing
   ```

2. **`pkgver`** — currently `0.1.0`. It must equal the git tag the source URL
   points at (`v$pkgver`). Note that the Rust crate version in
   `engine/Cargo.toml` is `0.2.1`, inherited from that crate's own upstream —
   it is intentionally *not* the package version, so do not "sync" them.

3. **`sha256sums[0]`** — currently `SKIP`, because the release asset it names
   does not exist yet and therefore cannot be hashed. **Do not submit to the
   AUR with `SKIP` on a non-VCS source.** Tag and push the release first (which
   runs `.github/workflows/release.yml`), then take the digest from the
   `SHA256SUMS.txt` published alongside the tarball:

   ```bash
   curl -sL https://github.com/Moritz230127/Moss-Terminal/releases/download/v0.1.0/SHA256SUMS.txt \
     | awk -v f='./moss-terminal-0.1.0.tar.gz' '$2==f{print $1}'
   ```

`sha256sums[1]` (the pinned kitty tarball) is already a real digest and must
stay one.

### Why `source[0]` is the release asset, not the GitHub tag archive

`source[0]` points at
`${url}/releases/download/v${pkgver}/moss-terminal-${pkgver}.tar.gz`, the
artefact this project's own release pipeline publishes, **not** at
`${url}/archive/refs/tags/v${pkgver}.tar.gz`.

`.github/workflows/release.yml` builds that asset with
`scripts/make-source-tarball.sh` under an explicit `SOURCE_DATE_EPOCH`: sorted
file list under `LC_ALL=C`, ownership forced to `0:0`, GNU tar format, `gzip
-n`. It is documented byte-reproducible, so the recorded digest can be
re-derived from the tag alone. GitHub's auto-generated tag archives carry no
such guarantee — their gzip layer has changed before and invalidated AUR
checksums en masse. The release asset is also smaller: `make-source-tarball.sh`
deliberately omits `engine/pics/` (13 MB of unreferenced screenshots) and the
220 MB regenerable `kitty/` tree.

Both unpack to the same top-level directory (`moss-terminal-$pkgver`), so
`_projsrc` is identical either way.

---

## What the package actually does

`kitty/` in the working tree is a *regenerable* artefact, so the package does
not ship it. Instead it downloads the pinned pristine kitty release and
reconstructs the Moss Terminal source at build time:

```
pristine kitty 0.48.1  +  kitty-patches/*.patch (10)  +  kitty-overlay/ (4 files)
        │                        via scripts/moss-apply-patches.sh
        ▼
  patched kitty  ──►  python setup.py linux-package  ──►  /usr/lib/moss-terminal/
engine/ (Rust)   ──►  cargo build ×2                 ──►  libmoss.so + moss CLI
```

Two things in this pipeline are easy to get wrong and are commented at length
in the `PKGBUILD` itself:

- **The engine is built twice and the order is load-bearing.** `cargo build
  --release` first (produces the `moss` CLI plus an ALSA/TTY-linked
  `libmoss.so`), then `cargo build --release --no-default-features` (skips the
  CLI because its `[[bin]]` has `required-features = ["cli"]`, and *overwrites*
  `libmoss.so` with the embed-safe variant that gets `dlopen()`ed into the
  kitty render process). Reversing them silently ships an audio stack inside
  your terminal emulator.

- **`check()` would clobber `libmoss.so`.** `cargo test --release` builds with
  default features and runs *after* `build()`. So `build()` snapshots both
  artefacts into `$srcdir/moss-staging/` the moment they are produced, and
  `package()` installs from there.

`_kittyver` and `sha256sums[1]` are a matched pair. The patch series applies
with zero fuzz to that exact kitty version and no other; if you bump one, bump
the other and regenerate the series with `scripts/moss-sync-kitty.sh`. A
mismatch fails loudly in `prepare()` (the patch script exits 2 on conflict)
rather than producing a kitty without the AI hook.

---

## Install layout, and why it cannot collide with `kitty`

Moss Terminal is a kitty fork, so the number one packaging risk is stomping on
the stock `kitty` package. It does not:

| We install | Stock `kitty` owns |
|---|---|
| `/usr/bin/moss-terminal` (wrapper), `/usr/bin/moss` | `/usr/bin/kitty`, `/usr/bin/kitten` |
| `/usr/lib/moss-terminal/**` | `/usr/lib/kitty/**` |
| `/usr/share/applications/moss-terminal.desktop` | `.../kitty.desktop`, `.../kitty-open.desktop` |
| `.../icons/hicolor/*/apps/moss-terminal.{png,svg}` | `.../apps/kitty.{png,svg}` |
| `/usr/share/{doc,licenses}/moss-terminal/**` | `/usr/share/doc/kitty/**` |
| `/usr/share/moss-terminal/packaging/**` | — |

Deliberate non-installs:

- **`kitten` is not exposed in `/usr/bin`.** It stays at
  `/usr/lib/moss-terminal/bin/kitten`; `/usr/bin/kitten` belongs to `kitty`.
- **No `/usr/share/terminfo` entry.** We depend on `kitty-terminfo` instead of
  shipping a byte-identical duplicate.
- **kitty's man pages stay inside the payload**, at
  `/usr/lib/moss-terminal/share/man`, so `man kitty` keeps meaning the system
  kitty. Read ours with
  `MANPATH=/usr/lib/moss-terminal/share/man man kitty`.

### Where `libmoss.so` goes, and why

It is installed **inside the kitty payload**, at
`/usr/lib/moss-terminal/lib/kitty/libmoss.so` — matching the reference
source-tree install (`~/.local/share/moss-terminal/lib/kitty/libmoss.so`), not
at `/usr/lib/moss-terminal/libmoss.so`, which *no* loader finds on its own.

There are two independent loaders in one process, and they resolve differently:

- `kitty-overlay/kitty/moss_integration.py:_candidate_library_paths()` tries
  `$MOSS_ENGINE_LIB`, then `dirname(dirname(__file__)) + "/libmoss.so"`. The
  file sits at `<prefix>/lib/kitty/kitty/moss_integration.py`, so that fallback
  is exactly `<prefix>/lib/kitty/libmoss.so`. **This one now works unaided.**
- `kitty-overlay/kitty/moss_hook.c` tries `$MOSS_ENGINE_LIB`, then the bare
  soname `"libmoss.so"`. The library has **no `SONAME`** (`readelf -d` shows no
  such entry) and is deliberately in no `ld.so` search path, and the Python side
  does not re-export the variable — so **the C hook still requires
  `$MOSS_ENGINE_LIB`.**

Consequence: `/usr/bin/moss-terminal` is the supported entry point. Launching
`/usr/lib/moss-terminal/bin/kitty` directly — a hand-written `.desktop`, a
compositor `exec` line, a systemd unit, an env-sanitising launcher — yields a
degraded terminal. Installing the library at the payload-relative path does not
fully rescue that case, but it does mean the Python half degrades gracefully
rather than the engine being absent entirely, and it removes any chance of the
two loaders finding *different* files.

The wrapper therefore exports the same path, and the two must never disagree —
if they name different files, the C hook and the Python side `dlopen` two
separate objects, i.e. two independent engine instances in one process:

```sh
export MOSS_ENGINE_LIB="${MOSS_ENGINE_LIB:-/usr/lib/moss-terminal/lib/kitty/libmoss.so}"
exec /usr/lib/moss-terminal/bin/kitty "$@"
```

The payload is relocatable: for `linux-*` bundle types kitty's `setup.py`
compiles the launcher with a *relative* `-DKITTY_LIB_PATH="../lib/kitty"`,
which `kitty/launcher/main.c` resolves against
`dirname(realpath("/proc/self/exe"))`. That is why `--prefix` can point at a
staging directory during `build()` and the tree can simply be copied into
`/usr/lib/moss-terminal/` in `package()` with nothing patched.

---

## Dependency notes

Baseline is the Arch `kitty` package's `Depends On`, with two deliberate
deltas:

- **Removed `kitty-shell-integration`.** Arch builds kitty with
  `--shell-integration='enabled no-rc'` and ships those scripts separately.
  We must not use them: patches 08/09 modify kitty's *bundled* shell
  integration (the fish `\r` binding and the bash `command_not_found_handle`
  that intercept `》`), and we build with the upstream default
  `shell_integration=enabled` so kitty injects our patched copy from
  `/usr/lib/moss-terminal/lib/kitty/shell-integration/`. Arch's unpatched
  copy cannot trigger `》` at all.
- **Added `alsa-lib`.** The `moss` CLI binary is built with default features
  and links `libasound` via `rodio` for the alarm tool (`ldd /usr/bin/moss` →
  `libasound.so.2`). The embedded `libmoss.so` needs only libc/libgcc/libm,
  but the CLI ships in the same package, so the dependency is real.

No sphinx in `makedepends`: kitty's *release tarball* ships prebuilt docs
(`docs/_build/man`, `docs/_build/html`), so `setup.py` never shells out to
`make docs`. This is only true of the release tarball — a git checkout of
kitty would need sphinx.

### Build-time network access

`prepare()` runs `cargo fetch --locked` and `go mod download`. Both are
network access outside `source=()`, which is normally a supply-chain smell —
here it is bounded and checksummed:

- `cargo fetch --locked` refuses to resolve anything `engine/Cargo.lock` does
  not already pin, and the lockfile carries a checksum per crate. `build()`
  and `check()` then run with `--frozen` (= `--locked --offline`).
- `go mod download` is pinned and checksummed by kitty's own `go.sum`. The
  kitty release tarball does not vendor its Go modules (verified: no
  `vendor/`), so there is no offline alternative. `build()` then runs with
  `-mod=readonly`.

Both are stated explicitly here because a reviewer will and should ask.

---

## Building and testing locally

```bash
cd packaging/aur
makepkg -s            # build, resolving makedepends
makepkg -si           # build and install
namcap moss-terminal-*.pkg.tar.zst   # lint the built package
```

Useful during iteration:

```bash
makepkg --printsrcinfo        # metadata only, no build, no network
makepkg -o                    # stop after prepare() — checks the patch series applies
makepkg -e                    # rebuild without re-extracting
makepkg --nocheck             # skip cargo test (the slow part)
namcap PKGBUILD               # lint the recipe itself
```

Inspect the staged tree before it is compressed:

```bash
find pkg/moss-terminal -type f | sort | head -50
```

Re-verify that nothing collides with the installed `kitty`:

```bash
comm -12 \
  <(pacman -Qlq moss-terminal 2>/dev/null | sort -u) \
  <(pacman -Ql kitty | awk '{print $2}' | sort -u)
# any output at all is a bug
```

The `-git` variant lives in its own directory (AUR requires one package base
per repo):

```bash
mkdir -p ../aur-git && cp PKGBUILD-git ../aur-git/PKGBUILD \
  && cp moss-terminal.install ../aur-git/
cd ../aur-git
makepkg --printsrcinfo > .SRCINFO   # its OWN .SRCINFO — not the release one
makepkg -si
```

The `-git` package base needs its own `.SRCINFO` committed alongside its
`PKGBUILD`; the release package's file is not interchangeable (different
`pkgbase`, `provides`, `conflicts` and `source`).

### Testing beyond the package build

`check()` runs the engine's `cargo test --release` only. kitty's own
`./test.py` and `./test.py --module moss` are **not** run: they need a real
terminal, and much of the suite needs a display, a GPU context and a working
font stack, none of which exist inside makepkg's headless non-interactive
environment. The moss integration tests additionally have to be started via
`kitty +launch`, i.e. they need the terminal to actually open a window. Run
them by hand from a source checkout after installing.

---

## Updating to a new release

```bash
# 0. tag and push, so the release workflow publishes
#    moss-terminal-X.Y.Z.tar.gz + SHA256SUMS.txt

# 1. bump the version
sed -i 's/^pkgver=.*/pkgver=X.Y.Z/; s/^pkgrel=.*/pkgrel=1/' PKGBUILD

# 2. take sha256sums[0] from the release's own SHA256SUMS.txt (authoritative —
#    it also lets you verify the download independently of what you fetched)
curl -sL https://github.com/Moritz230127/Moss-Terminal/releases/download/vX.Y.Z/SHA256SUMS.txt \
  | awk -v f='./moss-terminal-X.Y.Z.tar.gz' '$2==f{print $1}'
#    then edit sha256sums[0] in PKGBUILD.
#    `updpkgsums` (pacman-contrib) also works, but it only re-hashes whatever
#    it downloads — it cannot cross-check against what upstream published.
#    Never let it rewrite sha256sums[1] to something other than the pinned
#    kitty digest.

# 3. rebuild and test
makepkg -f

# 4. regenerate metadata — ALWAYS after touching PKGBUILD
makepkg --printsrcinfo > .SRCINFO
```

Reset `pkgrel=1` on any `pkgver` change; bump `pkgrel` (leaving `pkgver`
alone) when only the recipe changed.

### Rebuild after a python minor upgrade

`depends=('python3')` is unversioned, but the payload is **not** version-
independent: kitty links the libpython of the machine that built it (verified:
`ldd /usr/lib/kitty/kitty/fast_data_types.so` → `libpython3.14.so.1.0`). Arch
rebuilds its own `kitty` on every python minor bump; an AUR package gets no such
rebuild. So when `python` goes 3.14 → 3.15, Moss Terminal will fail to start
with

```
libpython3.X.so.1.0: cannot open shared object file: No such file or directory
```

The fix is simply to rebuild the package (`makepkg -f`, or reinstall via your
AUR helper). Bump `pkgrel` and push a rebuild if you maintain the AUR entry.

If the new release also moves to a newer kitty, bump `_kittyver` **and**
`sha256sums[1]` together, and confirm the patch series still applies:

```bash
makepkg -o            # runs prepare() only; the patch script exits 2 on conflict
```

The `-git` package never needs a manual `pkgver` bump — `pkgver()` derives it
from `git describe`, falling back to `0.1.0.r<commits>.g<sha>` while the repo
is untagged. Its `.SRCINFO` still has to be regenerated whenever the recipe
changes.

---

## Publishing to the AUR

One git repository per package base, hosted on `aur.archlinux.org`, containing
**only** the packaging files — never `src/`, `pkg/`, or built packages.

```bash
# one-time: upload an SSH public key at https://aur.archlinux.org/account/
# and add to ~/.ssh/config:
#   Host aur.archlinux.org
#     User aur
#     IdentityFile ~/.ssh/aur
#     HostName aur.archlinux.org

git clone ssh://aur@aur.archlinux.org/moss-terminal.git aur-moss-terminal
cd aur-moss-terminal

cp ../PKGBUILD ../moss-terminal.install ../.SRCINFO .
printf '*\n!PKGBUILD\n!.SRCINFO\n!*.install\n!.gitignore\n' > .gitignore

git add -A
git commit -m 'Initial import: moss-terminal X.Y.Z'
git push origin master        # AUR still uses 'master'
```

Cloning a not-yet-existing package name gives you an empty repo — that is the
normal way to create a new package base. Repeat with
`ssh://aur@aur.archlinux.org/moss-terminal-git.git` for the VCS variant.

Subsequent updates are just: edit, `updpkgsums`, `makepkg --printsrcinfo >
.SRCINFO`, commit, push.

---

## Pre-push checklist

- [ ] `OWNER` **and** `you@example.com` substituted everywhere
      (`grep -rn 'OWNER\|you@example\.com' .` returns nothing)
- [ ] `pkgver` equals the git tag the `source=` URL points at, that tag is
      pushed, and the release workflow has finished publishing
      `moss-terminal-$pkgver.tar.gz` + `SHA256SUMS.txt`
- [ ] `sha256sums[0]` is a real digest, **not** `SKIP`, and it matches the entry
      in the release's `SHA256SUMS.txt`
- [ ] `sha256sums[1]` still matches the `_kittyver` you actually build against
- [ ] `pkgrel` reset to 1 on a version bump, incremented on a recipe-only change
- [ ] `bash -n PKGBUILD && bash -n PKGBUILD-git`
- [ ] `namcap PKGBUILD` — clean, or every warning understood
- [ ] `makepkg -f` succeeds from a clean `src/`, ideally in a clean chroot
      (`extra-x86_64-build` from devtools) so undeclared makedepends surface
- [ ] `namcap moss-terminal-*.pkg.tar.zst` — clean, or every warning understood
- [ ] no file collisions with `kitty` (the `comm -12` command above returns
      nothing)
- [ ] `moss-terminal` launches and a `》` question round-trips after
      `moss config`
- [ ] `.SRCINFO` regenerated **last**, after the final `PKGBUILD` edit, and
      committed in the same commit
- [ ] repo contains only `PKGBUILD`, `.SRCINFO`, `*.install`, `.gitignore` —
      no `src/`, no `pkg/`, no tarballs
