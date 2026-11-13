# Kiro UI

A native chat window for [Kiro CLI](https://kiro.dev/cli/), connected through the Agent Client Protocol (ACP). It is written in Rust with [egui](https://github.com/emilk/egui) and ships as one binary: no webview, no bundled browser, no JavaScript runtime. The same binary can also serve the interface to a browser.

> Fork of [guyon-it-consulting/kiro-ui](https://github.com/guyon-it-consulting/kiro-ui) by Jerome Guyon (Guyon IT Consulting). This fork rewrites the app in Rust and replaces the React frontend with the egui interface, for the desktop and the browser alike. See [Credits](#credits).

<p align="center">
  <img src="docs/guide/tour.gif" alt="Kiro UI in use: tabs, streaming replies and tool calls" width="720">
</p>

## Why it uses so little

- The interface is drawn by egui, not by Chromium or WebKit. The app is a single process and repaints only when something happens, so it uses no CPU while idle.
- The desktop window talks to the Rust core inside the same process. There is no local web server. On Linux and macOS nothing listens on a network port; on Windows a localhost port is used only to detect a second launch.
- All tabs share one `kiro-cli acp` process by default (Settings → ACP Transport → Share agent process). Every extra `kiro-cli` starts its own MCP servers and costs hundreds of MB.

Measured on Linux (x86_64, one tab, `kiro-cli` excluded; PSS counts shared pages proportionally):

| | Processes | Memory (PSS) | Install size |
|---|---|---|---|
| Electron (previous) | 8 | 363 MB | 100 MB `.deb` |
| egui (current) | 1 | 63 MB | 19 MB binary |

With 3 tabs, one shared agent used 1.0 GB PSS for the agent processes, against 1.7 GB with one agent per tab.

## Features

- Streaming replies with Markdown, highlighted code blocks and unified diffs
- Tabs, each with its own agent, model, permission policy and effort level
- Tool calls in collapsible groups, with raw input and output, clickable file paths and live shell output
- The agent's plan and the subagent pipeline, updated as they run
- A side panel listing the files the agent reads and edits
- An MCP panel with server status, tool lists and OAuth sign-in links
- Auto-approve policies and saved trust rules for tool permissions
- Slash command autocomplete, with subcommand options supplied by the agent
- `/rewind` with a timeline of turns, to branch a conversation
- A message queue: reorder, merge, edit, delete or send a message now
- Follow-up suggestions from Amazon Bedrock after each turn (optional)
- `/goal` with a progress banner
- A context meter with a compact button, plus cumulative tokens and cost
- Attachments from the attach button or by dropping files on the window
- Export of a conversation to Markdown
- Session history per workspace, with a title filter; delete a saved session from the sidebar
- A settings page for the workspace, editor links, transport, trust rules, suggestions, Kiro agent settings and limits
- A debug panel showing the raw JSON-RPC traffic
- Automatic restart of an agent that crashes

## Build from source

### Prerequisites

| Tool | Needed for | Notes |
|---|---|---|
| [Rust](https://rustup.rs/) (stable) | Everything | `rustup` installs `cargo` and `rustc` |
| [Kiro CLI](https://kiro.dev/cli/) | Running the app | `kiro-cli` in `PATH`, logged in (`kiro-cli login`) |
| `wasm32-unknown-unknown` target and `wasm-bindgen-cli` 0.2.130 | Browser mode only | See [Browser mode](#browser-mode-optional) |
| `zenity` (Linux) | File and folder dialogs | macOS uses `osascript`, Windows uses PowerShell |

On Linux, compiling needs no `-dev` packages. Running the app needs an X11 or Wayland session with OpenGL (Mesa) and `libxkbcommon`, which desktop distributions install by default.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust
sudo apt install zenity                                           # Debian/Ubuntu dialogs
```

### Desktop app

```bash
git clone https://github.com/JihacK/kiro-ui.git
cd kiro-ui
cargo build --release -p kiro-ui
./target/release/kiro-ui
```

This produces one binary, `target/release/kiro-ui` (`kiro-ui.exe` on Windows). Start it from a terminal or double-click it. The first build compiles every dependency and takes a few minutes; later builds only recompile what changed. While developing, `cargo run` gives a faster debug build.

To install it for the current user on Linux, with the binary in `~/.local/bin` plus a desktop entry and icon (no root needed):

```bash
sh scripts/install-linux.sh                  # or: PREFIX=/some/prefix sh scripts/install-linux.sh
sh scripts/install-linux.sh --with-server    # the same, with browser mode built in
```

Linux is the tested platform. The code has macOS and Windows paths for dialogs and editor links, but CI does not build those yet.

### Tests and lint

```bash
cargo test --workspace                                   # unit tests
cargo clippy --workspace --all-targets -- -D warnings    # lint
cargo clippy -p kiro-ui --all-targets --features server -- -D warnings
cargo check -p kiro-ui-web --target wasm32-unknown-unknown
```

CI runs these commands. Build output goes to `target/`, which grows to several GB with debug builds; `cargo clean --profile dev` removes the debug files and keeps the release binary.

## Browser mode (optional)

Browser mode serves the same egui interface, compiled to WebAssembly and drawn on a canvas. It is built into the `kiro-ui` binary only when you enable the `server` feature, and it runs on the same core as the desktop app.

Set up the WebAssembly tools once:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked
```

Build and start it:

```bash
cargo xtask dist                                        # builds the wasm UI, then target/release/kiro-ui with browser mode
./target/release/kiro-ui --server                       # serves http://127.0.0.1:3000
./target/release/kiro-ui --server --port 8080 --open    # another port, and open the browser
```

The port comes from `--port`, then the `KIRO_UI_PORT` or `PORT` environment variable, then 3000. Without `--server`, the binary opens the desktop window as usual.

While working on the code:

```bash
cargo xtask web                                         # rebuild the wasm UI into dist/web
cargo run -p kiro-ui --features server -- --server
```

In our tests the server process used about 15 MB of memory while idle (`kiro-cli` excluded), and the browser downloads about 2.4 MB of compressed WebAssembly on first load.

How it differs from the desktop app:

- The interface is a canvas, so browsers get no screen-reader support. The desktop app keeps it through AccessKit.
- Folder and file pickers and the export dialog open on the machine running the server, not in the browser.
- The server listens on 127.0.0.1 only. Each start creates a new token, which the page passes to the WebSocket (`/ws`). The server also checks the Host and Origin headers and sends a Content Security Policy.

## Layout

```
crates/kiro-ui-core   ACP client, tab and session hub, settings, trust rules, suggestions, dialogs (native only)
crates/kiro-ui-app    The egui interface; builds for native and wasm and talks to a Backend (JSON in, JSON out)
crates/kiro-ui        The kiro-ui binary: the desktop window, plus --server with the `server` feature
crates/kiro-ui-web    Browser entry point (canvas and WebSocket backend)
xtask                 cargo xtask web | dist
```

```
egui window ──in-process──▶ kiro-ui-core ──ACP (ndjson over stdio/TCP)──▶ kiro-cli acp
browser (wasm UI) ──WebSocket /ws──▶ kiro-ui --server ──▶ kiro-ui-core ──▶ kiro-cli acp
```

## Commands

| Command | What it does |
|---|---|
| `cargo run` | Run the desktop app (debug build) |
| `cargo build --release -p kiro-ui` | Release build in `target/release/kiro-ui` |
| `sh scripts/install-linux.sh` | Build and install for the current user (Linux) |
| `cargo xtask web` | Build the wasm UI into `dist/web` |
| `cargo xtask dist` | Build the wasm UI and a release binary with browser mode |
| `cargo test --workspace` | Run the unit tests |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint |

## Configuration

Settings live in `~/.kiro-ui/` and are shared by the desktop app and browser mode:

| File | Contents |
|---|---|
| `settings.json` | Workspace, editor, transport, limits, suggestions |
| `trust.json` | Saved tool permission rules |

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| Ctrl/⌘ + T, Ctrl/⌘ + N | New tab |
| Ctrl/⌘ + B | Show or hide the sidebar |
| Ctrl/⌘ + L | Clear messages |
| Escape | Cancel the running turn |
| Enter / Shift+Enter | Send / new line |

## TCP transport (remote agent)

```bash
# On the remote machine:
socat TCP-LISTEN:9000,reuseaddr,fork EXEC:"kiro-cli acp"
# Then: Settings → ACP Transport → TCP → host:9000
```

## Credits

Jerome Guyon of [Guyon IT Consulting](https://github.com/guyon-it-consulting) created Kiro UI at [guyon-it-consulting/kiro-ui](https://github.com/guyon-it-consulting/kiro-ui). That project designed the interface and its features (multi-tab sessions, tool call visualization, permissions and trust rules, rewind, the message queue, follow-up suggestions, goals, the settings page) and built the original web frontend.

[JihacK](https://github.com/JihacK) maintains this fork, which rewrites the app in Rust: the ACP bridge (`kiro-ui-core`), the egui interface (`kiro-ui-app`), the `kiro-ui` binary, and browser mode (`kiro-ui-web` plus the `server` feature). Please report bugs in the Rust port here, not upstream.

## License

Apache 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). This is a modified version of the original work: this fork changed or added the Rust crates, the desktop app and the build setup.
