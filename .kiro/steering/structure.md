# Project Structure

```
kiro-ui/
├── .kiro/steering/              # Steering files (product, tech, structure, acp-protocol)
├── aidlc/                       # Planning & reference docs
├── Cargo.toml                   # Workspace (core, app, kiro-ui, web, xtask) + release profile
├── crates/
│   ├── kiro-ui-core/src/        # Native only, transport-agnostic
│   │   ├── lib.rs               # AppState: settings/trust persistence, workspace, shutdown
│   │   ├── hub.rs               # Client + Hub: tabs ↔ ACP sessions, shared agent routing, UI actions
│   │   ├── agent.rs             # Spawn kiro-cli / connect TCP, initialize, terminate
│   │   ├── rpc.rs               # ndjson JSON-RPC 2.0 connection
│   │   ├── notifications.rs     # session/update + _kiro.dev/* → UI events
│   │   ├── permissions.rs       # Trust + policy auto-resolution
│   │   ├── prompt.rs            # Prompt content blocks (text, images, files)
│   │   ├── suggestions.rs       # Bedrock follow-up suggestions, model list, test
│   │   └── picker.rs            # Native dialogs, URL opener, notifications (OS tools)
│   ├── kiro-ui-app/src/         # The egui UI; compiles for native and wasm32
│   │   ├── backend.rs           # Backend trait (send/poll/open_url/shutdown), allowed_url
│   │   ├── app.rs               # KiroApp: event drain, effects, user actions
│   │   ├── model.rs             # UI state + event handling (pure, unit-tested)
│   │   ├── view.rs              # Layout: header, sidebar, tabs, config bar, side panels, toasts, rewind
│   │   ├── chat.rs              # Messages, tool blocks, diffs, thinking, plan, permissions, panels
│   │   ├── input.rs             # Prompt box, slash popup, queue, goal, suggestions, context meter
│   │   ├── settings.rs          # Settings page
│   │   └── theme.rs             # Palette (dark/light) → egui visuals
│   ├── kiro-ui/                 # Binary `kiro-ui`: main.rs (PATH fix, single instance, runtime, eframe,
│   │                            #   NativeBackend over kiro_ui_core::Client); server.rs (feature `server`)
│   └── kiro-ui-web/             # cdylib for wasm32: canvas, WebRunner, WebBackend (WebSocket); static/ loader
├── xtask/                       # cargo xtask web | dist
├── dist/web/                    # Built wasm UI (git-ignored), embedded by the server feature
├── packaging/linux/             # .desktop entry
├── scripts/install-linux.sh     # Build + per-user install
└── .github/workflows/ci.yml     # Rust-only CI
```

## Architecture

```
egui window ──Backend (in-process)──▶ kiro-ui-core::Client ──ACP──▶ kiro-cli acp   (desktop, default)
wasm UI in browser ──WebSocket /ws──▶ kiro-ui --server ──▶ kiro-ui-core::Client ──ACP──▶ kiro-cli acp
```

- One `Client`/`Hub` per window (desktop) or per WebSocket (browser mode).
- Shared mode (default): all tabs of a client are sessions on one `kiro-cli` process. Per-tab mode: one process per tab.
- The UI ↔ core protocol is JSON actions/events, identical for both backends. `kiro-ui-app` only sees the `Backend` trait.

## Conventions

- Rust: keep the core transport-agnostic (no axum/egui types in `kiro-ui-core`); Kiro payloads stay `serde_json::Value`
- `kiro-ui-app` must stay wasm-clean: no kiro-ui-core, tokio, `std::fs`, `std::process`, `std::time::Instant` (use `web_time`); everything external goes through `Backend`
- Desktop UI: pure state changes in `model.rs` (return `Effect`s), rendering in the view modules; no per-frame clones of message lists
- Repaint only on events/input; schedule timed repaints only while something is live (running turn, thinking timer, toasts, debounces)
- Only use glyphs covered by the bundled egui fonts
- Theme: dark by default, persisted in eframe storage; settings in `~/.kiro-ui/settings.json`, trust in `~/.kiro-ui/trust.json`
- All Kiro interactions go through ACP, never direct file access to Kiro's data
- Browser mode reuses the egui UI; there is no separate web frontend
