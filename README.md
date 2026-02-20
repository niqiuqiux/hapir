# HAPIR

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/Shirasuki/hapir)

App for Claude Code / Codex / Gemini / OpenCode, vibe coding anytime, anywhere.

Run AI coding sessions and control them remotely through Web / PWA / Telegram Mini App.

> **Note**: This project is heavily based on [hapi](https://github.com/tiann/hapi) by [@tiann](https://github.com/tiann). Most of the codebase originates from hapi, with modifications to certain features. Configurations and protocols are **not compatible** with the original project.

[中文文档](./README_CN.md)

## Features

- **Multi-agent support** — Claude Code, Codex, Gemini, OpenCode
- **Remote control** — Web UI, PWA, Telegram Mini App
- **Seamless switching** — Switch between local and remote sessions freely
- **Permission management** — Approve or deny AI requests from your phone
- **Terminal access** — Remote PTY via WebSocket
- **File browser** — Browse files with git status and diff
- **Voice assistant** — ElevenLabs integration
- **Push notifications** — Web Push and Telegram bot
- **Offline support** — PWA with offline message buffering

## Architecture

Rust workspace with three crates:

```
hapir/
├── src/main.rs              # CLI entry point
├── crates/
│   ├── hapir-shared/        # Protocol & shared types
│   ├── hapir-hub/           # Axum server
│   └── hapir-cli/           # CLI client
└── web/                     # React + Vite + TypeScript frontend
```

- **hapir-shared** — WebSocket protocol, domain types, i18n
- **hapir-hub** — Central server with SyncEngine, SQLite persistence, SSE, WebSocket rooms
- **hapir-cli** — Agent orchestration, local/remote loop, RPC handlers

The frontend is a React 19 + Vite + Tailwind CSS PWA. TypeScript types are auto-generated from Rust schemas via `ts-rs`.

## Prerequisites

- Rust nightly (edition 2024, resolver 3)
- [Bun](https://bun.sh/) (for frontend)

## Build

```bash
cargo build                  # Debug build
cargo build --release        # Release build
```

In release builds, the frontend is compiled and embedded into the hub binary via `rust-embed`.

To build the frontend separately:

```bash
cd web && bun install && bun run build
```

## Usage

Start the hub server:

```bash
hapi hub
```

Run an AI agent session (Claude is the default):

```bash
hapi              # Claude (default)
hapi claude
hapi codex
hapi gemini
hapi opencode
```

Other commands:

```bash
hapi auth login   # Save API token
hapi auth status  # Show current config
hapi runner start # Start background runner
hapi doctor       # Show diagnostics
```

## Environment Variables

**Hub:**

| Variable | Default | Description |
|---|---|---|
| `HAPIR_HOME` | `~/.hapir` | Data directory |
| `DB_PATH` | `{HAPIR_HOME}/hapir.db` | SQLite database path |
| `HAPIR_LISTEN_HOST` | `127.0.0.1` | Listen address |
| `HAPIR_LISTEN_PORT` | `3006` | Listen port |
| `HAPIR_PUBLIC_URL` | — | Public URL for external access |
| `CORS_ORIGINS` | — | Comma-separated allowed origins |
| `TELEGRAM_BOT_TOKEN` | — | Telegram bot token |
| `VAPID_SUBJECT` | — | VAPID subject for web push |

**CLI:**

| Variable | Default | Description |
|---|---|---|
| `HAPIR_API_URL` | `http://localhost:3006` | Hub URL |
| `CLI_API_TOKEN` | — | Authentication token |
| `HAPIR_HOME` | `~/.hapir` | Data directory |
| `HAPIR_EXPERIMENTAL` | — | Enable experimental features |

Logging is controlled via `RUST_LOG` (defaults to `info`).

## Acknowledgments

This project is a derivative of [hapi](https://github.com/tiann/hapi). Thanks to [@tiann](https://github.com/tiann) for the original work.

## License

This project is licensed under [AGPL-3.0](./LICENSE), same as the original [hapi](https://github.com/tiann/hapi).
