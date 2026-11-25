//! Terminals driven in process through the client API, without the host's
//! socket: a real shell in a real pseudo-terminal.

mod common;

use agenthost_host::api::{serve_connection, Principal};
use agenthost_host::Host;
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods::{self as m, Capability};
use agenthost_proto::*;
use base64::Engine as _;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

/// A host that isn't serving, whose terminals run plain `sh` (the same in
/// every test, so setting it from several is harmless).
fn open(root: &std::path::Path) -> Arc<Host> {
    std::env::set_var("AGENTHOST_TERMINAL_SHELL", "/bin/sh");
    Host::open(common::test_config(root)).unwrap()
}

/// One client connection, as the socket or a tunnel would make it.
struct Client {
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<String>,
    next: u64,
    /// Notifications read while waiting for a response.
    notes: Vec<Notification>,
    /// Bytes of output that arrived marked as a replay.
    replayed: u64,
}

impl Client {
    async fn connect(host: &Arc<Host>, principal: Principal) -> Client {
        let (tx, in_rx) = mpsc::channel(256);
        let (out_tx, rx) = mpsc::channel(1024);
        tokio::spawn(serve_connection(host.clone(), principal, in_rx, out_tx));
        let mut c = Client { tx, rx, next: 0, notes: vec![], replayed: 0 };
        c.call(m::HELLO, json!({"protocol_version": PROTOCOL_VERSION, "client_name": "test", "client_version": "0"})).await.unwrap();
        c
    }

    async fn local(host: &Arc<Host>) -> Client {
        Self::connect(host, Principal::local()).await
    }

    async fn recv(&mut self) -> Message {
        let line = tokio::time::timeout(Duration::from_secs(20), self.rx.recv()).await.expect("the host went quiet").expect("the connection closed");
        Message::parse(&line).unwrap()
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.next += 1;
        let id = json!(self.next);
        self.tx.send(Message::Request(Request::new(id.clone(), method, params)).to_json()).await.unwrap();
        loop {
            match self.recv().await {
                Message::Response(r) if r.id == id => return match r.error {
                    Some(e) => Err(e),
                    None => Ok(r.result.unwrap_or(Value::Null)),
                },
                Message::Notification(n) => self.notes.push(n),
                _ => {}
            }
        }
    }

    async fn note(&mut self) -> Notification {
        if !self.notes.is_empty() {
            return self.notes.remove(0);
        }
        loop {
            if let Message::Notification(n) = self.recv().await {
                return n;
            }
        }
    }

    async fn open(&mut self, cwd: &std::path::Path) -> Terminal {
        let v = self.call(m::TERMINAL_OPEN, json!({"cwd": cwd, "cols": 80, "rows": 24})).await.unwrap();
        serde_json::from_value(v).unwrap()
    }

    async fn attach(&mut self, id: &str) -> TerminalAttachResult {
        serde_json::from_value(self.call(m::TERMINAL_ATTACH, json!({"terminal_id": id})).await.unwrap()).unwrap()
    }

    async fn input(&mut self, id: &str, text: &str) {
        let data = base64::engine::general_purpose::STANDARD.encode(text);
        self.call(m::TERMINAL_INPUT, json!({"terminal_id": id, "data": data})).await.unwrap();
    }

    /// The next output of `id`: where it begins and its bytes.
    async fn output(&mut self, id: &str) -> (u64, Vec<u8>) {
        loop {
            let n = self.note().await;
            if n.method == m::N_TERMINAL_OUTPUT {
                let o: TerminalOutput = serde_json::from_value(n.params).unwrap();
                if o.terminal_id == id {
                    let data = base64::engine::general_purpose::STANDARD.decode(o.data).unwrap();
                    if o.replay {
                        self.replayed += data.len() as u64;
                    }
                    return (o.offset, data);
                }
            }
        }
    }

    /// Read output from `pos` on, acknowledging it, until it holds `needle`.
    async fn until(&mut self, id: &str, pos: &mut u64, needle: &str) -> String {
        let mut all = Vec::new();
        loop {
            let (offset, data) = self.output(id).await;
            assert_eq!(offset, *pos, "output arrives in order, without gaps");
            *pos += data.len() as u64;
            all.extend_from_slice(&data);
            self.call(m::TERMINAL_ACK, json!({"terminal_id": id, "offset": *pos})).await.unwrap();
            let text = String::from_utf8_lossy(&all);
            if text.contains(needle) {
                return text.into_owned();
            }
        }
    }

    /// The next `terminal.changed` about `id`.
    async fn changed(&mut self, id: &str) -> TerminalChanged {
        loop {
            let n = self.note().await;
            if n.method == m::N_TERMINAL_CHANGED {
                let c: TerminalChanged = serde_json::from_value(n.params).unwrap();
                if c.terminal.terminal_id == id {
                    return c;
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_runs_what_you_type_where_you_asked() {
    let dir = tempdir();
    let host = open(dir.path());
    let work = dir.path().join("project");
    std::fs::create_dir_all(&work).unwrap();
    let mut c = Client::local(&host).await;

    let t = c.open(&work).await;
    assert_eq!((t.cols, t.rows, t.exited), (80, 24, None));
    assert_eq!(t.shell, "/bin/sh");
    assert!(c.changed(&t.terminal_id).await.terminal.exited.is_none(), "opening is announced");
    let a = c.attach(&t.terminal_id).await;
    let mut pos = a.offset;
    assert_eq!(pos, 0);

    // The arithmetic keeps the echo of what was typed from matching.
    c.input(&t.terminal_id, "echo sum-$((6*7)) in $(basename \"$PWD\") on $(tty | cut -c1-8) as $TERM\n").await;
    let out = c.until(&t.terminal_id, &mut pos, "as xterm-256color").await;
    assert!(out.contains("sum-42 in project on /dev/pts as xterm-256color") || out.contains("sum-42 in project on /dev/tty"), "{out}");

    // Resizing reaches the program.
    c.call(m::TERMINAL_RESIZE, json!({"terminal_id": t.terminal_id, "cols": 100, "rows": 30})).await.unwrap();
    c.input(&t.terminal_id, "echo size=$(stty size | tr ' ' x)\n").await;
    c.until(&t.terminal_id, &mut pos, "size=30x100").await;
    assert_eq!(c.replayed, 0, "nothing printed while attached is a replay");

    // The shell ending is reported; the terminal stays until closed.
    c.input(&t.terminal_id, "exit 3\n").await;
    let ended = loop {
        let ch = c.changed(&t.terminal_id).await;
        if ch.terminal.exited.is_some() {
            break ch;
        }
    };
    assert_eq!(ended.terminal.exited, Some(TerminalExit { code: Some(3), signal: None }));
    assert!(!ended.removed);
    let list: TerminalList = serde_json::from_value(c.call(m::TERMINAL_LIST, json!({})).await.unwrap()).unwrap();
    assert_eq!(list.terminals.len(), 1);
    let e = c.call(m::TERMINAL_INPUT, json!({"terminal_id": t.terminal_id, "data": ""})).await.unwrap_err();
    assert_eq!(e.code, codes::INVALID_STATE);

    c.call(m::TERMINAL_CLOSE, json!({"terminal_id": t.terminal_id})).await.unwrap();
    assert!(c.changed(&t.terminal_id).await.removed);
    assert!(host.terminals.list().is_empty());
    assert_eq!(c.call(m::TERMINAL_ATTACH, json!({"terminal_id": t.terminal_id})).await.unwrap_err().code, codes::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_outlives_its_client_and_replays() {
    let dir = tempdir();
    let host = open(dir.path());
    let id = {
        let mut c = Client::local(&host).await;
        let t = c.open(dir.path()).await;
        let a = c.attach(&t.terminal_id).await;
        let mut pos = a.offset;
        c.input(&t.terminal_id, "KEPT=$((40+2)); echo first-$KEPT\n").await;
        c.until(&t.terminal_id, &mut pos, "first-42").await;
        t.terminal_id
        // The connection drops here; the shell does not.
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c = Client::local(&host).await;
    let a = c.attach(&id).await;
    assert_eq!(a.offset, 0);
    assert!(a.end > 0, "what came before the attach is marked as a replay");
    let mut pos = a.offset;
    let replay = c.until(&id, &mut pos, "first-42").await;
    assert!(replay.contains("first-42"));
    assert!(pos <= a.end);
    // Still the same shell: its variable is there.
    c.input(&id, "echo again-$((KEPT+1))\n").await;
    c.until(&id, &mut pos, "again-43").await;
    assert_eq!(c.replayed, a.end, "exactly what came before the attach is marked a replay");

    // Closing ends the shell even though it never exited by itself.
    let pid_gone = {
        c.call(m::TERMINAL_CLOSE, json!({"terminal_id": id})).await.unwrap();
        assert!(c.changed(&id).await.removed);
        host.terminals.list().is_empty()
    };
    assert!(pid_gone);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_waits_for_the_client() {
    let dir = tempdir();
    let host = open(dir.path());
    let mut c = Client::local(&host).await;
    let t = c.open(dir.path()).await;
    let id = t.terminal_id.clone();
    let a = c.attach(&id).await;
    let mut pos = a.offset;
    c.input(&id, "stty -echo; echo ready-$((1+1))\n").await;
    c.until(&id, &mut pos, "ready-2").await;

    // Three megabytes, more than the host keeps, then a marker.
    let total: u64 = 3_000_000;
    c.input(&id, &format!("head -c {total} /dev/zero | tr '\\0' x; echo; echo done-$((2+2))\n")).await;

    // Without acknowledgements, only a window's worth arrives.
    let acked = pos;
    let mut got = 0u64;
    loop {
        match tokio::time::timeout(Duration::from_millis(300), c.output(&id)).await {
            Ok((offset, data)) => {
                assert_eq!(offset, pos);
                pos += data.len() as u64;
                got += data.len() as u64;
            }
            Err(_) => break,
        }
    }
    assert!(pos - acked <= TERMINAL_WINDOW_BYTES, "sent {} past the acknowledgement", pos - acked);
    assert!(got >= TERMINAL_WINDOW_BYTES / 2, "a window's worth did arrive ({got})");

    // Acknowledging lets the rest through, every byte in order.
    c.call(m::TERMINAL_ACK, json!({"terminal_id": id, "offset": pos})).await.unwrap();
    let rest = c.until(&id, &mut pos, "done-4").await;
    let xs = got + rest.bytes().filter(|b| *b == b'x').count() as u64;
    assert!(xs >= total, "every byte arrived: {xs} of {total}");
    c.call(m::TERMINAL_CLOSE, json!({"terminal_id": id})).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_local_socket_gets_a_shell() {
    let dir = tempdir();
    let host = open(dir.path());
    let phone = Principal { device_id: None, capabilities: Capability::phone(), label: "phone".into() };
    let mut c = Client::connect(&host, phone).await;
    for method in [m::TERMINAL_LIST, m::TERMINAL_OPEN, m::TERMINAL_ATTACH, m::TERMINAL_INPUT, m::TERMINAL_RESIZE, m::TERMINAL_ACK, m::TERMINAL_DETACH, m::TERMINAL_CLOSE] {
        let e = c.call(method, json!({"terminal_id": "term_x", "data": "", "cols": 1, "rows": 1, "offset": 0})).await.unwrap_err();
        assert_eq!(e.code, codes::CAPABILITY_DENIED, "{method}");
    }
    assert!(host.terminals.list().is_empty());

    // And a paired device hears nothing about the desktop's terminals.
    let mut local = Client::local(&host).await;
    let t = local.open(dir.path()).await;
    local.call(m::TERMINAL_CLOSE, json!({"terminal_id": t.terminal_id})).await.unwrap();
    c.call(m::HOST_INFO, json!({})).await.unwrap();
    assert!(c.notes.iter().all(|n| n.method != m::N_TERMINAL_CHANGED && n.method != m::N_TERMINAL_OUTPUT));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_needs_a_real_folder() {
    let dir = tempdir();
    let host = open(dir.path());
    let mut c = Client::local(&host).await;
    for cwd in [json!("relative/path"), json!(dir.path().join("missing")), json!("/etc/hostname")] {
        let e = c.call(m::TERMINAL_OPEN, json!({"cwd": cwd})).await.unwrap_err();
        assert_eq!(e.code, codes::INVALID_PARAMS, "{cwd}");
    }
    // No folder: the home folder.
    let t: Terminal = serde_json::from_value(c.call(m::TERMINAL_OPEN, json!({})).await.unwrap()).unwrap();
    assert_eq!(t.cwd, host.ctx.cfg.home_dir.display().to_string());
    assert_eq!((t.cols, t.rows), (80, 24));
    c.call(m::TERMINAL_CLOSE, json!({"terminal_id": t.terminal_id})).await.unwrap();
}
