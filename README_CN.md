# HAPIR

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/Shirasuki/hapir)

Claude Code / Codex / Gemini / OpenCode 的控制台，随时随地 vibe coding。

运行 AI 编程会话，通过 Web / PWA / Telegram Mini App 远程控制。

> **说明**：本项目大量基于 [@tiann](https://github.com/tiann) 的 [hapi](https://github.com/tiann/hapi)，大部分代码沿用自 hapi，并对部分功能进行了修改。配置和协议与原项目**不互通**。

[English](./README.md)

## 功能

- **多代理支持** — Claude Code、Codex、Gemini、OpenCode
- **远程控制** — Web UI、PWA、Telegram Mini App
- **无缝切换** — 本地与远程会话自由切换
- **权限管理** — 在手机上审批 AI 的操作请求
- **终端访问** — 通过 WebSocket 远程 PTY
- **文件浏览** — 带 git 状态和 diff 的文件浏览器
- **语音助手** — ElevenLabs 集成
- **推送通知** — Web Push 和 Telegram 机器人
- **离线支持** — PWA 离线消息缓冲

## 架构

Rust 工作区，包含六个 crate 和一个根二进制：

```
hapir/
├── src/main.rs              # CLI 入口
├── crates/
│   ├── hapir-shared/        # 协议与共享类型
│   ├── hapir-hub/           # Axum 服务端
│   ├── hapir-cli/           # CLI 客户端
│   ├── hapir-infra/         # 共享基础设施（WS 客户端、RPC、处理器）
│   ├── hapir-runner/        # 后台 Runner 进程
│   └── hapir-acp/           # Agent 协议后端（ACP、Codex App Server）
└── web/                     # React + Vite + TypeScript 前端
```

- **hapir-shared** — WebSocket 协议、领域类型、国际化
- **hapir-hub** — 中心服务器，包含 SyncEngine、SQLite 持久化、SSE、WebSocket 房间
- **hapir-cli** — 代理编排、本地/远程循环、会话管理
- **hapir-infra** — CLI 和 Runner 共用的基础设施：自动重连的 WebSocket 客户端、RPC 处理器（bash、文件、git、ripgrep 等）、持久化、认证
- **hapir-runner** — 后台 Runner 进程，负责机器注册、WebSocket 连接管理和 git worktree 管理
- **hapir-acp** — 独立的 Agent 协议后端：ACP SDK、Codex App Server、JSON-RPC 2.0 stdio 传输

前端是 React 19 + Vite + Tailwind CSS 的 PWA 应用。TypeScript 类型通过 `ts-rs` 从 Rust schema 自动生成。

## 前置要求

- Rust nightly（edition 2024，resolver 3）
- [Bun](https://bun.sh/)（用于前端）
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) >= 2.1.47（用于 Claude 代理）

## 构建

```bash
cargo build                  # Debug 构建
cargo build --release        # Release 构建
```

Release 构建时，前端会被编译并通过 `rust-embed` 嵌入到 hub 二进制文件中。

单独构建前端：

```bash
cd web && bun install && bun run build
```

## 使用

启动 hub 服务器：

```bash
hapir hub
```

运行 AI 代理会话（默认为 Claude）：

```bash
hapir              # Claude（默认）
hapir claude
hapir codex
hapir gemini
hapir opencode
```

其他命令：

```bash
hapir auth login   # 保存 API token
hapir auth status  # 查看当前配置
hapir runner start # 启动后台 runner
hapir mcp          # 运行 MCP stdio 桥接
hapir doctor       # 显示诊断信息
```

## 环境变量

**Hub：**

| 变量 | 默认值 | 说明 |
|---|---|---|
| `HAPIR_HOME` | `~/.hapir` | 数据目录 |
| `DB_PATH` | `{HAPIR_HOME}/hapir.db` | SQLite 数据库路径 |
| `HAPIR_LISTEN_HOST` | `127.0.0.1` | 监听地址 |
| `HAPIR_LISTEN_PORT` | `3006` | 监听端口 |
| `HAPIR_PUBLIC_URL` | — | 外部访问的公开 URL |
| `CORS_ORIGINS` | — | 逗号分隔的允许来源 |
| `TELEGRAM_BOT_TOKEN` | — | Telegram 机器人 token |
| `VAPID_SUBJECT` | — | Web Push 的 VAPID subject |

**CLI：**

| 变量 | 默认值 | 说明 |
|---|---|---|
| `HAPIR_API_URL` | `http://localhost:3006` | Hub 地址 |
| `CLI_API_TOKEN` | — | 认证 token |
| `HAPIR_HOME` | `~/.hapir` | 数据目录 |
| `HAPIR_EXPERIMENTAL` | — | 启用实验性功能 |

日志通过 `RUST_LOG` 环境变量控制（默认 `info`）。

## 致谢

本项目是 [hapi](https://github.com/tiann/hapi) 的衍生项目。感谢 [@tiann](https://github.com/tiann) 的开源贡献。

## 许可证

本项目使用 [AGPL-3.0](./LICENSE) 许可证，与原项目 [hapi](https://github.com/tiann/hapi) 一致。
