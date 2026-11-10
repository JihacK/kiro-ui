# Product Overview

Kiro UI is a lightweight native desktop chat interface for Kiro CLI (Rust + egui), connected via the Agent Client Protocol (ACP). An optional browser mode serves the original web frontend from the same Rust core.

## Purpose

Provide a visual alternative to the terminal for Kiro's agentic capabilities with minimal resource usage: one small native process, and by default one shared `kiro-cli` agent for all tabs.

## Target Users

- Developers who prefer a visual interface over terminal for AI-assisted coding
- Users on modest hardware who want the smallest possible footprint

## Key Features

- Real-time streaming chat with Markdown rendering and code highlighting
- Multi-tab sessions (tab = session), shared agent process by default
- Per-tab configuration: agent, model, permissions, effort level
- Tool call visualization with collapsible diffs and real-time shell streaming
- Permission prompts with auto-approve policies and trust persistence
- Thinking display with elapsed timer, agent plan, subagent pipeline
- Slash command autocomplete with dynamic subcommand options
- Conversation branching via /rewind with turn timeline
- Workspace-scoped session history with title filter
- Cancel current turn, message queue (reorder, merge, edit, delete, send now)
- Follow-up suggestions via Amazon Bedrock (optional)
- Goal iterations, context meter with compact, metering display
- MCP server panel with live status and OAuth links
- File follow-along panel with editor deep links
- File/image attachments (attach button, drag and drop)
- Chat export to Markdown
- Settings page, protocol debug panel
- Crash recovery with automatic agent restart
- Dark/light theme, keyboard shortcuts (Ctrl/⌘ T, N, L, B)

## Design Principles

- **Lightweight first** — single process, no webview, no local server for the desktop app; idle means no CPU
- **Protocol-first** — features driven by ACP capabilities; the UI adapts to what the agent supports
- **Streaming-native** — every interaction streams in real time
- **Dark by default** — developer-focused dark theme with optional light mode
- **ACP-only** — all Kiro interactions go through ACP, never direct file access

## Security

- Desktop: no network listeners; single-instance via a per-user Unix socket
- Browser mode: 127.0.0.1 only, per-start auth token, origin validation, CSP headers, DOMPurify on rendered Markdown
- External links limited to http(s) and editor schemes
- Rate limiting and resource limits (configurable)
