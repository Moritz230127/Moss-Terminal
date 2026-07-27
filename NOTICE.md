# NOTICE — licensing of Moss Terminal

Moss Terminal is a *combined work*: it consists of a modified copy of
[kitty](https://github.com/kovidgoyal/kitty) (GPL-3.0-or-later) into which a
separately-developed Rust engine (MIT) is loaded as a dynamic library.

**The distributed combined work — anything that contains or produces a patched
kitty — is licensed under the GNU General Public License, version 3 or (at your
option) any later version.** The full text is in [`LICENSE`](./LICENSE).

This file states, per path, which licence applies and who holds copyright. It is
informative: where it and the licence texts disagree, the licence texts govern.

---

## 1. Per-path licensing

| Path | Licence | Copyright |
|------|---------|-----------|
| `engine/` (Rust source, `Cargo.toml`, `engine/LICENSE`) | MIT | © 2026 SHORiN-KiWATA |
| `engine/assets/o200k_base.tiktoken` + `engine/assets/tiktoken-rs.LICENSE` | MIT (third party) | the BPE rank table and licence notice bundled from `tiktoken-rs`; see that file for its own copyright line |
| `kitty-patches/*.patch` | GPL-3.0-or-later | Diffs against kitty; © 2016– Kovid Goyal for the surrounding kitty code, © 2026 Moss Terminal contributors for the added lines |
| `kitty-overlay/kitty/moss_hook.c`, `moss_hook.h` | GPL-3.0-or-later | © 2026 Moss Terminal contributors |
| `kitty-overlay/kitty/moss_integration.py` | GPL-3.0-or-later | © 2026 Moss Terminal contributors |
| `kitty-overlay/kitty_tests/moss.py` | GPL-3.0-or-later | © 2026 Moss Terminal contributors |
| `kitty/` (generated working tree; **not** part of the source distribution) | GPL-3.0-or-later | © 2016– Kovid Goyal and kitty contributors, plus the above |
| `scripts/`, `packaging/`, `tests/`, `specs/`, top-level `*.md` | GPL-3.0-or-later | © 2026 Moss Terminal contributors |
| Build outputs: patched `kitty` binary, `linux-package/` | GPL-3.0-or-later | as above |
| Build output: `libmoss.so`, `moss` CLI binary | MIT (built solely from `engine/`) | © 2026 SHORiN-KiWATA |

`LICENSE` is the GPL-3.0 text (byte-identical to `kitty/LICENSE`).
`LICENSE.MIT` is the engine's MIT text (byte-identical to `engine/LICENSE`,
with its original copyright line preserved).

---

## 2. Why the combined work is GPL-3.0

* Upstream kitty is GPL-3.0-or-later. `kitty-patches/` are derivative works of
  kitty's source files (`kitty/screen.c`, `kitty/vt-parser.c`, `setup.py`,
  `kitty/window.py`, `kitty/boss.py`, `kitty/child.py`,
  `shell-integration/*`, `docs/changelog.rst`) and therefore must be
  GPL-3.0-or-later.
* `kitty-overlay/` files are compiled into, and imported by, the kitty process.
  They are parts of the kitty program and are likewise GPL-3.0-or-later.
* Consequently any binary distribution of Moss Terminal, and any source
  distribution that includes the patch series, is GPL-3.0-or-later as a whole.

## 3. Why `engine/` can stay MIT

MIT is GPL-3.0-compatible: MIT-licensed code may be combined into a GPL-3.0
work, and the resulting combination is distributed under the GPL-3.0. The
reverse is not true, so `engine/` must not take in GPL-licensed code without
relicensing.

`engine/` is a standalone Rust crate with no kitty code in it and no build-time
dependency on kitty. Its public surface towards kitty is a C ABI
(`moss_*` functions in `engine/src/ffi/`). It builds and runs on its own as the
`moss` CLI. Distributed on its own — the crate, `libmoss.so`, or the `moss`
binary — it is MIT-licensed and carries no GPL obligations.

The GPL-3.0 obligations attach when the engine is shipped *together with* the
patched kitty, i.e. to the Moss Terminal release artefacts as a set.

**Note on `dlopen`:** kitty loads `libmoss.so` at runtime via `dlopen` from
`kitty-overlay/kitty/moss_hook.c`. The FSF's position is that runtime dynamic
loading into one address space still forms a single combined work. This project
does not rely on any contrary reading: it distributes the combination under the
GPL-3.0, which is permitted precisely because the engine's own licence (MIT) is
GPL-compatible.

## 4. Upstream credits

* **kitty** — © 2016– Kovid Goyal `<kovid at kovidgoyal.net>` and contributors.
  <https://github.com/kovidgoyal/kitty>. GPL-3.0. Moss Terminal is an
  unofficial derivative and is neither endorsed by nor affiliated with the kitty
  project. Do not report Moss Terminal bugs to kitty's issue tracker.
  kitty bundles further third-party components under their own licences; see
  the upstream tree (e.g. its `glfw/`, `3rdparty/` and `LICENSE` files) after
  you materialise it with `scripts/moss-apply-patches.sh`.
* **Moss Engine upstream** — the code in `engine/` originates from the **Miyu**
  project by SHORiN-KiWATA (<https://github.com/SHORiN-KiWATA/Moss>), MIT
  licensed; the crate version `0.2.1` in `engine/Cargo.toml` is inherited from
  it. Moss Terminal's changes to it are contributed back under the same MIT
  licence.
* **DeepSeek-Reasonix** — the layered context-cache strategy in the engine was
  informed by ideas from the Reasonix desktop client. No Reasonix code is
  included in this repository.
* **Rust crates** — the engine's dependencies (see `engine/Cargo.toml` and
  `engine/Cargo.lock`) are third-party works under their own licences,
  predominantly MIT/Apache-2.0. `rusqlite` is built with the `bundled` feature
  and therefore compiles SQLite, which is in the public domain.

## 5. Obligations when you redistribute

* Ship `LICENSE` (GPL-3.0), `LICENSE.MIT` and this `NOTICE.md`.
* Provide the complete corresponding source for the patched kitty. Shipping
  `kitty-patches/` + `kitty-overlay/` + the pinned upstream tarball identified
  in `README.en.md` satisfies this, because
  `scripts/moss-apply-patches.sh` reconstructs the tree exactly.
* Keep the engine's MIT copyright notice with any copy of `engine/`,
  `libmoss.so` or the `moss` binary.
* Do not remove kitty's own copyright headers from patched files.

## 6. SPDX summary

```
SPDX-License-Identifier: GPL-3.0-or-later        # the project as distributed
SPDX-License-Identifier: MIT                     # engine/ taken alone
```
