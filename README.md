# Rust Coding Agent：分阶段教程

遵循 KISS 原则，每个阶段都保留可运行的版本。当前实现阶段 3：通过 OpenCode Go 的 Omen Alpha 调用只读工具查看项目。阶段 1 的完整源码保留在 `examples/stage1.rs`，可独立运行。

## 构建路线

| 阶段 | 目标 |
| --- | --- |
| 1：最小模型调用（已保留） | 同步发送一个问题，打印模型回答 |
| 2：多轮对话（已保留） | 输入循环、消息历史、退出和清空会话 |
| 3：只读工具（当前） | 列目录、读文件、搜索文本 |
| 4：Agent 循环 | 连续调用工具，回传结果，限制步数 |
| 5：修改与验证 | 修改代码、执行命令、根据检查结果修复 |
| 6：可用性整理 | 会话恢复、输出限制和运行体验 |

## 阶段 1：运行

需要 Rust 工具链，并且已在本机 OpenCode 中连接 Go 订阅。本项目使用 Rust 2024 edition，当前验证环境为 Rust 1.98.0；依赖版本由 `Cargo.lock` 固定。

直接运行，无需配置 API Key 环境变量：

```zsh
cargo run --example stage1 -- "用一个简短例子解释 Rust 的所有权"
```

程序自动读取 `~/.local/share/opencode/auth.json` 中 `opencode-go` 的 API 凭据。如果本机已有 `XDG_DATA_HOME` 设置，则读取该目录下的 `opencode/auth.json`。程序不会修改凭据文件，也不会打印或复制密钥到项目。凭据存储位置参见 [OpenCode CLI 文档](https://dev.opencode.ai/docs/cli/)。

回答输出到 stdout，进度和错误输出到 stderr。每次运行都是独立请求。

```sh
cargo run -- --help
cargo fmt --check
cargo clippy -- -D warnings
```

## 阶段 1：理解实现

从 `examples/stage1.rs` 的 `run()` 按顺序阅读：

1. 读取命令行问题，拒绝空输入。
2. 调用 `load_api_key()` 读取本机 OpenCode Go 凭据，检查认证类型和密钥是否有效。
3. 创建 `reqwest::blocking::Client`，设置连接和请求超时。
4. 发送带 Bearer 认证的 JSON 请求，并携带客户端名称及 `x-opencode-session` 会话标识。
5. 检查 HTTP 状态，从 `choices[0].message.content` 取出回答并打印。

阶段 1 使用三个依赖：`reqwest` 负责同步 HTTP 和 TLS，`serde_json` 构造与读取 JSON，`anyhow` 给失败步骤补充可读的错误信息。应用代码没有异步运行时配置、流式输出或框架抽象。

请求固定使用：

```text
POST https://opencode.ai/zen/go/v1/chat/completions
model: omen-alpha
```

`opencode-go/omen-alpha` 是 OpenCode 配置中的带提供商前缀名称；直接调用 API 时使用 `omen-alpha`。接入信息及请求头要求见 [OpenCode Go 官方文档](https://opencode.ai/docs/go/)（核对日期：2026-09-05）。

## 阶段 1：验收与边界

- 正常：本机已连接 Go 订阅，直接提问，终端出现模型回答，退出码为 0。
- 输入错误：没有问题、凭据文件无法读取、JSON 无效或 Go 凭据缺失时，程序在请求前报错，退出码为 1。
- 请求错误：显示网络错误或 HTTP 状态；不会自动重试。

真实模型回答需要在有效 Go 订阅与网络下验证，编译成功不等于远程接口调用成功。阶段 1 没有多轮记忆、代码读取、文件修改或工具调用。


## 阶段 2：运行多轮对话

```sh
cargo run
```

沿用本机 OpenCode Go 凭据，不需要新增配置。阶段 2 使用 `rustyline` 管理输入编辑和终端重绘，处理中文退格及光标移动。每行输入作为一个问题；等待回答后再输入下一行。

可以按下面的顺序验证（模型的具体措辞可能不同）：

```text
你> 请记住，我正在学习的项目叫小螃蟹。只回答收到。
助手> 收到。
你> 我正在学习的项目叫什么？
助手> 小螃蟹。
你> /clear
会话已清空。
你> 我正在学习的项目叫什么？不知道就直说。
助手> 我不知道你正在学习的项目名称。
你> /exit
```

空行被忽略；`/exit` 或输入结束（空输入时按 Ctrl-D）退出；`/clear` 清空历史并保留系统提示词。命令必须独占一行。带问题参数时仍支持原来的单次问答：

```sh
cargo run -- "解释 Rust 的借用"
```

## 阶段 2：理解实现

阶段 2 的完整代码保留在 `examples/stage2.rs`，按职责拆成几个小函数：

1. `run()` 根据是否传入问题参数，选择多轮或单次模式，并创建 HTTP 客户端。
2. `initial_messages()` 创建含一条 `system` 消息的历史，定义中文 Rust 助手的角色和能力边界。
3. `chat()` 读取一行输入，处理本地命令，把用户消息加入 `Vec<Value>`。
4. `ask()` 将消息列表发给模型，返回回答文本；`chat()` 再把回答以 `assistant` 角色追加到历史。
5. `new_session()` 创建会话标识。同一段对话复用该标识，清空后重新生成。

第二轮请求的消息顺序为：

```text
system → user（第一问）→ assistant（第一答）→ user（第二问）
```

模型的对话记忆来自每次重新发送的消息历史。会话请求头用于服务端缓存优化，不能替代消息历史。

如果请求失败，撤回刚追加的用户消息，保留之前完整的问答并继续等待输入。不会自动重试。凭据读取或终端读取失败则退出程序并报告错误。

## 阶段 2：验收与边界

已使用本机 Go 凭据完成真实三轮验证：先告知项目代号“蓝鲸742”，第二轮模型正确复述，清空后再问回答“不知道”。空行、清空、退出与 EOF 也已通过本地检查。

- 第二轮能回答第一轮提供的项目名称。
- `/clear` 后，后续请求不再携带之前的问答。
- 空行和本地命令不发起模型请求；`/exit` 和 EOF 均能正常退出。
- 阶段 1 示例和原来的单次问答入口仍能运行。

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --examples
```

历史只存于进程内存，退出后丢失；当前没有历史裁剪，长对话可能触及模型上下文或订阅限制，可用 `/clear` 开始新对话。每次只处理一行输入，仍使用同步、非流式请求。下一阶段增加只读代码工具。

输入编辑：使用左右方向键移动光标、Backspace 删除字符；Ctrl-C 取消尚未提交的输入并重新显示提示符。`rustyline` 负责字符宽度与换行重绘，避免直接依赖终端回显导致中文删除残留。该行为只针对等待输入时，模型请求中的中断不在本阶段处理。


## 阶段 3：只读代码工具

```sh
# 阶段 2 仍可独立运行（含中文输入修复）
cargo run --example stage2
# 当前阶段交互模式
cargo run
# 当前阶段单次任务
cargo run -- "请读取 Cargo.toml 和 src/main.rs，说明项目依赖和程序入口。"
cargo run -- "请列出当前目录，并搜索 src 中包含 load_api_key 的行。"
```

工作目录是启动程序时的当前目录。模型通过 `tools` 参数获得三个函数定义，选择调用并返回 JSON 参数；Rust 执行本地只读操作，将结果作为 `tool` 消息回传。终端显示工具名称，正文交给模型分析。

| 工具 | 参数 | 返回 |
| --- | --- | --- |
| `list_directory` | `path` | 最多 200 个直接子项与截断标识 |
| `read_file` | `path` | 最大 64 KiB 的 UTF-8 文件文本 |
| `search_text` | `path`、`query` | 字面匹配的文件、行号和文本 |

`src/main.rs` 管理对话和模型协议，`src/tools/` 管理工具。没有新增 Rust 依赖。

本阶段每轮最多两次模型请求：第一次允许模型选择一批工具（最多 8 个，顺序执行），第二次禁用工具，让模型根据结果总结。不需要工具时只有一次请求。目录未知时，可以先询问目录，再在下一轮要求读取具体文件；自动连续探索放在阶段 4。

对话历史包含 assistant 工具调用、每个带对应 `tool_call_id` 的结果和最终回答。工具执行错误会以 JSON 返回模型；模型请求失败则回滚整轮新增消息，保留此前完整历史。

所有工具路径先解析为真实路径，再检查是否位于工作目录内，拒绝 `..` 和指向目录外的符号链接。搜索不跟随遇到的符号链接，跳过隐藏项、`target`、`node_modules`；最多检查 2000 项、返回 50 条匹配，每条最多 300 字符，跳过不可读、过大或非文本文件。截断标识和跳过数量用于说明结果不一定完整。此路径检查不是用于防御其他进程同时替换文件的操作系统沙箱。

工具读取的内容会发送到 OpenCode Go。请在准备让模型查看的项目目录中运行；当前只读工具不提供文件修改或命令执行。

验证命令：

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --examples
cargo build
python3 tests/terminal_input.py
```


### 工具代码的阅读顺序

```text
src/tools/
├── mod.rs             # Tool 接口、工具注册、统一定义与调用入口
├── common.rs          # 共享路径边界检查和限量文本读取
├── list_directory.rs  # 目录工具：描述、参数、执行逻辑
├── read_file.rs       # 文件工具：描述、参数、执行逻辑
└── search_text.rs     # 搜索工具：描述、参数、执行逻辑
```

先读 `mod.rs` 的 `Tool` trait：每个工具实现 `name`、`description`、`parameters`、`execute` 四个方法。再从最简单的 `read_file.rs` 开始看实现。

Agent 只调用 `tools::definitions()` 和 `tools::execute(root, name, args)`。两者使用同一个 `registry()`，让提供给模型的工具与实际可执行的工具保持一致。`&dyn Tool` 表示通过统一接口调用不同工具，无需 Agent 判断具体工具类型。

新增工具时，创建对应文件并实现 `Tool`，然后在 `mod.rs` 声明模块、加入注册列表并调整数组长度；Agent 无需修改。工具集合固定在编译期，不涉及动态插件加载。

响应诊断：总结请求会明确要求根据现有工具结果回答。解析保留结束原因，兼容字符串和文本块正文；截断、总结阶段再次调用工具、空正文会分别报错。只检测推理内容是否存在，不打印推理原文，也不会将其替代最终答案。没有自动追加请求。
