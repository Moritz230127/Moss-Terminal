<!--
Thanks for contributing. Please read CONTRIBUTING.md first — especially the
patch-series workflow. The single most common mistake is editing kitty/
directly: that directory is generated, gitignored, and replaced wholesale by
scripts/moss-sync-kitty.sh --adopt. Your change must live in kitty-patches/
or kitty-overlay/ to survive.
-->

## What this changes

<!-- One paragraph. What is different after this PR, and why. -->

Fixes #

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] kitty patch series change (`kitty-patches/` or `kitty-overlay/`)
- [ ] Build / packaging / CI
- [ ] Documentation only
- [ ] Refactor with no behaviour change

## Which layers does it touch?

- [ ] Layer 3 — Rust engine (`engine/`)
- [ ] Layer 2 — kitty Python integration (`kitty-overlay/kitty/moss_integration.py`)
- [ ] Layer 1 — kitty C core (`kitty-patches/`, `kitty-overlay/kitty/moss_hook.c`)
- [ ] Shell integration (the `》` trigger)
- [ ] Scripts / installer / packaging
- [ ] Docs

## Tests I actually ran

Tick only what you ran, and paste the relevant tail of the output below.
"Should be fine" is not a test result.

- [ ] `cd engine && cargo fmt --all -- --check`
- [ ] `cd engine && cargo clippy --all-targets -- -D warnings` (zero warnings)
- [ ] `cd engine && cargo test`
- [ ] `cd kitty && ./test.py --module moss`
- [ ] `cd kitty && ./test.py` (full kitty suite)
- [ ] `python3 tests/e2e_mock_llm.py`
- [ ] `python3 tests/e2e_answer_leak.py`
- [ ] `kitty/kitty/launcher/kitty +launch tests/e2e_run_command_embedded.py`
- [ ] `kitty/kitty/launcher/kitty +runpy "exec(open('tests/e2e_kitty_capture.py').read())"`
- [ ] Manually, in a real Moss Terminal window (say which shell)

<details>
<summary>Test output</summary>

```
paste here
```

</details>

## If you touched the patch series

- [ ] I did **not** hand-edit `kitty/`; the change is in `kitty-patches/` and/or `kitty-overlay/`.
- [ ] The series still applies with zero fuzz to a pristine tree — output pasted below:

<details>
<summary><code>scripts/moss-apply-patches.sh &lt;pristine-tree&gt; --check</code></summary>

```
paste here
```

</details>

- [ ] User-visible kitty behaviour changed → I extended `kitty-patches/10-changelog-rst.patch`
      (kitty's `docs/changelog.rst` contract), rather than adding a new patch.
- [ ] The patch series is still as small as it can be: nothing here could have lived
      in `kitty-overlay/` or `engine/` instead.

## If you touched the engine

- [ ] Nothing new is reachable from `engine/src/ffi/` that blocks, touches the tty,
      installs a signal handler, calls `exit()`, or can panic across the FFI boundary.
- [ ] Any CLI-only machinery is behind the `cli` feature, so
      `cargo build --release --no-default-features` stays clean.
- [ ] New dependency? Justified below, and it does not add a system library to
      `libmoss.so`.

## Checklist

- [ ] Every commit is signed off (`git commit -s`) — see the DCO section in CONTRIBUTING.md.
- [ ] A regression fix comes with a test that fails without the fix.
- [ ] User-visible change → entry added to `CHANGELOG.md` under `## [Unreleased]`.
- [ ] Docs updated: `README.en.md` and the relevant Chinese doc, kept consistent.
- [ ] No `.env`, API key, personal path, or captured terminal content in the diff
      (`git diff --cached` before pushing).
- [ ] Nothing generated is committed: `kitty/`, `engine/target/`, `.sync-work/`,
      `kitty.prev/`, `__pycache__/`, `*.orig`, `*.rej`.
- [ ] Licensing understood: changes under `engine/` are contributed as MIT,
      everything else as GPL-3.0-or-later (see NOTICE.md). No GPL code was copied
      into `engine/`.

## Anything reviewers should look at closely

<!-- Risky bits, things you were unsure about, alternatives you rejected. -->
