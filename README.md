*中文 · [English](./README.en.md)*

# Moss Terminal

Moss Terminal 是一个基于 [kitty](https://github.com/kovidgoyal/kitty) 重构的、深度内嵌 AI 引擎的终端应用。在提示符下输入 `》问题` 即可直接触发 AI——AI 天然看得到你在这个窗口里执行过的命令和产生的输出，回答以流式彩色文本直接出现在终端中。没有分屏、没有独立窗口、没有后台服务。

## 特性

- **`》` 即触发**——输入 `》问题` 按回车直接对话，不需要快捷键或切换模式
- **全上下文感知**——AI 天然可见当前窗口执行过的命令与输出，无需手动粘贴
- **增量多轮**——每次提问只发送上一轮之后新增的终端内容，但对话历史完整保留
- **多窗口隔离**——每个 kitty 窗口是一个完全独立的会话，互不污染
- **流式彩色回显**——思考过程绿色、工具调用蓝色、正文默认色，直接写入当前终端
- **零守护进程**——AI 引擎是被 kitty 进程自己加载的动态库，没有 daemon、没有 socket
- **随 kitty 更新自动同步**——补丁化改动可在新版 kitty 源码上重新应用并自动化测试

完整的架构决策与对话模型见 [项目概要.md](./项目概要.md)。

## 架构

```
┌─────────────────────────────────────────────────────────────────┐
│                 kitty 进程（单进程，无子进程/无 daemon）           │
│                                                                   │
│  Layer 1 · kitty C 核心（10 个补丁 + moss_hook.c/.h）             │
│    screen_linefeed / shell_prompt_marking / OSC 7 → moss_* 调用   │
│    OSC 7717（Moss 控制序列）→ moss_handle_osc                     │
│    dlopen($MOSS_ENGINE_LIB 或 libmoss.so)；库缺失 = 空操作         │
│                             │ 函数指针调用（渲染线程，零阻塞）      │
│  Layer 2 · kitty Python 层（moss_integration.py）                 │
│    ctypes 绑定 libmoss.so；30ms 定时器排空引擎输出                 │
│    经 Screen 的 VT parser 注入窗口；流结束写换行哨兵解除阻塞        │
│                             │ ctypes FFI（13 个 moss_* 函数）      │
│  Layer 3 · Moss Engine（Rust：CLI 二进制 moss / cdylib libmoss.so）│
│    每窗口独立会话 · 增量上下文 · tokio 运行时 · 流式着色输出        │
│    工具系统 / 知识库 / 记忆 / 多 provider / 缓存分层策略            │
└─────────────────────────────────────────────────────────────────┘
```

`》` 交互链路：shell integration（zsh `accept-line` / fish `\r` 绑定 / bash `command_not_found_handle`，仅 `MOSS_TERMINAL=1` 时激活）拦截 `》` 开头的输入 → 发 OSC 7717 → 静默阻塞等待 → 引擎流式回答注入屏幕 → 换行哨兵 → 提示符回来。`Ctrl+C` 随时可中断。

## 快速开始

仓库里**没有** `kitty/` 目录——它是补丁生成物，克隆后第一步必须先把它生成出来：

```bash
# 0. 生成 kitty 构建树（首次必做）
curl -LO https://github.com/kovidgoyal/kitty/releases/download/v0.48.1/kitty-0.48.1.tar.xz
echo 'aadb428e20ad678c0a7969c0a80c46f391b49addeb7fda57b06a14a4d102fb1d  kitty-0.48.1.tar.xz' | sha256sum -c -
tar xf kitty-0.48.1.tar.xz
./scripts/moss-apply-patches.sh kitty-0.48.1 --check   # 干跑：补丁是否仍能干净应用
./scripts/moss-apply-patches.sh kitty-0.48.1           # 真正应用补丁与覆盖文件
mv kitty-0.48.1 kitty

# 1. 一键安装
python3 scripts/moss-setup.py
```

第 1 步交互式完成依赖检测、引擎与 kitty 编译、安装、AI provider 配置、桌面入口创建。完成后运行 `moss-terminal` 即可。完整流程、依赖清单（Arch 上别忘了 `simde`）、`--dry-run`/`--prefix` 选项、手动安装步骤见 [部署与使用指南.md](./部署与使用指南.md)。

## 开发构建

```bash
# 1. 编译引擎：CLI 二进制 + 嵌入式 cdylib
cd engine
cargo build --release
cargo build --release --no-default-features   # 覆盖出内嵌安全版 libmoss.so

# 2. 编译打了 Moss 补丁的 kitty（kitty/ 需先按“快速开始”第 0 步生成）
cd ../kitty
make debug

# 3. 从源码目录直接试跑，不安装到系统任何地方
cd ..
./scripts/moss-start.sh
```

构建纯净版（无 AI 集成）kitty：`MOSS_ENABLED=0 make debug`。

## 测试

```bash
cd engine && cargo test                    # 引擎单元测试
cd kitty && ./test.py --module moss        # kitty 侧的 moss 集成测试
cd kitty && ./test.py                      # kitty 完整测试套件
python3 tests/e2e_mock_llm.py              # 针对 libmoss.so 的沙箱化端到端测试（本地 mock LLM）
```

## 目录结构

```
engine/          Moss Engine（Rust）：CLI moss + 嵌入式 libmoss.so
kitty-patches/   对 kitty 的 10 个补丁（~146 行）        ← kitty 改动的唯一源头
kitty-overlay/   随补丁复制进 kitty 树的 4 个新文件      ← kitty 新增文件的唯一源头
scripts/         moss-setup.py / moss-start.sh / moss-sync-kitty.sh / moss-apply-patches.sh
                 / make-source-tarball.sh
packaging/       可选的 pacman hook + systemd user 定时器（只交付不自动安装）
packaging/aur/   PKGBUILD / PKGBUILD-git / .SRCINFO / moss-terminal.install
tests/           全链路端到端测试
specs/           需求规格
kitty/           已应用 Moss 补丁的 kitty 0.48.1 构建树
                 ★ 生成物：不在仓库里（已 gitignore）、不进源码包，由上面两个目录
                   经 scripts/moss-apply-patches.sh 重新生成；请勿直接编辑
```

逐条说明见 [文件清单.md](./文件清单.md)。

## 与上游 kitty 同步

```bash
./scripts/moss-sync-kitty.sh --notify-only     # 只检测新版本，不做任何改动
./scripts/moss-sync-kitty.sh                    # 下载新版 → 打补丁 → 编译 → 测试，结果落在 .sync-work/，不动仓库
./scripts/moss-sync-kitty.sh --adopt            # 上一步通过后，原子换入 kitty/（旧树保留为 kitty.prev/）
```

`--adopt` 是唯一会修改仓库里 `kitty/` 目录的操作，且只在前面的下载、打补丁、编译、测试全部成功之后才会执行；没有它，整个同步流程都是仓库之外的纯预演。详细选项与故障处理见 [部署与使用指南.md](./部署与使用指南.md#6-升级跟随上游-kitty)。

## 致谢

- [kitty](https://github.com/kovidgoyal/kitty) by Kovid Goyal——Moss Terminal 的终端内核
- Moss Engine 源自 Miyu 项目
- 上下文缓存分层策略参考 Reasonix

## 参与与安全

- [CONTRIBUTING.md](./CONTRIBUTING.md)——补丁序列工作流（**永远不要直接编辑 `kitty/`**）、测试与提交规范
- [SECURITY.md](./SECURITY.md)——安全模型、威胁边界、漏洞报告方式
- [CHANGELOG.md](./CHANGELOG.md)——版本变更记录

## 许可

Moss Terminal 作为一个**整体分发物**是 kitty 的衍生作品，因此以 **GPL-3.0-or-later** 授权——协议全文见 [LICENSE](./LICENSE)。

`engine/` 目录单独取出时是 **MIT** 授权，协议全文见 [LICENSE.MIT](./LICENSE.MIT)；由它单独构建出的 `libmoss.so` 与 `moss` CLI 同样是 MIT。

逐路径的授权与版权归属、以及"为什么组合作品是 GPL"的说明，以 [NOTICE.md](./NOTICE.md) 为准。
