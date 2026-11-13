# Technology Stack

Primary goal: **lightweight and low resource usage.** Prefer fewer processes, fewer dependencies, and repainting/working only on events.

## Native desktop app (primary)

- Rust (stable, edition 2021), Cargo workspace; no Node.js anywhere
- egui / eframe 0.37 — immediate-mode native UI, `glow` (OpenGL) renderer, X11 + Wayland, AccessKit
  - `egui_commonmark` — Markdown rendering (default features off; built-in code highlighting)
  - `similar` — line diffs for tool-call diff views
  - Bundled monochrome emoji/icon fonts (`egui/monochrome_emoji_fonts`) — only use glyphs those fonts cover (check with `fc-query` before adding new symbols)
- tokio — 2 worker threads, runs `kiro-ui-core`
- No webview, no local HTTP server, no IPC: the UI talks to `kiro_ui_core::Client` in-process through a `Backend` trait (JSON actions in, JSON events out)
- The UI lives in `kiro-ui-app`, which compiles for native and wasm32; it must not depend on kiro-ui-core, tokio, `std::fs`, `std::process` or `std::time::Instant` (use `web_time::Instant`)
- Native dialogs via OS tools (zenity / osascript / PowerShell) in `kiro_ui_core::picker` — no GTK/toolkit dependency

## Core (`kiro-ui-core`)

- Hand-rolled ACP client: newline-delimited JSON-RPC 2.0 over stdio (spawned `kiro-cli acp`) or TCP, payloads as `serde_json::Value`
- `Client::start(state, emit)` / `Client::send(action)` — JSON actions in (`{"action": ...}`), JSON events out (`{"type": ...}`); same protocol for the native and the browser backend
- Shared agent process per client by default (`shareAgentProcess` setting); routing by `sessionId`, buffering while sessions open, subagent attribution via `_kiro.dev/subagent/list_update`
- Amazon Bedrock suggestions via `aws-config` + `aws-sdk-bedrockruntime` / `aws-sdk-bedrock` (clients created lazily)
- Settings/trust persisted as pretty JSON in `~/.kiro-ui/` (key order preserved)

## Browser mode (optional, feature `server`)

- The same egui UI compiled to wasm32 (`kiro-ui-web`, eframe `WebRunner` on a canvas, `WebBackend` over web-sys WebSocket), built by `cargo xtask web` into `dist/web` (wasm-bindgen CLI 0.2.130, `--target web`, gzip level 9; `wasm-opt -Oz` when found)
- `kiro-ui --server [--port N] [--open]` (binary built with `--features server`): axum 0.8 (HTTP/1 + WebSocket), rust-embed serves `dist/web` (`.gz` files served with `Content-Encoding: gzip`, ETag, `no-cache`)
- `GET /` returns `index.html` with the per-start token in `<meta name="kiro-token">`; `GET /ws?token=...` upgrades to a WebSocket carrying JSON text frames, one `kiro_ui_core::Client` per socket
- Browser limits: canvas UI (no screen-reader support), dialogs open on the server machine

## Security

- Desktop: no listening sockets except a per-user Unix socket (`~/.kiro-ui/desktop.sock`) for single-instance focus
- Browser mode: binds 127.0.0.1 only, per-start token (constant-time compare), `Host` must be localhost or 127.0.0.1 (DNS-rebinding guard), WebSocket `Origin` check, max message 64 MiB, CSP (`script-src 'self' 'wasm-unsafe-eval'`, `frame-ancestors 'none'`), nosniff, no-referrer
- Rate limiting and max tabs (configurable), path whitelist for dialog start paths, external URL scheme whitelist (http, https, vscode, cursor, idea; `Backend::open_url` is only called for allowed URLs)

## Build & test

- `cargo run` / `cargo build --release -p kiro-ui` (binary `kiro-ui`)
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
- Release profile: `opt-level = "s"`, LTO, 1 codegen unit, stripped
- Browser mode: one-time `rustup target add wasm32-unknown-unknown` and `cargo install wasm-bindgen-cli --version 0.2.130 --locked`
- `cargo xtask web` builds the wasm UI into `dist/web`; `cargo xtask dist` also builds `cargo build --release -p kiro-ui --features server`
- Dev loop for browser mode: `cargo xtask web`, then `cargo run -p kiro-ui --features server -- --server`
- The server embeds `dist/web` at compile time; a build script writes a placeholder `index.html` when it is missing, so the crate always compiles
- Also checked in CI: `cargo clippy -p kiro-ui --all-targets --features server -- -D warnings`, `cargo check -p kiro-ui-web --target wasm32-unknown-unknown`

## Protocol

- ACP via the hand-rolled client in `kiro-ui-core/src/rpc.rs`
- Kiro extensions (`_kiro.dev/*`) translated to UI events in `kiro-ui-core/src/notifications.rs`
- Permission requests answered from trust rules / tab policy, otherwise forwarded to the UI and resolved by `permission_response`

## Local actions

Handled by the core immediately: not rate limited, no wait for the first tab's session, no live agent needed.

| Action | Reply event |
|---|---|
| `get_settings`, `merge_settings {patch}`, `set_trust_rules {rules}` | `Settings {settings, trust, workspace}` |
| `suggest {tabId, context}` | `Suggestions {tabId, suggestions, error?}` |
| `suggestion_models {patch?}` | `SuggestionModels {models, error?}` |
| `suggestion_test` | `SuggestionTest {ok, model?, error?}` |
| `pick_folder {tabId, start}` | `FolderPicked {tabId, path}` |
| `pick_files {tabId, start}`, `attach_paths {tabId, paths}` | `FilesAttached {tabId, items, errors}` |
| `export {markdown, fileName, start?}` | `Exported {path?, error?, cancelled?}` |
| `notify {body}` | none |

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
