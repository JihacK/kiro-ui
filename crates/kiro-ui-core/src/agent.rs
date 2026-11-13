//! Agent transport: spawn `kiro-cli acp` (or connect over TCP), run the ACP
//! `initialize` handshake, and terminate gracefully.

use crate::rpc::{ConnEvent, RpcConn, RpcError};
use crate::AppState;
use regex::Regex;
use serde_json::json;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

pub const PROTOCOL_VERSION: u64 = 1;

static NEXT_AGENT_ID: AtomicU64 = AtomicU64::new(1);
static AUTH_ERR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)not authenticated|not logged in|login required|unauthorized|auth.*fail").unwrap()
});

#[derive(Clone, Copy)]
enum Sig {
    Term,
    Kill,
}

pub struct Agent {
    pub id: u64,
    pub conn: Arc<RpcConn>,
    /// Shared by all tabs of one client (vs. owned by one tab).
    pub shared: bool,
    /// For a per-tab agent, the tab that owns it.
    pub owner_tab: Option<String>,
    pub is_process: bool,
    pub auth_error: AtomicBool,
    exited: Arc<AtomicBool>,
    kill_tx: Option<mpsc::UnboundedSender<Sig>>,
    terminating: Mutex<bool>,
}

impl Agent {
    pub fn is_dead(&self) -> bool {
        if self.is_process {
            self.exited.load(Ordering::SeqCst)
        } else {
            self.conn.is_closed()
        }
    }

    pub fn mark_auth_error(&self) -> bool {
        !self.auth_error.swap(true, Ordering::SeqCst)
    }

    /// SIGTERM, then SIGKILL if still alive after 3s. TCP transports just close the socket.
    pub async fn terminate(&self) {
        {
            let mut t = self.terminating.lock().unwrap();
            if *t {
                return;
            }
            *t = true;
        }
        let Some(kill) = &self.kill_tx else {
            self.conn.close();
            return;
        };
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        let _ = kill.send(Sig::Term);
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if self.exited.load(Ordering::SeqCst) {
                return;
            }
        }
        let _ = kill.send(Sig::Kill);
    }

    /// Fire-and-forget teardown used when a tab closes (after a short grace period).
    pub fn terminate_later(self: &Arc<Self>, delay: Duration) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            me.terminate().await;
        });
    }
}

/// Spawn/connect an agent according to current settings and complete `initialize`.
pub async fn spawn(
    app: &Arc<AppState>,
    events: mpsc::UnboundedSender<ConnEvent>,
    debug: Arc<AtomicBool>,
    shared: bool,
    owner_tab: Option<String>,
) -> Result<Arc<Agent>, RpcError> {
    let id = NEXT_AGENT_ID.fetch_add(1, Ordering::Relaxed);
    let exited = Arc::new(AtomicBool::new(false));

    let (conn, kill_tx, is_process) = if app.setting("transport").as_deref() == Some("tcp") {
        let host = app.setting("tcpHost").unwrap_or_else(|| "localhost".into());
        let port: u16 = app.setting("tcpPort").and_then(|p| p.trim().parse().ok()).unwrap_or(9000);
        let stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| RpcError::local(format!("connect {host}:{port}: {e}")))?;
        let _ = stream.set_nodelay(true);
        let (r, w) = stream.into_split();
        (RpcConn::start(id, r, w, events.clone(), debug), None, false)
    } else {
        let command = app.setting("acpCommand").unwrap_or_else(|| "kiro-cli acp".into());
        let mut parts = command.split_whitespace();
        let program = parts.next().ok_or_else(|| RpcError::local("ACP command is empty"))?;
        let mut cmd = Command::new(program);
        cmd.args(parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Only meaningful for Node-based agents; harmless otherwise.
            .env("NODE_OPTIONS", format!("--max-old-space-size={}", app.max_child_mem_mb()))
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| RpcError::local(format!("spawn {program}: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let err_events = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if AUTH_ERR_RE.is_match(&line) {
                    let _ = err_events.send(ConnEvent::AuthError(id));
                }
            }
        });

        let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<Sig>();
        let exit_flag = exited.clone();
        let exit_events = events.clone();
        tokio::spawn(async move {
            let status = loop {
                tokio::select! {
                    status = child.wait() => break status,
                    Some(sig) = kill_rx.recv() => send_signal(&mut child, sig),
                }
            };
            exit_flag.store(true, Ordering::SeqCst);
            let _ = exit_events.send(ConnEvent::Exited(id, status.ok().and_then(|s| s.code())));
        });

        (RpcConn::start(id, stdout, stdin, events, debug), Some(kill_tx), true)
    };

    let agent = Arc::new(Agent {
        id,
        conn,
        shared,
        owner_tab,
        is_process,
        auth_error: AtomicBool::new(false),
        exited,
        kill_tx,
        terminating: Mutex::new(false),
    });
    app.register_agent(&agent);
    Ok(agent)
}

impl Agent {
    /// ACP `initialize` handshake. Terminates the agent on failure.
    pub async fn initialize(self: &Arc<Self>) -> Result<(), RpcError> {
        let init = self
            .conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": true }, "terminal": true },
                    "clientInfo": { "name": "kiro-ui", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await;
        if init.is_err() {
            self.terminate_later(Duration::ZERO);
        }
        init.map(|_| ())
    }
}

/// Signature of the settings that determine how an agent is launched.
pub fn transport_signature(app: &AppState) -> String {
    ["transport", "acpCommand", "tcpHost", "tcpPort", "maxChildMemMb"]
        .iter()
        .map(|k| app.setting(k).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn send_signal(child: &mut tokio::process::Child, sig: Sig) {
    #[cfg(unix)]
    if let (Sig::Term, Some(pid)) = (sig, child.id()) {
        // SAFETY: plain kill(2) on our own child's pid.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        return;
    }
    let _ = sig;
    let _ = child.start_kill();
}

/// Kiro session ids are UUIDs; anything else is refused before it reaches a command line.
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The Kiro CLI binary: the configured ACP command's program when it is kiro-cli, else `kiro-cli` on PATH.
fn kiro_cli_program(app: &AppState) -> String {
    let configured = (app.setting("transport").as_deref() != Some("tcp"))
        .then(|| app.setting("acpCommand"))
        .flatten()
        .and_then(|c| c.split_whitespace().next().map(str::to_owned))
        .filter(|p| std::path::Path::new(p).file_stem().is_some_and(|s| s.to_string_lossy().starts_with("kiro-cli")));
    configured.unwrap_or_else(|| "kiro-cli".into())
}

/// First line of the CLI's message, without colour codes or the `Error: ` prefix.
fn cli_message(out: &std::process::Output) -> String {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    let text = ANSI.replace_all(&text, "");
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("unknown error");
    let line = line.strip_prefix("Error: ").unwrap_or(line);
    line.chars().take(300).collect()
}

/// Delete a saved session with `kiro-cli chat --delete-session` (ACP has no delete method).
/// Kiro refuses while any process still has the session loaded; that error is returned as-is.
pub async fn delete_session(app: &AppState, session_id: &str) -> Result<(), String> {
    if !valid_session_id(session_id) {
        return Err("invalid session id".into());
    }
    let program = kiro_cli_program(app);
    let mut cmd = Command::new(&program);
    cmd.args(["chat", "--delete-session", session_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| format!("{program} did not finish within 30s"))?
        .map_err(|e| format!("spawn {program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(cli_message(&out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_that_could_be_flags_or_paths_are_refused() {
        assert!(valid_session_id("5b8d5c8f-9d07-46e7-85ed-19108c580b02"));
        for bad in ["", "-d", "--help", "../x", "a b", "a;rm", "a/b", &"a".repeat(129)] {
            assert!(!valid_session_id(bad), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_message_is_the_first_line_without_colour_or_prefix() {
        use std::os::unix::process::ExitStatusExt;
        let out = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: b"\x1b[31mError: \x1b[0mFailed to delete chat session x: Session is active in another process (PID 1)\nmore\n".to_vec(),
        };
        assert_eq!(cli_message(&out), "Failed to delete chat session x: Session is active in another process (PID 1)");
    }
}
