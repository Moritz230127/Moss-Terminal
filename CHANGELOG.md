# Changelog

All notable changes to Moss Terminal are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Versioning note: `0.1.0` is the version of **Moss Terminal** (the combined
product, tagged `v0.1.0`). It is distinct from the version of the Rust crate in
`engine/Cargo.toml` (`0.2.1`), which is inherited from the engine's upstream
project and is not changed by Moss Terminal releases.

[Unreleased]: https://github.com/Moritz230127/Moss-Terminal/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Moritz230127/Moss-Terminal/releases/tag/v0.1.0

## [Unreleased]

Nothing yet.

## [0.1.0] — 2026-07-26

First public release. Built against **kitty 0.48.1**
(sha256 `aadb428e20ad678c0a7969c0a80c46f391b49addeb7fda57b06a14a4d102fb1d`).

Because this is the initial release, *Changed* and *Fixed* below describe work
done during pre-release development rather than differences from an earlier
published version. Entries reference the commit that introduced them.

### Added

- **In-process FFI architecture** — the Moss engine ships as a `cdylib`
  (`libmoss.so`) that the kitty process `dlopen`s. No daemon, no socket, no IPC.
  13 exported `moss_*` C-ABI symbols; one isolated session per kitty window.
  (`1fa094e`, `75a10a0`)
- **`》` trigger** — typing `》question` at a shell prompt sends
  `OSC 7717;ask;<base64>` to the engine and streams the answer back into the
  window. Shell integration for zsh (`accept-line` widget), fish (`\r` binding)
  and bash (`command_not_found_handle`), all gated on `MOSS_TERMINAL=1`.
  (`1fa094e`)
- **Terminal-context capture in the C core** — `screen_linefeed`, OSC 133 prompt
  marking and OSC 7 feed captured lines to the engine. Main screen only (never
  the alternate screen), visible text only, prompt kind preserved. (`1fa094e`)
- **Patch-series repository layout** — the fork is `kitty-patches/` (10 patches,
  ~146 lines) + `kitty-overlay/` (4 new files:
  `kitty/moss_hook.c`, `kitty/moss_hook.h`, `kitty/moss_integration.py`,
  `kitty_tests/moss.py`), applied by `scripts/moss-apply-patches.sh`. The kitty
  working tree is fully regenerable. (`1fa094e`)
- **`scripts/moss-sync-kitty.sh`** — detect / fetch / patch / build / test / adopt
  pipeline for following upstream kitty releases. Everything before `--adopt`
  happens out-of-tree in `.sync-work/`. (`1fa094e`)
- **`scripts/moss-setup.py`** — seven-stage interactive installer: dependency
  check, dual engine build, kitty `linux-package` build, install to a prefix,
  provider configuration, desktop entry, summary. Supports `--dry-run`,
  `--prefix`, `--yes`. (`1fa094e`, step-5 confirmation added in `1db9353`)
- **`MOSS_ENABLED=0` build mode** — compiles every C hook to a no-op, producing
  plain unmodified kitty from the same tree. (`1fa094e`)
- **Cache-shape tracking** (`engine/src/agent/cache_shape.rs`) — hashes the
  system-prompt and tool-definition prefixes, classifies what changed and can
  report it inline via `display.show_cache_diagnostics`. (`e2f3382`)
- **Four-level context-overflow ladder** — `soft` 0.5 / `snip` 0.6 / `compact`
  0.8 / `force` 0.9, configurable, with pathological configurations
  auto-corrected. (`e2f3382`)
- **Render-layer tool-result pruning** — UTF-8-safe head/tail excerpting that
  keeps the most recent 1–2 turns verbatim and never rewrites the database, so
  pruning and append-only storage both hold. (`e2f3382`)
- **Archive retrieval over evicted turns** — FTS5 trigram external-content table
  with table-level sync triggers, self-healing backfill for legacy databases,
  and full candidate filtering in SQL, which makes Chinese keywords searchable.
  (`e2f3382`)
- **Four bundled default personas**, idempotently seeded into `personas/`, with
  a two-directory search path. (`1db9353`)
- **Inline privacy notice** on the first answer served by the built-in zero-key
  public provider, stating that terminal context is sent to `opencode.ai`.
  (`1db9353`)
- **Explicit loss markers** when an unconsumed line is evicted, instead of
  dropping it silently. (`1db9353`)
- **Test suites** — `tests/e2e_mock_llm.py` (context assembly, multi-turn
  increments, per-window isolation, busy/cancel, window-close cleanup),
  `tests/e2e_kitty_capture.py` (capture through the real kitty C hook: eza icons,
  CJK, OSC 133, trigger-line de-duplication), `tests/e2e_run_command_embedded.py`
  (ECHILD regression), `tests/e2e_answer_leak.py` (injection-loop timing
  regression), plus kitty's own `./test.py --module moss`.
  (`1fa094e`, `6ae0558`, `75a10a0`)

### Changed

- **Default answer format is now plain text.** The terminal renders inline with
  no Markdown parser, so fences, headings and bold markers used to show up
  literally. The system prompt now forbids Markdown syntax by default (commands
  are indented four spaces with a `$` prefix); Markdown is produced only on
  explicit request. (`c631af7`)
- **Baseline moved from kitty 0.48.0 to 0.48.1**, with the whole patch series
  regenerated so every patch applies with zero fuzz. (`7bd2645`, `9a9b573`)
- **Content downgrader now emits incrementally** instead of buffering whole
  lines; only genuinely ambiguous prefixes (```` ``` ````, `#`, table rows) are
  held back. (`6ae0558`)
- **`compact` now triggers at 0.8** of the context window instead of 0.9.
  (`e2f3382`)
- **Prompt-fingerprint changes back up the conversation database** to
  `conversation.pre-*.db` instead of wiping the tables outright. (`2d5db8c`)
- **Sync-script version detection** now parses `kitty/kitty/constants.py` with
  Python rather than shell text-munging, and an already-patched tree is marked
  with `.moss-patched` so re-runs are idempotent. (`7bd2645`)
- **`ProviderConfig.display_name`** accepts `name` as a serde alias, so the
  configuration examples in `specs/需求规格.md` load as written. (`1db9353`)

### Fixed

- **AI answers leaked into the next turn's increment.** `busy` was cleared as
  soon as the turn future finished, while kitty's 30 ms timer was still injecting
  answer bytes; the injection re-entered through `moss_on_line` and polluted the
  next question's terminal context. Capture is now muted from `ask()` onwards
  and un-muted by the injector through the new `moss_set_capture` FFI call,
  after the final byte reaches the screen and before the sentinel — with the
  same release on the cancel and error paths. Regression test:
  `tests/e2e_answer_leak.py`. (`75a10a0`)
- **`models_cache` was never loaded on the embedded path**, so `context_window`
  could not be resolved and the entire trim/compact stack silently did nothing.
  The cache is now loaded at FFI init and refreshed in the background, and models
  whose window cannot be resolved are pinned to 65536 with a warning logged.
  (`2d5db8c`)
- **Provider context-overflow errors are now classified**
  (`llm::is_context_overflow_error`) and the FFI session compacts and retries
  once, instead of surfacing a raw API error when the token estimate is wrong.
  (`e2f3382`)
- **`run_command` reported a false failure under an embedded host.** kitty reaps
  its children process-wide, so the engine's own child could be collected before
  tokio's `wait()` ran, returning `ECHILD` — surfaced as `✗ run_command` and
  making the model retry the same command. Regression test:
  `tests/e2e_run_command_embedded.py`. (`6ae0558`)
- **fish leaked its `read` prompt** (`read>`) ahead of the streamed output; the
  binding now uses `read -s -P ''`. (`2d5db8c`)
- **Markdown hard-downgrade safety net** — the engine's render layer now strips
  fences, headings, bold and inline code incrementally rather than trusting the
  model to obey a plain-text instruction. Safe across chunk boundaries; fence
  bodies are preserved and indented. Markdown pipe tables were wired into the
  same pipeline (separator row removed, cells joined with two spaces).
  (`2d5db8c`, `6ae0558`)
- **Reasoning blocks are now collapsed** to a one-line summary with elapsed time.
  The engine wraps them in APC markers; the kitty side verifies the line count
  with `wcswidth` *and* the cursor position before scrolling back to erase, and
  leaves the block untouched if either check fails — it never erases the wrong
  rows. (`2d5db8c`)
- **Automatic eviction with memory disabled** no longer deletes turns that have
  no archive; reasoning is folded into the archive on eviction. (`e2f3382`)
- **Recursion defect** between `chat_messages` and `effective_context_tokens`.
  (`e2f3382`)
- **`.gitignore` completeness** and three documentation drifts (FFI symbol count,
  test inventory, fish timeout). (`7bd2645`, `1db9353`)

### Known limitations

- Under **bash**, `》` is intercepted by `command_not_found_handle`, i.e. after
  the shell has parsed the line. Quoting, globs and whitespace are mangled by
  word splitting, and a compound line such as `》q; cmd` has its tail executed by
  bash. zsh and fish intercept before execution. See
  [SECURITY.md](./SECURITY.md).
- Shells other than zsh, fish and bash have no `》` trigger at all (line capture
  is shell-independent, but triggering is not).
- fish has no `read` timeout, so a hung request must be cancelled with `Ctrl+C`;
  zsh and bash fall back to a 300 s watchdog.
- Acceptance was recorded as *conditional* — several success metrics (3 s
  first token, three-window GUI isolation, CPU/memory, the zsh path) require
  manual measurement outside the sandbox. See `验收报告.md`.
