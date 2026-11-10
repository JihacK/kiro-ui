# Project Structure

```
kiro-ui/
├── .kiro/steering/              # Steering files (product, tech, structure, acp-protocol)
├── aidlc/                       # Planning & reference docs
├── Cargo.toml                   # Workspace (core, desktop, server) + release profile
├── crates/
│   ├── kiro-ui-core/src/
│   │   ├── lib.rs               # AppState: settings/trust persistence, workspace, shutdown
│   │   ├── hub.rs               # Client + Hub: tabs ↔ ACP sessions, shared agent routing, UI actions
│   │   ├── agent.rs             # Spawn kiro-cli / connect TCP, initialize, terminate
│   │   ├── rpc.rs               # ndjson JSON-RPC 2.0 connection
│   │   ├── notifications.rs     # session/update + _kiro.dev/* → UI events
│   │   ├── permissions.rs       # Trust + policy auto-resolution
│   │   ├── prompt.rs            # Prompt content blocks (text, images, files)
│   │   ├── suggestions.rs       # Bedrock follow-up suggestions, model list, test
│   │   └── picker.rs            # Native dialogs, URL opener, notifications (OS tools)
│   ├── kiro-ui-desktop/
│   │   ├── assets/icon-64.rgba  # Window icon (raw RGBA, avoids an image decoder)
│   │   └── src/
│   │       ├── main.rs          # PATH fix, single instance, runtime, eframe setup
│   │       ├── app.rs           # KiroApp: event drain, effects, user actions
│   │       ├── model.rs         # UI state + event handling (pure, unit-tested)
│   │       ├── view.rs          # Layout: header, sidebar, tabs, config bar, side panels, toasts, rewind
│   │       ├── chat.rs          # Messages, tool blocks, diffs, thinking, plan, permissions, panels
│   │       ├── input.rs         # Prompt box, slash popup, queue, goal, suggestions, context meter
│   │       ├── settings.rs      # Settings page
│   │       └── theme.rs         # Palette (dark/light) → egui visuals
│   └── kiro-ui-server/src/      # Optional browser mode: main.rs (axum), http.rs (routes, WS bridge)
├── src/client/                  # React web frontend (browser mode only)
├── packaging/linux/             # .desktop entry
├── scripts/install-linux.sh     # Build + per-user install
├── tests/                       # Vitest (web client) + Playwright e2e (browser mode)
└── package.json                 # npm scripts for both modes
```

## Architecture

```
egui window ──in-process──▶ kiro-ui-core::Client ──ACP──▶ kiro-cli acp   (desktop, default)
browser ──WebSocket──▶ kiro-ui-server ──▶ kiro-ui-core::Client ──ACP──▶ kiro-cli acp
```

- One `Client`/`Hub` per window (desktop) or per WebSocket (browser mode).
- Shared mode (default): all tabs of a client are sessions on one `kiro-cli` process. Per-tab mode: one process per tab.
- The UI ↔ core protocol is JSON actions/events, identical for both frontends.

## Conventions

- Rust: keep the core transport-agnostic (no axum/egui types in `kiro-ui-core`); Kiro payloads stay `serde_json::Value`
- Desktop UI: pure state changes in `model.rs` (return `Effect`s), rendering in the view modules; no per-frame clones of message lists
- Repaint only on events/input; schedule timed repaints only while something is live (running turn, thinking timer, toasts, debounces)
- Only use glyphs covered by the bundled egui fonts
- Theme: dark by default, persisted in eframe storage; settings in `~/.kiro-ui/settings.json`, trust in `~/.kiro-ui/trust.json`
- All Kiro interactions go through ACP, never direct file access to Kiro's data
- Web frontend (browser mode): React + TypeScript, CSS variables for theming, no external state library
