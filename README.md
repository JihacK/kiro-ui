# Kiro UI

A lightweight native chat interface for [Kiro CLI](https://kiro.dev/cli/), connected via the Agent Client Protocol (ACP). Written in Rust with [egui](https://github.com/emilk/egui): one small binary, no webview, no bundled browser or Node runtime.

<p align="center">
  <img src="docs/guide/tour.gif" alt="Kiro UI Demo" width="720">
</p>

## Why it's light

- **Native UI (egui)** — no Chromium/WebKit. The app is a single process that repaints only on events (≈0% CPU when idle).
- **No local server** — the UI talks to the Rust core in-process; nothing listens on a port.
- **One agent for all tabs** — by default every tab is a session on a single `kiro-cli acp` process (Settings → ACP Transport → *Share agent process*). Each extra `kiro-cli` costs hundreds of MB, because it starts its own MCP servers.

Measured on Linux (x86_64, one tab, `kiro-cli` excluded; PSS counts shared pages proportionally):

| | Processes | Memory (PSS) | Install size |
|---|---|---|---|
| Electron (previous) | 8 | 363 MB | 100 MB `.deb` |
| **egui (current)** | **1** | **63 MB** | **19 MB binary** |

With 3 tabs, sharing one agent used 1.0 GB PSS for the agent processes versus 1.7 GB with one agent per tab.

## Features

- **Real-time streaming** — Markdown rendering, code blocks with highlighting, unified diffs
- **Multi-tab sessions** — Per-tab agent, model, permission policy and effort level
- **Tool call visualization** — Collapsible blocks with grouping, raw I/O, clickable file paths, live shell output
- **Agent plan & subagent pipeline** — Live task list and pipeline status
- **File follow-along** — Side panel listing the files the agent touches
- **MCP server panel** — Status, tool lists, OAuth sign-in links
- **Permission management** — Auto-approve policies and persistent trust rules
- **Slash commands** — Autocomplete with dynamic subcommand options from the agent
- **Conversation branching** — `/rewind` with a turn timeline
- **Message queue** — Reorder, merge, edit, delete, send now
- **Follow-up suggestions** — Via Amazon Bedrock after each turn (optional)
- **Goal iterations** — `/goal` banner with progress
- **Context meter & metering** — Usage percentage, compact button, cumulative tokens and cost
- **Attachments** — Attach button or drag and drop files onto the window
- **Chat export** — Save the conversation as Markdown
- **Session history** — Workspace-scoped list with title filter
- **Settings page** — Workspace, editor links, transport, trust rules, suggestions, Kiro agent settings, limits
- **Protocol debug panel** — Raw JSON-RPC traffic
- **Crash recovery** — Dead agents are restarted automatically

## Quick start

Requirements: a Rust toolchain (stable) and [Kiro CLI](https://kiro.dev/cli/) installed and logged in (`kiro-cli` in `PATH`).

```bash
cargo run --release -p kiro-ui-desktop
```

Install for the current user on Linux (binary in `~/.local/bin`, plus desktop entry and icon):

```bash
sh scripts/install-linux.sh
```

## Browser mode (optional)

The original web frontend still works, served by a small Rust server that uses the same core:

```bash
npm install
npm start            # builds the frontend, serves http://127.0.0.1:3000
npm run dev          # Vite HMR on :5173 + server on :3000
```

The web server binds to 127.0.0.1 only, requires a per-start auth token for the API and WebSocket, checks WebSocket origins, and sends CSP headers.

## Layout

```
crates/kiro-ui-core      ACP client, tab/session hub, settings, trust, suggestions, dialogs
crates/kiro-ui-desktop   Native egui app (binary: kiro-ui)
crates/kiro-ui-server    Optional browser mode: HTTP + WebSocket over the core
src/client               Web frontend (React) for browser mode
```

```
egui window ──in-process──▶ kiro-ui-core ──ACP (ndjson over stdio/TCP)──▶ kiro-cli acp
browser ──WebSocket──▶ kiro-ui-server ──▶ kiro-ui-core ──▶ kiro-cli acp
```

## Scripts

| Command | Description |
|---|---|
| `npm run desktop` | Run the native app (debug build) |
| `npm run desktop:build` | Release build → `target/release/kiro-ui` |
| `npm run desktop:install` | Build and install for the current user (Linux) |
| `npm start` / `npm run dev` | Browser mode |
| `npm run test:rust` | Rust unit tests |
| `npm run lint:rust` | Clippy with warnings as errors |
| `npm test` / `npm run test:e2e` | Web frontend unit / Playwright tests |
| `npm run ci` | Full pipeline (JS + Rust) |

## Configuration

Stored in `~/.kiro-ui/`, shared by the desktop app and browser mode:

| File | Purpose |
|---|---|
| `settings.json` | Workspace, editor, transport, limits, suggestions |
| `trust.json` | Persistent tool permission rules |

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| Ctrl/⌘ + T, Ctrl/⌘ + N | New tab |
| Ctrl/⌘ + B | Toggle sidebar |
| Ctrl/⌘ + L | Clear messages |
| Escape | Cancel running turn |
| Enter / Shift+Enter | Send / new line |

## TCP transport (remote agent)

```bash
# On the remote machine:
socat TCP-LISTEN:9000,reuseaddr,fork EXEC:"kiro-cli acp"
# Then: Settings → ACP Transport → TCP → host:9000
```

## License

Apache 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
