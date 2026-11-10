//! Minimal newline-delimited JSON-RPC 2.0 peer, as used by ACP over stdio/TCP.
//!
//! Payloads stay as `serde_json::Value`: the bridge only routes messages, and
//! Kiro's `_kiro.dev/*` extensions are untyped anyway.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: Option<i64>,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn local(msg: impl Into<String>) -> Self {
        RpcError { code: None, message: msg.into(), data: None }
    }
    fn from_value(v: &Value) -> Self {
        RpcError {
            code: v.get("code").and_then(Value::as_i64),
            message: v.get("message").and_then(Value::as_str).unwrap_or("Unknown error").to_string(),
            data: v.get("data").filter(|d| !d.is_null()).cloned(),
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A message initiated by the agent.
#[derive(Debug, Clone)]
pub enum Incoming {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    In,
    Out,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::In => "in",
            Dir::Out => "out",
        }
    }
}

/// Events delivered to the owner of a connection, tagged with the connection id.
#[derive(Debug)]
pub enum ConnEvent {
    Message(u64, Incoming),
    /// Raw traffic, only emitted while debug logging is enabled.
    Raw(u64, Dir, String),
    /// Read side reached EOF or failed.
    Closed(u64),
    /// Agent child process exited (process transport only).
    Exited(u64, Option<i32>),
    /// Agent stderr reported an authentication problem.
    AuthError(u64),
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>;

pub struct RpcConn {
    out: mpsc::UnboundedSender<String>,
    pending: Arc<Pending>,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    tasks: Mutex<Vec<AbortHandle>>,
}

impl RpcConn {
    pub fn start<R, W>(
        id: u64,
        reader: R,
        writer: W,
        events: mpsc::UnboundedSender<ConnEvent>,
        debug: Arc<AtomicBool>,
    ) -> Arc<RpcConn>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Pending> = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));

        let w_events = events.clone();
        let w_debug = debug.clone();
        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(line) = out_rx.recv().await {
                if w_debug.load(Ordering::Relaxed) {
                    let _ = w_events.send(ConnEvent::Raw(id, Dir::Out, line.clone()));
                }
                let mut bytes = line.into_bytes();
                bytes.push(b'\n');
                if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        });

        let r_pending = pending.clone();
        let r_closed = closed.clone();
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::with_capacity(64 * 1024, reader);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if debug.load(Ordering::Relaxed) {
                    let _ = events.send(ConnEvent::Raw(id, Dir::In, line.to_string()));
                }
                let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
                route_incoming(id, msg, &r_pending, &events);
            }
            r_closed.store(true, Ordering::SeqCst);
            fail_all(&r_pending);
            let _ = events.send(ConnEvent::Closed(id));
        });

        Arc::new(RpcConn {
            out: out_tx,
            pending,
            next_id: AtomicU64::new(1),
            closed,
            tasks: Mutex::new(vec![writer_task.abort_handle(), reader_task.abort_handle()]),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn send(&self, msg: &Value) -> bool {
        self.out.send(msg.to_string()).is_ok()
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if self.is_closed() {
            return Err(RpcError::local("ACP connection closed"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        // Re-check after registering, so a concurrent close can't strand the waiter.
        if self.is_closed() || !self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})) {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError::local("ACP connection closed"));
        }
        rx.await.unwrap_or_else(|_| Err(RpcError::local("ACP connection closed")))
    }

    /// Like `request`, but gives up (and forgets the waiter) after `timeout`.
    pub async fn request_timeout(&self, method: &str, params: Value, timeout: std::time::Duration) -> Result<Value, RpcError> {
        match tokio::time::timeout(timeout, self.request(method, params)).await {
            Ok(r) => r,
            Err(_) => {
                // The timed-out future dropped its receiver; forget such stale waiters.
                self.pending.lock().unwrap().retain(|_, tx| !tx.is_closed());
                Err(RpcError::local("ACP request timed out"))
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn respond(&self, id: Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str, data: Option<Value>) {
        let mut err = json!({"code": code, "message": message});
        if let Some(d) = data {
            err["data"] = d;
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": err}));
    }

    /// Stop both I/O tasks (closes a TCP socket; stdio pipes close with the child).
    pub fn close(&self) {
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        if !self.closed.swap(true, Ordering::SeqCst) {
            fail_all(&self.pending);
        }
    }
}

fn fail_all(pending: &Pending) {
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(RpcError::local("ACP connection closed")));
    }
}

fn route_incoming(conn_id: u64, mut msg: Value, pending: &Pending, events: &mpsc::UnboundedSender<ConnEvent>) {
    let id = msg.get("id").filter(|v| !v.is_null()).cloned();
    match msg.get("method").and_then(Value::as_str).map(str::to_owned) {
        Some(method) => {
            let params = msg.get_mut("params").map(Value::take).unwrap_or(Value::Null);
            let incoming = match id {
                Some(id) => Incoming::Request { id, method, params },
                None => Incoming::Notification { method, params },
            };
            let _ = events.send(ConnEvent::Message(conn_id, incoming));
        }
        None => {
            let Some(num) = id.as_ref().and_then(Value::as_u64) else { return };
            let Some(tx) = pending.lock().unwrap().remove(&num) else { return };
            let result = match msg.get("error").filter(|e| !e.is_null()) {
                Some(err) => Err(RpcError::from_value(err)),
                None => Ok(msg.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn request_response_and_incoming_routing() {
        let (client_io, agent_io) = duplex(64 * 1024);
        let (cr, cw) = tokio::io::split(client_io);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let conn = RpcConn::start(7, cr, cw, ev_tx, Arc::new(AtomicBool::new(false)));

        let (ar, mut aw) = tokio::io::split(agent_io);
        let agent = tokio::spawn(async move {
            let mut lines = BufReader::new(ar).lines();
            let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(req["method"], "session/new");
            let id = req["id"].clone();
            // Notification and agent->client request arrive before the response.
            aw.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"s\"}}\n").await.unwrap();
            aw.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"p1\",\"method\":\"session/request_permission\",\"params\":{}}\n").await.unwrap();
            aw.write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"sessionId\":\"s\"}}}}\n").as_bytes()).await.unwrap();
            let req2: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            aw.write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":-32601,\"message\":\"nope\"}}}}\n", req2["id"]).as_bytes()).await.unwrap();
        });

        let res = conn.request("session/new", json!({})).await.unwrap();
        assert_eq!(res["sessionId"], "s");
        let err = conn.request("bad", json!({})).await.unwrap_err();
        assert_eq!(err.code, Some(-32601));
        assert_eq!(err.message, "nope");
        agent.await.unwrap();

        match ev_rx.recv().await.unwrap() {
            ConnEvent::Message(7, Incoming::Notification { method, .. }) => assert_eq!(method, "session/update"),
            e => panic!("unexpected {e:?}"),
        }
        match ev_rx.recv().await.unwrap() {
            ConnEvent::Message(7, Incoming::Request { id, method, .. }) => {
                assert_eq!(id, json!("p1"));
                assert_eq!(method, "session/request_permission");
            }
            e => panic!("unexpected {e:?}"),
        }
        assert!(matches!(ev_rx.recv().await.unwrap(), ConnEvent::Closed(7)));
        assert!(conn.request("x", json!({})).await.is_err());
    }
}
