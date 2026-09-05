# Rust Coding Agent：分阶段教程

遵循 KISS 原则，每个阶段都保留可运行的版本。当前实现阶段 1：在命令行输入问题，通过 OpenCode Go 的 Omen Alpha 获取一次回答。

## 构建路线

| 阶段 | 目标 |
| --- | --- |
| 1：最小模型调用（当前） | 同步发送一个问题，打印模型回答 |
| 2：多轮对话 | 输入循环、消息历史、退出和清空会话 |
| 3：只读工具 | 列目录、读文件、搜索文本 |
| 4：Agent 循环 | 连续调用工具，回传结果，限制步数 |
| 5：修改与验证 | 修改代码、执行命令、根据检查结果修复 |
| 6：可用性整理 | 会话恢复、输出限制和运行体验 |

## 阶段 1：运行

需要 Rust 工具链，并且已在本机 OpenCode 中连接 Go 订阅。本项目使用 Rust 2024 edition，当前验证环境为 Rust 1.98.0；依赖版本由 `Cargo.lock` 固定。

直接运行，无需配置 API Key 环境变量：

```zsh
cargo run -- "用一个简短例子解释 Rust 的所有权"
```

程序自动读取 `~/.local/share/opencode/auth.json` 中 `opencode-go` 的 API 凭据。如果本机已有 `XDG_DATA_HOME` 设置，则读取该目录下的 `opencode/auth.json`。程序不会修改凭据文件，也不会打印或复制密钥到项目。凭据存储位置参见 [OpenCode CLI 文档](https://dev.opencode.ai/docs/cli/)。

回答输出到 stdout，进度和错误输出到 stderr。每次运行都是独立请求。

```sh
cargo run -- --help
cargo fmt --check
cargo clippy -- -D warnings
```

## 阶段 1：理解实现

从 `src/main.rs` 的 `run()` 按顺序阅读：

1. 读取命令行问题，拒绝空输入。
2. 调用 `load_api_key()` 读取本机 OpenCode Go 凭据，检查认证类型和密钥是否有效。
3. 创建 `reqwest::blocking::Client`，设置连接和请求超时。
4. 发送带 Bearer 认证的 JSON 请求，并携带客户端名称及 `x-opencode-session` 会话标识。
5. 检查 HTTP 状态，从 `choices[0].message.content` 取出回答并打印。

依赖只保留三个：`reqwest` 负责同步 HTTP 和 TLS，`serde_json` 构造与读取 JSON，`anyhow` 给失败步骤补充可读的错误信息。应用代码没有异步运行时配置、流式输出或框架抽象。

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

真实模型回答需要在有效 Go 订阅与网络下验证，编译成功不等于远程接口调用成功。当前没有多轮记忆、代码读取、文件修改或工具调用；下一阶段只增加对话循环和消息历史。
