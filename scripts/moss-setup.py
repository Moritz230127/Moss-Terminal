#!/usr/bin/env python3
"""
Moss Terminal 安装脚本 / Moss Terminal installer
================================================

用法 / usage:
    python3 scripts/moss-setup.py [--dry-run] [--prefix DIR] [--yes]

架构提示 / architecture note: 没有守护进程。Rust engine 被编译为
cdylib (libmoss.so) 并被 kitty 进程 dlopen 加载；同一份 Rust 代码还
提供一个独立的 `moss` CLI 二进制。本脚本负责把两者和打过 Moss 补丁的
kitty 一起编译、安装、并写好 provider 配置。
There is no daemon: the Rust engine is compiled as a cdylib (libmoss.so)
that kitty dlopen()s into its own process; the same crate also produces a
standalone `moss` CLI binary. This script builds both plus the patched
kitty, installs everything, and writes the provider configuration.

流程 / phases (each a function below, orchestrated by main()), mapped onto
the 7 steps of FR-001 in specs/需求规格.md:
    1.   check_dependencies  — 检测环境 / probe build-time dependencies
    2-5. configure_provider  — 选择 Provider / 输入 API Key (不回显) /
         选择模型 / 配置确认 (展示完整配置并要求确认后才落盘写入
         ~/.config/moss/.env 与 config.jsonc)
         interactive provider setup: choose provider, enter the API key
         (hidden input), choose a model, then show the fully-resolved
         configuration and require confirmation before anything is written
    6.   build_engine + build_kitty + install — 编译两次 (cli 版 + 内嵌
         安全版) + `python3 setup.py linux-package` + 拷贝产物、写启动器
         two cargo builds, kitty's linux-package build, then copying
         artifacts and writing the launcher
    7.   create_desktop_entry + summary — 完成 / done

--dry-run 时不会执行任何构建命令、不会创建/写入/拷贝任何文件、也不会
读取任何交互输入 —— 只打印将要执行的操作（含步骤 5 配置确认的预览）。
With --dry-run nothing is built, nothing is written/copied/created, and no
interactive input is read — every action, including a preview of step 5
(confirm configuration), is only printed.

非交互式使用 / non-interactive use: 如果 stdin 不可读（例如
`< /dev/null`），可选的提示会退回各自的默认值而不是抛异常；到达步骤 5
「配置确认」时默认行为是放弃安装、不写入任何文件，除非传入 --yes。
--yes 只会跳过最终确认提示本身，绝不会替没有默认值的必填项（例如 API
Key）编造答案——那些字段在 stdin 不可读且确实需要它们时仍会清楚地报错
退出，而不是静默继续。
If stdin cannot be read (e.g. run with `< /dev/null`), optional prompts
fall back to their default instead of raising; by the time step 5
(confirm configuration) is reached, the default is to abort without
writing anything, unless --yes is given. --yes only skips the final
confirmation prompt — it never fabricates answers for required fields
with no default (e.g. an API key), which still fail loudly if stdin is
unreadable when they're actually needed, instead of silently proceeding.
"""

from __future__ import annotations

import argparse
import getpass
import json
import os
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

# ── 输出辅助 / output helpers (no emoji, just short tags) ──
GREEN = "\033[92m"
YELLOW = "\033[93m"
RED = "\033[91m"
BLUE = "\033[94m"
BOLD = "\033[1m"
RESET = "\033[0m"


def info(msg: str) -> None:
    print(f"{GREEN}[OK]{RESET}   {msg}")


def warn(msg: str) -> None:
    print(f"{YELLOW}[WARN]{RESET} {msg}")


def error(msg: str) -> None:
    print(f"{RED}[ERROR]{RESET} {msg}", file=sys.stderr)


def step(msg: str) -> None:
    print(f"\n{BOLD}{BLUE}==>{RESET} {BOLD}{msg}{RESET}")


# ── 路径 / paths ──
SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
ENGINE_DIR = REPO_ROOT / "engine"
KITTY_DIR = REPO_ROOT / "kitty"
BIN_DIR = Path.home() / ".local" / "bin"


def _xdg(var: str, fallback: Path) -> Path:
    v = os.environ.get(var)
    return Path(v) if v else fallback


CONFIG_DIR = _xdg("XDG_CONFIG_HOME", Path.home() / ".config") / "moss"
CACHE_DIR = _xdg("XDG_CACHE_HOME", Path.home() / ".cache") / "moss"

# Set by main() from --dry-run; read by the helpers below.
DRY_RUN = False
# Set by main() from --yes/--non-interactive: auto-confirms step 5 (the
# configuration confirmation prompt) instead of asking. It never fabricates
# answers for required fields that have no default (e.g. an API key) —
# those still prompt normally, or abort cleanly if stdin can't supply them.
AUTO_YES = False


# ── dry-run-aware filesystem helpers: the ONLY functions in this script
#    that touch disk. Every write path in the script funnels through one
#    of these so --dry-run is guaranteed to never create/modify anything. ──
def mkdir(path: Path) -> None:
    if DRY_RUN:
        print(f"  [dry-run] mkdir -p {path}")
        return
    path.mkdir(parents=True, exist_ok=True)


def copy_tree(src: Path, dst: Path) -> None:
    if DRY_RUN:
        print(f"  [dry-run] cp -a {src}/. {dst}/")
        return
    dst.mkdir(parents=True, exist_ok=True)
    shutil.copytree(src, dst, dirs_exist_ok=True)


def copy_file(src: Path, dst: Path, mode: Optional[int] = None) -> None:
    if DRY_RUN:
        modestr = f" (chmod {oct(mode)})" if mode is not None else ""
        print(f"  [dry-run] cp {src} -> {dst}{modestr}")
        return
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
    if mode is not None:
        os.chmod(dst, mode)


def write_text(path: Path, content: str, mode: Optional[int] = None) -> None:
    if DRY_RUN:
        modestr = f" (chmod {oct(mode)})" if mode is not None else ""
        print(f"  [dry-run] write {path} ({len(content.encode('utf-8'))} bytes){modestr}")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    if mode is not None:
        os.chmod(path, mode)


def run_cmd(cmd: list, cwd: Path) -> None:
    """Run cmd with inherited stdio (build output streams live); raise on
    failure. In --dry-run only the command line is printed."""
    display = f"cd {cwd} && {' '.join(shlex.quote(c) for c in cmd)}"
    if DRY_RUN:
        print(f"  [dry-run] {display}")
        return
    print(f"  $ {display}")
    result = subprocess.run(cmd, cwd=str(cwd))
    if result.returncode != 0:
        raise SystemExit(
            f"命令失败 / command failed (exit {result.returncode}): {display}"
        )


# ── a. check_dependencies ──
def _which_ok(name: str) -> bool:
    return shutil.which(name) is not None


def _pkgconfig_ok(pkg: str) -> bool:
    if not _which_ok("pkg-config"):
        return False
    try:
        return subprocess.run(
            ["pkg-config", "--exists", pkg],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        ).returncode == 0
    except OSError:
        return False


def check_dependencies() -> None:
    step("检测依赖 / checking dependencies (1/7)")

    hard_ok = True

    def hard(name: str, ok: bool, hint: str) -> None:
        nonlocal hard_ok
        if ok:
            info(f"{name}: 已找到 / found")
        else:
            hard_ok = False
            msg = f"{name}: 未找到 / not found — {hint}"
            (warn if DRY_RUN else error)(msg)

    hard("cargo", _which_ok("cargo"), "安装 rustup: https://rustup.rs")
    hard("rustc", _which_ok("rustc"), "安装 rustup: https://rustup.rs")
    hard(
        "python3 >= 3.8",
        sys.version_info >= (3, 8),
        f"当前版本 / current version: {sys.version.split()[0]}",
    )
    hard(
        "gcc/clang",
        _which_ok("gcc") or _which_ok("clang"),
        "安装 C 编译器 / install a C compiler (gcc or clang)",
    )

    # 软依赖：仅警告，不阻止安装（kitty 构建可能仍然失败，但由 build_kitty
    # 阶段的真实错误信息来报告）。
    # Soft dependencies: warn only, never block (an actual build failure in
    # build_kitty will report the real error if these turn out to matter).
    soft_bins = [
        ("pkg-config", "kitty 构建需要 / required by kitty's build"),
        ("go", "编译内置 kittens 需要, 参见 kitty/go.mod / needed to build the bundled kittens, see kitty/go.mod"),
    ]
    for name, hint in soft_bins:
        if _which_ok(name):
            info(f"{name}: 已找到 / found")
        else:
            warn(f"{name}: 未找到 / not found — {hint}")

    if _which_ok("pkg-config"):
        for pkg in ("harfbuzz", "fontconfig", "freetype2", "cairo", "openssl", "wayland-protocols"):
            if _pkgconfig_ok(pkg):
                info(f"pkg-config {pkg}: 已找到 / found")
            else:
                warn(f"pkg-config {pkg}: 未找到 / not found (kitty 构建可能需要 / kitty's build may need it)")

    if not hard_ok:
        if DRY_RUN:
            warn(
                "缺少上述必需依赖时正式安装会失败；仅因为 --dry-run 才继续预览 / "
                "the real install would fail without the items above; continuing only because this is --dry-run"
            )
        else:
            error("缺少必需依赖，安装终止 / missing required dependencies, aborting")
            raise SystemExit(1)


# ── b. build_engine ──
def build_engine() -> dict:
    step("编译 Moss Engine / building the Moss engine (two builds) (6/7)")
    # Two cargo builds are required here, IN THIS ORDER:
    #   1. `cargo build --release` (default features, includes "cli") builds
    #      the full `moss` CLI binary (config TUI, REPL, alarm audio via
    #      rodio, raw-mode terminal takeover) for standalone terminal use.
    #   2. `cargo build --release --no-default-features` re-links libmoss.so
    #      WITHOUT that cli-only machinery, which would be unsafe to run
    #      inside the kitty process itself (raw-mode takeover / audio /
    #      process::exit calls all assume they own the terminal). The `moss`
    #      binary target declares `required-features = ["cli"]`, so cargo
    #      simply skips rebuilding it during this second pass: the CLI
    #      binary produced by build 1 is left untouched on disk while
    #      target/release/libmoss.so gets overwritten with the embed-safe
    #      version. Swap the order and you ship a libmoss.so with the audio
    #      stack linked into kitty's address space.
    run_cmd(["cargo", "build", "--release"], cwd=ENGINE_DIR)
    run_cmd(["cargo", "build", "--release", "--no-default-features"], cwd=ENGINE_DIR)

    libmoss = ENGINE_DIR / "target" / "release" / "libmoss.so"
    moss_cli = ENGINE_DIR / "target" / "release" / "moss"
    if not DRY_RUN:
        if not libmoss.exists():
            raise SystemExit(f"编译产物缺失 / missing build artifact: {libmoss}")
        if not moss_cli.exists():
            raise SystemExit(f"编译产物缺失 / missing build artifact: {moss_cli}")
    info(f"engine cdylib (embed-safe): {libmoss}")
    info(f"moss CLI: {moss_cli}")
    return {"libmoss": libmoss, "moss_cli": moss_cli}


# ── c. build_kitty ──
def build_kitty() -> Path:
    step("编译 Kitty / building kitty (linux-package) (6/7)")
    run_cmd(["python3", "setup.py", "linux-package"], cwd=KITTY_DIR)
    pkg_dir = KITTY_DIR / "linux-package"
    if not DRY_RUN and not pkg_dir.exists():
        raise SystemExit(f"构建失败，未找到 / build failed, missing: {pkg_dir}")
    info(f"kitty package: {pkg_dir}")
    return pkg_dir


# ── d. install ──
def install(prefix: Path, engine_artifacts: dict, kitty_pkg_dir: Path) -> dict:
    step(f"安装到 {prefix} / installing to {prefix} (6/7)")

    mkdir(prefix)
    copy_tree(kitty_pkg_dir, prefix)

    # Where does kitty auto-discover libmoss.so? Verified against
    # kitty/kitty/moss_integration.py:_candidate_library_paths(), which
    # computes base = dirname(dirname(abspath(<this file>))). After
    # `python3 setup.py linux-package`, moss_integration.py ends up at
    # <prefix>/lib/kitty/kitty/moss_integration.py (setup.py's libdir is
    # <ddir>/lib/kitty, and the kitty python package is copied into
    # <libdir>/kitty) — so base == <prefix>/lib/kitty, i.e. the directory
    # that directly contains the kitty/, kittens/ and shell-integration/
    # subdirs, NOT <prefix> itself. Dropping libmoss.so there is what makes
    # auto-discovery work with zero env vars. We still export
    # MOSS_ENGINE_LIB in the launcher below anyway (checked first, and it's
    # what protects a user who invokes <prefix>/bin/kitty directly through
    # some other wrapper) — the file lives at the one path that satisfies
    # both lookup strategies simultaneously.
    engine_lib_dst = prefix / "lib" / "kitty" / "libmoss.so"
    copy_file(engine_artifacts["libmoss"], engine_lib_dst)

    mkdir(BIN_DIR)
    moss_cli_dst = BIN_DIR / "moss"
    copy_file(engine_artifacts["moss_cli"], moss_cli_dst, mode=0o755)

    kitty_bin = prefix / "bin" / "kitty"
    launcher_path = BIN_DIR / "moss-terminal"
    launcher_script = (
        "#!/usr/bin/env bash\n"
        "# Generated by moss-setup.py -- re-run the installer instead of editing this by hand.\n"
        "set -euo pipefail\n"
        f'export MOSS_ENGINE_LIB="{engine_lib_dst}"\n'
        f'exec "{kitty_bin}" "$@"\n'
    )
    write_text(launcher_path, launcher_script, mode=0o755)

    info(f"kitty: {kitty_bin}")
    info(f"libmoss.so: {engine_lib_dst}")
    info(f"moss CLI: {moss_cli_dst}")
    info(f"启动器 / launcher: {launcher_path}")

    if str(BIN_DIR) not in os.environ.get("PATH", "").split(os.pathsep):
        warn(
            f"{BIN_DIR} 不在 PATH 中，请加入 shell 配置文件 / not on PATH, add it to your shell rc "
            f'(e.g. export PATH="{BIN_DIR}:$PATH")'
        )

    return {
        "prefix": prefix,
        "kitty_bin": kitty_bin,
        "libmoss": engine_lib_dst,
        "moss_cli": moss_cli_dst,
        "launcher": launcher_path,
    }


# ── e. configure_provider ──
# opencode Zen 的模型 id 与 engine/src/default_models.rs 保持一致。该 Rust
# 文件是只读的（本次任务禁止修改），且 Python 脚本没有办法导入它，所以这
# 里把字面量原样抄一份，并在注释里钉住来源，方便以后发现两边漂移。
# opencode Zen's model ids mirror engine/src/default_models.rs (read-only
# for this script, and not importable from Python). The literals are
# copied here with a pointer back to the source of truth so a future
# drift between the two is easy to spot.
_OPENCODE_DEFAULT_CHAT_MODEL = "big-pickle"  # == OPENCODE_DEFAULT_CHAT_MODEL
_OPENCODE_DEFAULT_VISION_MODEL = "mimo-v2.5-free"  # == OPENCODE_DEFAULT_VISION_MODEL

PRESETS = [
    {
        "key": "opencode",
        "label": "opencode Zen — 免费公共端点，无需 API Key / free public endpoint, no key needed",
        # id MUST be exactly "opencode" (engine's OPENCODE_PROVIDER_ID): the
        # engine's is_opencode_zen() check only recognizes "opencode" or
        # "opencodezen" and falls back to the public key "public" for those
        # ids when api_key is absent. Any other id (e.g. "opencode-zen")
        # would make resolved_api_keys() fail with "missing API key".
        "id": "opencode",
        "display_name": "opencode Zen",
        "base_url": "https://opencode.ai/zen/v1",
        "needs_key": False,
        "model_choices": [
            {"id": _OPENCODE_DEFAULT_CHAT_MODEL, "note": "默认对话模型 / default chat model"},
            {"id": _OPENCODE_DEFAULT_VISION_MODEL, "note": "视觉/多模态模型 / vision-capable model"},
        ],
    },
    {
        "key": "deepseek",
        "label": "DeepSeek",
        "id": "deepseek",
        "display_name": "DeepSeek",
        "base_url": "https://api.deepseek.com",
        "needs_key": True,
        "model_choices": [
            {"id": "deepseek-chat", "note": "通用对话模型 / general-purpose chat model"},
            {"id": "deepseek-reasoner", "note": "推理模型 / reasoning model"},
        ],
    },
    {
        "key": "openai",
        "label": "OpenAI",
        "id": "openai",
        "display_name": "OpenAI",
        "base_url": "https://api.openai.com/v1",
        "needs_key": True,
        "model_choices": [
            {"id": "gpt-4o-mini", "note": "小型、便宜 / small & cheap"},
            {"id": "gpt-4o", "note": "旗舰多模态 / flagship multimodal"},
            {"id": "gpt-4.1", "note": "长上下文 / long-context"},
        ],
    },
    {
        "key": "custom",
        "label": "自定义 (OpenAI 兼容 API) / custom (OpenAI-compatible API)",
        "id": None,
        "display_name": None,
        "base_url": None,
        "needs_key": None,
        "model_choices": [],
    },
]


def sanitize_env_suffix(provider_id: str) -> str:
    """ASCII-only, upper-cased suffix for the MOSS_<SUFFIX>_API_KEY env var
    name. Deriving this from provider_id (always ASCII-safe by construction
    here) rather than a free-form display name is what avoids bug S13,
    where a non-ASCII display name produced an invalid (non-ASCII) env var
    name that the engine's .env parser silently ignored."""
    out = "".join(ch if ("A" <= ch <= "Z" or "0" <= ch <= "9") else "_" for ch in provider_id.upper())
    return out.strip("_") or "PROVIDER"


def ask(prompt: str, default: str = "") -> str:
    """input() with a default. On EOF (stdin unreadable, e.g. `< /dev/null`)
    this falls back to the default instead of raising, so a non-interactive
    run never crashes on an optional prompt — see the module docstring."""
    suffix = f" [{default}]" if default else ""
    try:
        val = input(f"{prompt}{suffix}: ").strip()
    except EOFError:
        print()
        return default
    return val or default


def ask_required(prompt: str) -> str:
    """Like ask() but for fields with no sane default (custom provider id,
    API key, ...). EOF here means the wizard genuinely cannot proceed, so
    it aborts cleanly with a clear message instead of a raw traceback."""
    while True:
        try:
            val = input(f"{prompt}: ").strip()
        except EOFError:
            print()
            raise SystemExit(
                "无法在非交互式环境下读取必填输入，安装终止 / "
                f"cannot read required input non-interactively, aborting ({prompt})"
            )
        if val:
            return val
        warn("不能为空，请重新输入 / cannot be empty, try again")


def ask_choice(n: int, default: int = 1) -> int:
    """Numbered-menu prompt. Enter alone -- or EOF -- picks `default`, so a
    non-interactive run always resolves to a deterministic choice instead
    of hanging or crashing."""
    while True:
        try:
            raw = input(f"请输入编号 / enter choice (1-{n}) [{default}]: ").strip()
        except EOFError:
            print()
            return default
        if not raw:
            return default
        if raw.isdigit() and 1 <= int(raw) <= n:
            return int(raw)
        warn("无效选择 / invalid choice")


def ask_yes_no(prompt: str, default_yes: bool) -> bool:
    default = "y" if default_yes else "n"
    raw = ask(f"{prompt} (y/n)", default).strip().lower()
    return raw.startswith("y")


def strip_jsonc_comments(text: str) -> str:
    """Remove // line comments and /* */ block comments that live outside
    string literals, mirroring the engine's json_comments crate closely
    enough to safely re-parse a config.jsonc for merging. String contents
    (e.g. "https://...") are left untouched."""
    out = []
    i, n = 0, len(text)
    in_string = False
    escape = False
    while i < n:
        c = text[i]
        if in_string:
            out.append(c)
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == '"':
                in_string = False
            i += 1
            continue
        if c == '"':
            in_string = True
            out.append(c)
            i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            if j == -1:
                break
            out.append("\n")
            i = j + 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            j = text.find("*/", i + 2)
            i = n if j == -1 else j + 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def write_env_key(env_path: Path, key_name: str, value: str) -> None:
    """Upsert KEY=value into the .env file, preserving every other line
    (so re-running the wizard for a second provider doesn't clobber the
    first one's key)."""
    quote = '"' if '"' not in value else "'"
    new_line = f"{key_name}={quote}{value}{quote}"

    lines = []
    replaced = False
    if env_path.exists():
        for line in env_path.read_text(encoding="utf-8").splitlines():
            s = line.strip()
            if s.startswith(f"{key_name}=") or s.startswith(f"export {key_name}="):
                lines.append(new_line)
                replaced = True
            else:
                lines.append(line)
    if not replaced:
        lines.append(new_line)

    env_path.parent.mkdir(parents=True, exist_ok=True)
    env_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    os.chmod(env_path, 0o600)


def write_merged_config(config_file: Path, provider_obj: dict, active_id: str) -> None:
    """Back up any existing config.jsonc, then merge in this provider
    (upsert by id) and set it active -- every other top-level key (and
    every other provider) is left exactly as-is. If the existing file
    can't be parsed as JSONC it is still backed up, then replaced with a
    fresh minimal config (bug S12 used to clobber it with no backup at
    all)."""
    existing: dict = {}
    if config_file.exists():
        ts = time.strftime("%Y%m%d-%H%M%S")
        backup_path = config_file.with_name(config_file.name + f".bak.{ts}")
        shutil.copy2(config_file, backup_path)
        info(f"已备份旧配置 / backed up previous config -> {backup_path}")

        raw = config_file.read_text(encoding="utf-8", errors="replace")
        try:
            parsed = json.loads(strip_jsonc_comments(raw))
            if isinstance(parsed, dict):
                existing = parsed
            else:
                warn("旧配置根节点不是对象，将重新写入 / previous config root was not an object; writing fresh")
        except Exception as exc:  # noqa: BLE001 - report and fall back to fresh config
            warn(f"旧配置无法解析 ({exc})，将重新写入 / previous config could not be parsed; writing fresh")

    providers = existing.get("providers")
    providers = list(providers) if isinstance(providers, list) else []
    providers = [p for p in providers if not (isinstance(p, dict) and p.get("id") == active_id)]
    providers.append(provider_obj)

    merged = dict(existing)
    merged["providers"] = providers
    merged["active_provider"] = active_id

    header = (
        "// 由 moss-setup.py 写入/合并：仅更新了 active_provider 与本 provider 条目，其余键保持不变。\n"
        "// Written/merged by moss-setup.py: only active_provider and this provider entry were touched.\n"
    )
    content = header + json.dumps(merged, indent=2, ensure_ascii=False) + "\n"
    config_file.parent.mkdir(parents=True, exist_ok=True)
    config_file.write_text(content, encoding="utf-8")


def choose_model(preset: dict) -> tuple[str, list]:
    """步骤 4 / FR-001 step 4「选择模型」：按 provider 列出已知模型供选择，
    始终保留一个"其他"选项用于手填任意模型 id；直接回车选中列表第一项，
    与旧版本"回车用建议值"的行为保持一致。opencode 的候选 id 来自
    engine/src/default_models.rs（见 PRESETS 定义处的注释）；其余
    provider 的候选 id 是各自公开文档里的模型名 —— engine 把它们当作
    OpenAI 兼容 API 透传，本身不校验/维护模型列表。
    Lists this provider's known models (numbered) plus a free-form "other"
    escape hatch; Enter alone selects the first entry, preserving the old
    "Enter accepts the suggestion" behaviour."""
    choices = preset.get("model_choices") or []
    print("\n可用模型 / available models:")
    for i, c in enumerate(choices, 1):
        print(f"  {i}. {c['id']}  ({c['note']})")
    other_idx = len(choices) + 1
    print(f"  {other_idx}. 其他 / other -- 手动输入模型 id / type your own model id")

    idx = ask_choice(other_idx, default=1 if choices else other_idx)
    if idx == other_idx:
        default_model = ask_required("模型 id / model id")
    else:
        default_model = choices[idx - 1]["id"]

    extra_raw = ask(
        "其他要一并启用的模型，可选、逗号分隔 / additional models to enable, optional, comma-separated",
        "",
    )
    models = [default_model]
    for m in (x.strip() for x in extra_raw.split(",")):
        if m and m not in models:
            models.append(m)
    return default_model, models


def _api_key_status(needs_key: bool, env_path: Path) -> str:
    if not needs_key:
        return "不需要 / not required"
    return (
        f"已提供，内容不会显示 (将写入 {env_path}, 权限 0600) / "
        f"provided, value not shown (will be written to {env_path}, mode 0600)"
    )


def confirm_configuration(
    *,
    provider_id: str,
    display_name: str,
    base_url: str,
    models: list,
    default_model: str,
    needs_key: bool,
    env_path: Path,
    config_file: Path,
    prefix: Path,
) -> bool:
    """步骤 5 / FR-001 step 5「配置确认」：在写入任何文件之前完整展示已收
    集到的配置并要求确认；绝不打印 API key 本身，只说明是否已提供、将写
    到哪里。Shows the fully-resolved configuration before anything is
    written and asks for confirmation. The API key value is never
    printed — only whether one was collected and where it will land."""
    step("配置确认 / confirm configuration (5/7)")
    print(f"  Provider id         : {provider_id}")
    print(f"  显示名称 / name      : {display_name}")
    print(f"  Base URL            : {base_url}")
    print(f"  可用模型 / models    : {', '.join(models)}")
    print(f"  默认模型 / default   : {default_model}")
    print(f"  API Key             : {_api_key_status(needs_key, env_path)}")
    print(f"  配置文件 / config    : {config_file}")
    print(f"  安装前缀 / prefix    : {prefix}")
    print(
        "  将执行 / will do     : 编译带 Moss 补丁的 kitty + Moss Engine "
        "(两次 cargo build) + 安装到上述前缀并写启动器 / build the "
        "Moss-patched kitty + the Moss engine (two cargo builds), then "
        "install to the prefix above and write the launcher"
    )
    print()
    return ask_yes_no("确认以上配置并继续安装? / confirm the above and proceed?", False)


def configure_provider(prefix: Path) -> None:
    step("配置 AI Provider / configure AI provider")

    if DRY_RUN:
        info(
            "--dry-run: 交互式配置向导预览，不读取任何输入、不写入任何文件 / "
            "interactive wizard preview -- no input is read, nothing is written"
        )
        step("选择 Provider / choose provider (2/7)")
        info("--dry-run: 跳过 / skipped")
        step("输入 API Key / enter API key (3/7)")
        info("--dry-run: 跳过，.env 不会被写入 / skipped, .env would not be written")
        step("选择模型 / choose model (4/7)")
        info("--dry-run: 跳过 / skipped")
        step("配置确认 / confirm configuration (5/7)")
        info("--dry-run: 跳过，config.jsonc 不会被写入 / skipped, config.jsonc would not be written")
        return

    env_path = CONFIG_DIR / ".env"
    config_file = CONFIG_DIR / "config.jsonc"

    while True:
        step("选择 Provider / choose provider (2/7)")
        print("可选预设 / available presets:")
        for i, p in enumerate(PRESETS, 1):
            print(f"  {i}. {p['label']}")
        preset = PRESETS[ask_choice(len(PRESETS), default=1) - 1]

        if preset["key"] == "custom":
            provider_id = ask_required(
                "Provider ID (短 ascii 标识，如 my-llm / short ascii id, e.g. my-llm)"
            )
            display_name = ask("显示名称 / display name", provider_id)
            base_url = ask_required("API Base URL")
            needs_key = ask_yes_no("是否需要 API Key? / does it need an API key?", True)
        else:
            provider_id = preset["id"]
            display_name = preset["display_name"]
            base_url = preset["base_url"]
            needs_key = preset["needs_key"]

        # 步骤 3 / FR-001 step 3：在选择模型之前输入 API Key（不回显），
        # 与需求规格表格的顺序保持一致（旧版本先选模型、后要 key）。
        api_key = None
        env_var_name = None
        if needs_key:
            env_var_name = f"MOSS_{sanitize_env_suffix(provider_id)}_API_KEY"
            step("输入 API Key / enter API key (3/7, 输入内容不会显示 / input is hidden)")
            while True:
                try:
                    api_key = getpass.getpass(f"{display_name} API Key: ").strip()
                except EOFError:
                    print()
                    raise SystemExit(
                        "无法在非交互式环境下读取 API Key，安装终止 / "
                        "cannot read the API key non-interactively, aborting"
                    )
                if api_key:
                    break
                warn("API Key 不能为空 / API key cannot be empty")

        step("选择模型 / choose model (4/7)")
        default_model, models = choose_model(preset)

        if AUTO_YES:
            step("配置确认 / confirm configuration (5/7)")
            info("--yes: 自动确认，跳过确认提示 / auto-confirmed, prompt skipped")
            proceed = True
        else:
            proceed = confirm_configuration(
                provider_id=provider_id,
                display_name=display_name,
                base_url=base_url,
                models=models,
                default_model=default_model,
                needs_key=needs_key,
                env_path=env_path,
                config_file=config_file,
                prefix=prefix,
            )

        if proceed:
            break

        if ask_yes_no(
            "重新选择 provider/模型? (否 = 放弃，不写入任何文件) / "
            "redo provider & model selection? (no = abort, nothing written)",
            False,
        ):
            print()
            continue

        print()
        info("已取消，未写入任何文件 / cancelled, nothing was written")
        raise SystemExit(0)

    provider_obj = {
        "id": provider_id,
        "display_name": display_name,
        "base_url": base_url,
        "models": models,
        "default_model": default_model,
    }
    if env_var_name:
        provider_obj["api_key"] = f"$env:{env_var_name}"

    step("写入配置 / writing configuration")
    if env_var_name:
        write_env_key(env_path, env_var_name, api_key)
        info(f"API Key 已写入 (0600, 未回显) / API key written (mode 0600, never echoed): {env_path}")

    write_merged_config(config_file, provider_obj, provider_id)
    info(f"配置已写入 / config written: {config_file}")


# ── f. create_desktop_entry ──
def create_desktop_entry(launcher_path: Path) -> Path:
    step("创建桌面入口 / creating desktop entry (7/7)")

    template_path = SCRIPT_DIR / "moss-terminal.desktop"
    if template_path.exists():
        lines = template_path.read_text(encoding="utf-8").splitlines()
    else:
        # Fallback in case this script is ever copied out on its own.
        lines = [
            "[Desktop Entry]",
            "Type=Application",
            "Name=Moss Terminal",
            "Comment=AI-native terminal built on kitty / 基于 kitty 的 AI 原生终端",
            "TryExec=moss-terminal",
            "Exec=moss-terminal",
            "Icon=kitty",
            "Categories=System;TerminalEmulator;",
            "Terminal=false",
            "StartupNotify=true",
        ]

    abs_launcher = str(launcher_path)
    out_lines = []
    for line in lines:
        if line.startswith("Exec="):
            out_lines.append(f"Exec={abs_launcher}")
        elif line.startswith("TryExec="):
            out_lines.append(f"TryExec={abs_launcher}")
        else:
            out_lines.append(line)

    dest = Path.home() / ".local" / "share" / "applications" / "moss-terminal.desktop"
    write_text(dest, "\n".join(out_lines) + "\n", mode=0o755)
    info(f"桌面入口 / desktop entry: {dest}")
    return dest


# ── g. summary ──
def summary(install_info: dict, desktop_entry: Path) -> None:
    step("安装完成 / installation complete (7/7)")
    info(f"安装目录 / install prefix: {install_info['prefix']}")
    info(f"kitty: {install_info['kitty_bin']}")
    info(f"moss CLI: {install_info['moss_cli']}")
    info(f"engine 库 / engine library: {install_info['libmoss']}")
    info(f"启动器 / launcher: {install_info['launcher']}")
    info(f"桌面入口 / desktop entry: {desktop_entry}")
    print()
    info("启动方式 / to launch: moss-terminal  (或从应用菜单 / or from the application menu)")
    info("使用方法 / usage: 在终端提示符输入 》你的问题 并回车 / at the shell prompt type 》your question and press enter")
    info(f"配置文件 / config file: {CONFIG_DIR / 'config.jsonc'}")
    info(f"日志目录 / logs: {CACHE_DIR / 'logs'}")
    print()
    if DRY_RUN:
        warn("这是 --dry-run 预览：没有任何命令被执行、没有任何文件被写入 / "
             "this was a --dry-run preview: no command ran and no file was written")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Moss Terminal 安装脚本 / Moss Terminal installer",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="只打印将要执行的操作，不构建、不写入任何文件 / print planned actions only, build and write nothing",
    )
    parser.add_argument(
        "--prefix",
        default=str(Path.home() / ".local" / "share" / "moss-terminal"),
        help="安装前缀 (默认 ~/.local/share/moss-terminal) / install prefix (default: ~/.local/share/moss-terminal)",
    )
    parser.add_argument(
        "--yes",
        "--non-interactive",
        dest="yes",
        action="store_true",
        help=(
            "自动确认步骤 5「配置确认」，不再询问 / auto-confirm step 5 "
            "(the configuration confirmation prompt) instead of asking. "
            "This does NOT fabricate answers for required fields that have "
            "no default, e.g. an API key -- those still prompt normally, "
            "and still abort cleanly if stdin can't supply them. Without "
            "this flag, if the confirmation prompt itself can't be read "
            "(e.g. stdin is /dev/null) the installer aborts without "
            "writing anything rather than silently proceeding."
        ),
    )
    args = parser.parse_args()

    global DRY_RUN, AUTO_YES
    DRY_RUN = args.dry_run
    AUTO_YES = args.yes
    prefix = Path(args.prefix).expanduser()

    print(f"{BOLD}Moss Terminal 安装程序 / installer{RESET}")
    if DRY_RUN:
        print(f"{YELLOW}(--dry-run: 预览模式，不会执行任何操作 / preview mode, nothing will be executed){RESET}")
    print("=" * 60)

    # 顺序对应 FR-001 的 7 个步骤：1 检测环境 -> 2-5 provider/key/model/确认
    # (configure_provider，全部在写任何文件之前完成) -> 6 编译/安装 -> 7 完成。
    # Order mirrors FR-001's 7 steps: 1 detect environment -> 2-5
    # provider/key/model/confirm (configure_provider, all before anything
    # is written) -> 6 build/install -> 7 done.
    check_dependencies()
    configure_provider(prefix)
    engine_artifacts = build_engine()
    kitty_pkg_dir = build_kitty()
    install_info = install(prefix, engine_artifacts, kitty_pkg_dir)
    desktop_entry = create_desktop_entry(install_info["launcher"])
    summary(install_info, desktop_entry)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print()
        error("已取消 / cancelled")
        sys.exit(130)
    except SystemExit:
        raise
    except Exception as exc:  # noqa: BLE001 - top-level safety net
        error(str(exc))
        sys.exit(1)
