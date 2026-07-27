# Security Policy

## Supported versions

| Version | Supported |
|---------|-----------|
| `0.1.x` (latest release) | ✅ Security fixes |
| Anything older / `main` before `v0.1.0` | ❌ |

Moss Terminal is pre-1.0. Only the most recent release line receives fixes;
there are no backports. Because the project vendors nothing but re-applies a
patch series onto a pinned upstream kitty (currently **0.48.1**), a kitty
security release is followed by re-running `scripts/moss-sync-kitty.sh` and
cutting a new Moss Terminal release — kitty's own advisories apply to us
unchanged.

## Reporting a vulnerability

**Do not open a public GitHub issue for a security problem.**

- Preferred: GitHub's private vulnerability reporting —
  <https://github.com/Moritz230127/Moss-Terminal/security/advisories/new>
- Email: `moritz001@163.com`
  *(placeholder — the maintainer must replace this with a real address before
  publishing the repository)*

Please include: the version or commit, your OS and shell, what an attacker
gains, and the smallest reproduction you can manage. If you have a patch, say
so; do not attach exploit code to a public thread.

What to expect: acknowledgement within 7 days, an assessment within 30 days, and
credit in the release notes unless you ask otherwise. This is a small volunteer
project — there is no bug bounty and no paid support.

Vulnerabilities in **kitty itself** belong to
<https://github.com/kovidgoyal/kitty>, not here. Report them upstream; tell us
afterwards so we can rebase and re-release.

---

## Security model

Read this section before deciding whether Moss Terminal belongs on a machine
that handles secrets. Several of the properties below are inherent to what the
product *is*, not bugs to be fixed.

### 1. Your terminal content is sent to an LLM provider

This is the feature. Every `》` question ships the terminal text produced in that
window since the last answer, plus the conversation history the engine keeps for
that window, to the configured provider over HTTPS.

Terminal output routinely contains secrets: `env` dumps, `kubectl` output, tokens
echoed by a failing curl, paths that identify you, source code, `git remote`
URLs with credentials in them. **All of it is eligible to be transmitted.** The
engine does not scan for or redact secrets.

Mitigations available to you:

- Do not ask questions in a window where a secret was recently displayed. The
  increment covers everything since the last answer, not just the last line.
- Open a fresh window (a fresh session) before asking, if in doubt.
- Point `active_provider` at a provider whose retention and training policy you
  have actually read — ideally a self-hosted, OpenAI-compatible endpoint on your
  own network. Any `base_url` works.
- Full-screen applications draw on the alternate screen, which is **never**
  captured. Only the main screen is.

### 2. The default provider is a public, zero-key endpoint

With no configuration, questions are answered through a built-in public endpoint
(`https://opencode.ai/zen/v1`) that requires no API key. That means
**out of the box your terminal context goes to a third party you have no account
with and no agreement with.**

The engine prints an inline notice the first time it answers through this
default (`engine/src/ffi/runtime.rs`), but the notice appears *with* the answer —
i.e. after the request has already been made. Configure your own provider
*before* the first question if that matters to you:

```bash
python3 scripts/moss-setup.py    # walks you through it
# or edit ~/.config/moss/config.jsonc and set active_provider
```

### 3. The engine can execute shell commands as you

The engine has a tool named `run_command`, gated by
`skills.allow_command_execution` in `~/.config/moss/config.jsonc`. **It defaults
to `true`.** When enabled:

- The command runs as `sh -lc '<command>'` — a *login* shell, so your profile
  scripts are sourced.
- It runs as your user, with your full privileges, with **no sandbox, no
  container, no chroot, and no allowlist**.
- It inherits the working directory of the kitty process. There is no confinement
  to a "workspace" directory despite the tool description saying "in the
  workspace".
- There is **no interactive confirmation prompt.** The model decides; the command
  runs; you see it stream past in blue.
- Timeout defaults to 30 s, capped at 120 s; the process group is killed on
  timeout or cancellation.

The engine also exposes `edit_file` (rewrite a line range of any file it can
reach), `trash_path` (move a path to the system Trash), and the read-only
`read_file` / `glob` / `grep`, which can reach `~` and `/`, not just a workspace.

Consequences you should weigh:

- A prompt-injection payload sitting in text the engine reads — a README, a log
  line, a filename, the output of a command you ran, a webpage a tool fetched —
  can attempt to steer the model into running a command. This is the standard
  agentic-LLM risk and it is not solved here.
- Turn it off if you do not want it:

  ```jsonc
  { "skills": { "allow_command_execution": false } }
  ```

  Then reload with `printf '\033]7717;reload-config\007'` in a Moss window, or
  restart. With it disabled, `run_command` refuses with an explicit error rather
  than executing.

- Do not run Moss Terminal as root. Do not run it on a machine where an
  unattended `rm` would be unrecoverable.

### 4. API keys on disk

- Keys live in `~/.config/moss/.env` (or `$MOSS_HOME/config/.env` when the
  *embedded* engine is running with that override), written with
  mode `0600` by `scripts/moss-setup.py`. If you create the file by hand,
  `chmod 600` it yourself — nothing enforces the mode at read time.
- `config.jsonc` should reference keys indirectly as `"$env:MOSS_..._API_KEY"`
  rather than inlining them. Resolution order is process environment first,
  `.env` second.
- Keys are readable by any process running as you, which includes anything the
  engine's own `run_command` executes. There is no keyring integration and no
  encryption at rest.
- `.gitignore` covers `.env`, but check `git diff --cached` before pushing.
- Engine logs go to `~/.cache/moss/logs/` (directory mode `0700`). Logs can
  contain captured terminal content. Attach them to a bug report only after
  reading them.

### 5. The bash `》` interception is best-effort — and leaks

Under **zsh** (an `accept-line` widget) and **fish** (a `\r` binding), the line is
intercepted *before* the shell executes it, and the question text reaches the
engine byte-for-byte.

Under **bash** there is no safe way to wrap Enter, so interception uses
`command_not_found_handle`, which fires **after** bash has already parsed and
begun executing the line. Therefore, in bash:

- A compound line is split first: for `》what is this; rm -rf ./build`, bash sends
  `》what is this` to the engine **and executes `rm -rf ./build` itself.** The
  same applies to `&&`, `||`, `|`, `&`, newlines inside quotes, and command
  substitution `$(...)` — which is expanded by bash *before* the handler ever
  runs.
- Word splitting has already occurred, so quotes, globs and runs of whitespace in
  your question are mangled (the handler sees `$*`).

**Use zsh or fish if you type anything that looks like shell syntax after `》`.**
This is documented as an architectural limitation, not a defect with a pending
fix. Shells other than zsh, fish and bash have no `》` trigger at all.

### 6. Trust boundaries and threat model

In scope — things we consider bugs and want reported:

- A way to make the engine execute a command when
  `skills.allow_command_execution` is `false`.
- Leaking an API key into terminal output, logs, or an LLM request body.
- A crafted terminal escape sequence (including a malicious `OSC 7717` payload,
  which any program writing to your tty can emit) that crashes kitty, corrupts
  its state, or causes the engine to act on attacker-chosen input in a way the
  user did not initiate.
- Anything that lets a *different* kitty window read another window's session,
  or that survives window close.
- Memory-unsafety across the FFI boundary, or a panic that unwinds into kitty.
- A world-writable or world-readable file created under `~/.config/moss`,
  `~/.cache/moss` or the install prefix.

Out of scope:

- "The AI sent my terminal contents to the provider." That is the documented
  purpose; see §1.
- "The AI ran a destructive command." If `allow_command_execution` is `true`, see
  §3 — that is the documented behaviour of the setting.
- "bash executed the tail of my compound `》` line." See §5.
- Attacks requiring an attacker who already has code execution as your user.
- Vulnerabilities in upstream kitty, in the LLM provider, or in third-party
  crates — report those to their maintainers (tell us too, so we can bump).

### 6a. Verifying what you install

Release artefacts are built by
[`.github/workflows/release.yml`](./.github/workflows/release.yml) from a tagged
commit, using no secret beyond the automatic `GITHUB_TOKEN`. The source tarball is
produced by `scripts/make-source-tarball.sh`, which is deterministic — you can
re-run it on the same tag and compare the sha256 it prints against
`SHA256SUMS.txt` in the release. Independently verify the pinned kitty tarball
against the sha256 recorded in `README.en.md` before building.
