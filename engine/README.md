# Moss Engine

Moss Engine 是 Moss Terminal 的 AI 引擎，用 Rust 编写。它以两种形态构建自同一份源码：

- **CLI 二进制 `moss`**——可在任意终端里独立使用的命令行 / REPL 助手。
- **嵌入式 cdylib `libmoss.so`**——被 Moss Terminal（kitty 分支）通过 `dlopen`/`ctypes` 懒加载进终端进程本身，为 `》问题` 交互提供能力。两者共享全部 agent、工具、知识库、记忆、配置逻辑，只是外壳不同。

Moss Terminal 的整体架构、》触发流程见仓库根目录的 [README.md](../README.md) 与 [部署与使用指南.md](../部署与使用指南.md)；本文件只讲引擎本身。

## 能力

- **工具系统**：`run_command` / `read_file` / `edit_file` / `grep` / `glob` / `trash_path` / 剪贴板 / 网络搜索与网页抓取 / 图片理解（vision）/ 应用补丁（apply_patch）/ MCP 外部工具 / 子代理（subagent）/ skills 等，统一通过 `tools/registry.rs` 注册；只读模式（plan）与完整模式（normal）对可写工具有不同的启用策略。
- **知识库**：SQLite + FTS5 的本地全文检索，`moss kb add/search/find/read/remove/reindex/embed` 管理；提问时是否检索由 AI 自行判断，无需用户手动指定。
- **记忆**：`memory/` 模块维护"发生过的事"与"信息中的知识点"两类记忆，按对话内容做联想召回，并带遗忘曲线（`forgetting_half_life_days` 等）避免无限膨胀。
- **上下文缓存分层策略**：对话历史 append-only 只追加；`agent/compact.rs`、`agent/overflow.rs` 实现按比例裁剪（`trim_at_ratio` / `trim_batch_ratio`）与超限时的处理策略（`on_overflow`），必要时用 LLM 摘要压缩旧历史，尽量维持 prompt cache 命中率。
- **多 provider**：OpenAI 兼容协议与 Anthropic 协议均支持，内置模板覆盖 opencode Zen（默认，免配置）、OpenAI、DeepSeek、Gemini、Xiaomi、Minimax、OpenRouter、本地 Ollama / LM Studio，也可完全自定义。
- **界面语言**：CLI、日志与工具状态输出支持中英文，`MOSS_LANG=zh`/`en` 或配置里的 `display.language` 控制，默认跟随系统 locale。

CLI 形态还可在 Moss Terminal 之外独立使用：`moss ask "问题"` 直接问一次，`moss fish-init` / `moss bash-init` / `moss zsh-init` 生成传统 shell 的 hook 脚本，让普通终端里也能直接对话——这是与 Moss Terminal 内嵌的 `》` 拦截机制完全独立的另一条路径，二者不冲突。

## 构建

```bash
cd engine

# CLI 二进制：默认 features，含 REPL、配置 TUI、shell 集成初始化、闹钟音频等
cargo build --release
./target/release/moss --version

# 嵌入式 cdylib：给 kitty 内嵌加载。--no-default-features 剔除 ALSA 音频、
# 终端 raw mode、以及任何可能调用 process::exit 的代码路径——这些都不该
# 出现在宿主进程（kitty）里
cargo build --release --no-default-features
ls target/release/libmoss.so
```

`cargo test` 跑单元测试；仓库根目录 `tests/e2e_mock_llm.py` 是针对 `libmoss.so` 的全链路端到端测试（见下）。

## FFI 一览

`src/ffi/mod.rs` 是嵌入 kitty 进程的 C ABI 边界，也是唯一对外导出的符号表——一共 12 个 `moss_*` 函数，全部经 `catch_unwind` 包裹，panic 永远不会跨越 FFI 边界。文本参数是不带 NUL 的 UTF-8 字节切片（长度显式传入），非法 UTF-8 会被有损替换。消费者是 kitty 的 `moss_integration.py`（ctypes）与 `moss_hook.c`（dlopen）。

| 函数 | 签名（简化） | 说明 |
|------|------|------|
| `moss_init` | `() -> i32` | 初始化引擎（幂等，可重复调用）。0 = 成功（含已初始化） |
| `moss_shutdown` | `()` | 关闭引擎，释放全部会话；可安全重复调用 |
| `moss_on_line` | `(window_id, kind, text, len)` | 喂入一条刚捕获的终端行；`kind` 是该行的 OSC 133 语义（0 未知 / 1 提示符 / 2 次级提示符 / 3 输出） |
| `moss_on_prompt_mark` | `(window_id, mark, data, len)` | 转发 OSC 133 事件；`mark` 为 `'A'`/`'C'`/`'D'`（命令开始 / 命令行 / 退出码） |
| `moss_on_cwd` | `(window_id, text, len)` | 更新窗口当前工作目录（来自 OSC 7） |
| `moss_ask` | `(window_id, text, len) -> i32` | 对该窗口发起一次提问。0 成功，-1 引擎未初始化，-2 该窗口已有请求在流式进行，-3 问题为空 |
| `moss_cancel` | `(window_id)` | 请求取消该窗口正在进行的回答 |
| `moss_poll_output` | `(window_id, buf, cap) -> usize` | 排空该窗口待输出的着色字节到 `buf`，返回实际写入的字节数（0 表示暂无） |
| `moss_stream_state` | `(window_id) -> i32` | 1 = 仍在流式或还有缓冲输出未取完，0 = 空闲 |
| `moss_window_closed` | `(window_id)` | kitty 关闭窗口时调用，释放该窗口的全部状态 |
| `moss_reload_config` | `() -> i32` | 重新读取 `config.jsonc`；空闲会话在下一次提问时生效。0 成功 |
| `moss_version` | `() -> *const u8` | 返回 NUL 结尾的引擎版本号字符串（静态存储，无需释放） |

排空协议：`moss_poll_output` 返回 0 且 `moss_stream_state` 也返回 0，才代表该窗口这一轮流式输出已经彻底结束。

## MOSS_HOME 沙箱

设置 `MOSS_HOME=/some/dir` 后，引擎的配置目录、数据目录、缓存目录、状态目录、图片目录会整体重定向为该目录下的 `config/`、`data/`、`cache/`、`state/`、`pictures/` 子目录，不再触碰 `~/.config/moss`、`~/.cache/moss` 等真实路径。未设置时使用标准 XDG 目录（`~/.config/moss`、`~/.cache/moss`、`~/.local/share/moss`、`~/.local/state/moss`）。

内嵌模式下每个 kitty 窗口对应 `state/windows/<window_id>/` 下独立的 `conversation.db`，会话完全隔离；配置、知识库、记忆则在同一进程内的所有窗口间共享。

`tests/e2e_mock_llm.py` 正是靠 `MOSS_HOME` 指向临时目录做到完全沙箱化：它像 `moss_integration.py` 一样用 ctypes 加载 `libmoss.so`，把 `active_provider` 指向本机起的一个 mock OpenAI 兼容 SSE 服务器，跑通行捕获 → `》` 提问 → 流式着色输出 → 多轮增量 → 多窗口隔离 → 取消的完整链路，全程只与 `127.0.0.1` 通信，不写入任何真实主机状态。

## 许可

Moss Engine 使用 MIT License 发布，见 `LICENSE`。
