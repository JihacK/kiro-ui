# Technology Stack

Primary goal: **lightweight and low resource usage.** Prefer fewer processes, fewer dependencies, and repainting/working only on events.

## Native desktop app (primary)

- Rust (stable, edition 2021), Cargo workspace
- egui / eframe 0.37 — immediate-mode native UI, `glow` (OpenGL) renderer, X11 + Wayland, AccessKit
  - `egui_commonmark` — Markdown rendering (default features off; built-in code highlighting)
  - `similar` — line diffs for tool-call diff views
  - Bundled monochrome emoji/icon fonts (`egui/monochrome_emoji_fonts`) — only use glyphs those fonts cover (check with `fc-query` before adding new symbols)
- tokio — 2 worker threads, runs `kiro-ui-core`
- No webview, no local HTTP server, no IPC: the UI calls `kiro_ui_core::Client` in-process
- Native dialogs via OS tools (zenity / osascript / PowerShell) in `kiro_ui_core::picker` — no GTK/toolkit dependency

## Core (`kiro-ui-core`)

- Hand-rolled ACP client: newline-delimited JSON-RPC 2.0 over stdio (spawned `kiro-cli acp`) or TCP, payloads as `serde_json::Value`
- `Client::start(state, emit)` / `Client::send(action)` — JSON actions in (`{"action": ...}`), JSON events out (`{"type": ...}`); same protocol the web frontend uses
- Shared agent process per client by default (`shareAgentProcess` setting); routing by `sessionId`, buffering while sessions open, subagent attribution via `_kiro.dev/subagent/list_update`
- Amazon Bedrock suggestions via `aws-config` + `aws-sdk-bedrockruntime` / `aws-sdk-bedrock` (clients created lazily)
- Settings/trust persisted as pretty JSON in `~/.kiro-ui/` (key order preserved)

## Browser mode (optional, `kiro-ui-server`)

- axum 0.8 (HTTP/1 + WebSocket), rust-embed serves `dist/client`
- React 19 + TypeScript frontend built with Vite 6 (marked + DOMPurify, highlight.js, react-diff-viewer-continued)

## Security

- Desktop: no listening sockets except a per-user Unix socket (`~/.kiro-ui/desktop.sock`) for single-instance focus
- Browser mode: binds 127.0.0.1 only, auth token per start (constant-time compare), WebSocket origin check, CSP headers
- Rate limiting and max tabs (configurable), path whitelist for dialog start paths, external URL scheme whitelist (http, https, vscode, cursor, idea)

## Build & test

- `cargo run -p kiro-ui-desktop` / `cargo build --release -p kiro-ui-desktop` (binary `kiro-ui`)
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
- Release profile: `opt-level = "s"`, LTO, 1 codegen unit, stripped
- Browser mode: `npm start` (Vite build + server), `npm run dev` (Vite :5173 + server :3000)
- The server crate embeds `dist/client` at compile time: run `npm run build` before building it

## Protocol

- ACP via the hand-rolled client in `kiro-ui-core/src/rpc.rs`
- Kiro extensions (`_kiro.dev/*`) translated to UI events in `kiro-ui-core/src/notifications.rs`
- Permission requests answered from trust rules / tab policy, otherwise forwarded to the UI and resolved by `permission_response`

## Kiro Extension Methods

All handled:
- `_kiro.dev/commands/available` — Slash commands, MCP servers, tools list
- `_kiro.dev/metadata` — Context usage percentage, turn duration
- `_kiro.dev/commands/options` — Dynamic option lists for slash commands
- `_kiro.dev/session/terminate` — Graceful session close
- `_kiro.dev/session/list` — List sessions with metadata
- `_kiro.dev/settings/list` — Fetch configurable settings
- `_kiro.dev/settings/set` — Update a setting
- `_kiro.dev/mcp/server_initialized` — MCP server online
- `_kiro.dev/mcp/server_init_failure` — MCP server failure
- `_kiro.dev/mcp/oauth_request` — OAuth URL for MCP auth
- `_kiro.dev/mcp/governance_disabled` — MCP admin-disabled
- `_kiro.dev/compaction/status` — Context compaction progress
- `_kiro.dev/clear/status` — Conversation clear acknowledgement
- `_kiro.dev/agent/switched` — Agent change (name, model, welcome)
- `_kiro.dev/agent/not_found` — Agent not found with fallback
- `_kiro.dev/agent/config_error` — Agent config parse error
- `_kiro.dev/error/rate_limit` — Rate limit notification
- `_kiro.dev/subagent/list_update` — Subagent session tracking
- `_kiro.dev/session/activity` — Multi-session activity events
- `_kiro.dev/session/list_update` — Dynamic session list changes
- `_kiro.dev/session/inbox_notification` — Messages from orchestrated sessions
- `_kiro.dev/session/update` — Extended updates (tool_call_chunk, retry_warning)
- `_kiro.dev/goal/status` — Goal iteration progress
