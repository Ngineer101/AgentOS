//! Drives the FFI API end to end, the way the iOS app does: a real host
//! (with the fake agent) behind the in-process dev relay, pairing confirmed
//! on the desktop side over the local socket, then sessions, streamed events,
//! resume without gaps, capability errors and revocation.

use agenthost_cli::Client;
use agenthost_ffi::{
    pair, AgentHostError, ConnectionListener, DeviceCredentials, PairingListener, RemoteClient,
};
use agenthost_host::relay_dev::DevRelay;
use agenthost_host::{Host, HostConfig};
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const T: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Test host (mirrors crates/host/tests/common/mod.rs)
// ---------------------------------------------------------------------------

static BUILD: Once = Once::new();

fn fake_agent() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    BUILD.call_once(|| {
        let st = std::process::Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "agenthost-fake-agent"])
            .current_dir(&root)
            .status()
            .expect("cargo build fake agent");
        assert!(st.success());
    });
    root.join("target/debug/agenthost-fake-agent")
}

struct TestHost {
    _dir: tempfile::TempDir,
    host: Arc<Host>,
    task: tokio::task::JoinHandle<()>,
    workspace: PathBuf,
}

impl TestHost {
    async fn start(relay_url: String) -> TestHost {
        let dir = tempfile::Builder::new()
            .prefix("ahf")
            .tempdir_in("/tmp")
            .unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.enable_fake_agent = true;
        cfg.fake_agent_unsandboxed = true;
        cfg.fake_agent_path = Some(fake_agent());
        cfg.home_dir = dir.path().join("home");
        cfg.relay_url = Some(relay_url);
        std::fs::create_dir_all(&cfg.home_dir).unwrap();
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&workspace).unwrap();
        let host = Host::open(cfg).unwrap();
        let task = agenthost_host::start(host.clone()).await.unwrap();
        let th = TestHost {
            _dir: dir,
            host,
            task,
            workspace,
        };
        let (c, _) = th.desktop().await;
        for _ in 0..100 {
            let info: HostInfo = c.call(m::HOST_INFO, json!({})).await.unwrap();
            if info.relay.connected {
                return th;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("host never connected to the relay");
    }

    async fn desktop(&self) -> (Client, mpsc::Receiver<jsonrpc::Notification>) {
        Client::connect(&self.host.ctx.cfg.socket_path)
            .await
            .unwrap()
    }

    async fn workspace_id(&self, c: &Client) -> String {
        c.add_workspace(self.workspace.to_str().unwrap(), None)
            .await
            .unwrap()
            .workspace_id
    }

    /// Stop serving (the relay then reports the host offline).
    async fn stop(self) {
        self.host.sessions.shutdown();
        if let Some(d) = self.host.devices.get() {
            d.shutdown();
        }
        self.task.abort();
        for _ in 0..50 {
            if self.host.sessions.live_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn host_and_relay() -> (TestHost, DevRelay) {
    let relay = DevRelay::start("127.0.0.1:0").await.unwrap();
    let th = TestHost::start(relay.url()).await;
    (th, relay)
}

// ---------------------------------------------------------------------------
// Listeners, as Swift would implement them
// ---------------------------------------------------------------------------

struct CodeListener(mpsc::UnboundedSender<String>);
impl PairingListener for CodeListener {
    fn on_short_code(&self, code: String) {
        let _ = self.0.send(code);
    }
}

#[derive(Debug)]
enum Note {
    Notification(String, Value),
    Closed(String, u16),
}

struct Recorder(mpsc::UnboundedSender<Note>);
impl ConnectionListener for Recorder {
    fn on_notification(&self, method: String, params_json: String) {
        let v = serde_json::from_str(&params_json).expect("params are JSON");
        let _ = self.0.send(Note::Notification(method, v));
    }
    fn on_closed(&self, reason: String, code: u16) {
        let _ = self.0.send(Note::Closed(reason, code));
    }
}

fn recorder() -> (Arc<Recorder>, mpsc::UnboundedReceiver<Note>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (Arc::new(Recorder(tx)), rx)
}

/// Pair through the FFI; the desktop confirms after checking the codes match.
async fn pair_via_ffi(th: &TestHost, accept: bool) -> Result<DeviceCredentials, AgentHostError> {
    let (desk, mut dn) = th.desktop().await;
    let start: PairStartResult = desk.call(m::DEVICE_PAIR_START, json!({})).await.unwrap();
    let info = agenthost_ffi::parse_pairing_qr(start.qr_payload.clone()).unwrap();
    assert!(!info.host_name.is_empty());
    let (code_tx, mut code_rx) = mpsc::unbounded_channel();
    let qr = start.qr_payload.clone();
    let phone =
        tokio::spawn(
            async move { pair(qr, "FFI phone".into(), Arc::new(CodeListener(code_tx))).await },
        );
    let pending: PairingPending = loop {
        let n = tokio::time::timeout(T, dn.recv()).await.unwrap().unwrap();
        if n.method == m::N_PAIRING_PENDING {
            break serde_json::from_value(n.params).unwrap();
        }
    };
    // The phone knows its code before the desktop confirms.
    let phone_code = tokio::time::timeout(T, code_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.short_code, phone_code, "codes must match");
    assert_eq!(pending.device_name, "FFI phone");
    desk.request(
        m::DEVICE_PAIR_CONFIRM,
        json!({"pairing_id": start.pairing_id, "accept": accept}),
    )
    .await
    .unwrap();
    phone.await.unwrap()
}

async fn call(c: &RemoteClient, method: &str, params: Value) -> Result<Value, AgentHostError> {
    let r = c.call(method.into(), params.to_string()).await?;
    Ok(serde_json::from_str(&r).expect("result is JSON"))
}

async fn wait_state(c: &RemoteClient, sid: &str, want: &str) {
    let deadline = Instant::now() + T;
    loop {
        let s = call(c, m::SESSION_GET, json!({"session_id": sid}))
            .await
            .unwrap();
        if s["state"] == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "session is {}, wanted {want}",
            s["state"]
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Collect `event` notifications until one matches `pred`; also count deltas.
async fn events_until(
    rx: &mut mpsc::UnboundedReceiver<Note>,
    mut pred: impl FnMut(&Value) -> bool,
) -> (Vec<Value>, usize) {
    let mut events = vec![];
    let mut deltas = 0;
    let deadline = tokio::time::Instant::now() + T;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(Note::Notification(method, params))) => {
                if method == m::N_SESSION_DELTA {
                    deltas += 1;
                } else if method == m::N_EVENT {
                    let done = pred(&params);
                    events.push(params);
                    if done {
                        return (events, deltas);
                    }
                }
            }
            Ok(Some(Note::Closed(r, c))) => panic!("closed ({c}: {r}); got {events:#?}"),
            Ok(None) => panic!("listener dropped"),
            Err(_) => panic!("timed out; got {events:#?}"),
        }
    }
}

fn is_turn_end(e: &Value) -> bool {
    e["type"] == "turn.end"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_connect_sessions_resume_and_revoke() {
    let (th, _relay) = host_and_relay().await;
    let t0 = Instant::now();
    let creds = pair_via_ffi(&th, true).await.unwrap();
    assert!(!creds.device_id.is_empty());
    assert!(!creds.noise_private.is_empty() && !creds.relay_private.is_empty());

    let (listener, mut notes) = recorder();
    let phone = RemoteClient::connect(creds.clone(), listener)
        .await
        .unwrap();
    let hello = phone.hello();
    assert_eq!(hello.device_id.as_deref(), Some(creds.device_id.as_str()));
    assert!(hello.capabilities.contains(&"sessions".to_string()));
    assert!(!hello.capabilities.contains(&"admin".to_string()));
    assert!(phone.is_connected());

    let list = call(&phone, m::SESSION_LIST, json!({"watch": true}))
        .await
        .unwrap();
    assert_eq!(list, json!([]));
    assert!(t0.elapsed() < Duration::from_secs(30));

    // Workspace from the desktop; session, prompt and stream from the phone.
    let (desk, _) = th.desktop().await;
    let ws = th.workspace_id(&desk).await;
    let s = call(
        &phone,
        m::SESSION_CREATE,
        json!({"workspace_id": ws, "agent": "fake"}),
    )
    .await
    .unwrap();
    let sid = s["session_id"].as_str().unwrap().to_string();
    wait_state(&phone, &sid, "idle").await;
    let sub = call(
        &phone,
        m::EVENTS_SUBSCRIBE,
        json!({"session_id": sid, "after_seq": 0}),
    )
    .await
    .unwrap();
    assert!(sub["subscription_id"].is_string());
    let prompt = json!({"session_id": sid, "prompt_id": "p-1", "text": "hello from the phone"});
    let r = call(&phone, m::SESSION_PROMPT, prompt.clone())
        .await
        .unwrap();
    assert_eq!(r["accepted"], true);
    let (events, deltas) = events_until(&mut notes, is_turn_end).await;
    assert!(
        events.iter().any(|e| e["type"] == "agent.message"
            && e["payload"]["text"] == "echo: hello from the phone"),
        "{events:#?}"
    );
    assert!(deltas > 0, "no session.delta notifications streamed");
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "gaps: {seqs:?}");
    assert_eq!(seqs[0], 1, "replay starts at the first event");
    let last_seq = *seqs.last().unwrap();

    // Retrying the same prompt id is safe: one prompt.
    let r = call(&phone, m::SESSION_PROMPT, prompt).await.unwrap();
    assert_eq!(r["accepted"], false);

    // Attachments through the relay: a file bigger than one tunnel frame
    // and one upload piece.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("photo.jpg");
    std::fs::write(&f, vec![7u8; 700_000]).unwrap();
    let att: Value = serde_json::from_str(
        &phone
            .upload_file(f.to_string_lossy().into(), None, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(att["name"], "photo.jpg");
    assert_eq!(att["mime"], "image/jpeg");
    assert_eq!(att["size"], 700_000);
    let r = call(
        &phone,
        m::SESSION_PROMPT,
        json!({"session_id": sid, "prompt_id": "p-att", "text": "see photo", "attachments": [att["attachment_id"]]}),
    )
    .await
    .unwrap();
    assert_eq!(r["accepted"], true);
    let (events, _) = events_until(&mut notes, is_turn_end).await;
    assert!(
        events.iter().any(|e| e["type"] == "agent.message"
            && e["payload"]["text"].as_str().unwrap_or("").contains("/attachments/photo.jpg (image/jpeg")),
        "{events:#?}"
    );
    let last_seq = events.last().unwrap()["seq"].as_i64().unwrap();
    // Voice without the model: a typed error that says to download.
    std::fs::write(dir.path().join("a.wav"), b"RIFF").unwrap();
    match phone
        .transcribe_wav_file(dir.path().join("a.wav").to_string_lossy().into(), None)
        .await
    {
        Err(AgentHostError::Rpc { code, data_json, .. }) => {
            assert_eq!(code, codes::UNAVAILABLE);
            assert!(data_json.unwrap_or_default().contains("needs_download"));
        }
        other => panic!("{other:?}"),
    }

    // Typed errors: capability denied, unknown session, bad params.
    match call(&phone, m::WORKSPACE_ADD, json!({"path": "/tmp"})).await {
        Err(AgentHostError::Rpc { code, .. }) => assert_eq!(code, codes::CAPABILITY_DENIED),
        other => panic!("{other:?}"),
    }
    match call(&phone, m::SESSION_GET, json!({"session_id": "nope"})).await {
        Err(AgentHostError::Rpc { code, .. }) => assert_eq!(code, codes::NOT_FOUND),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        phone.call(m::SESSION_LIST.into(), "{not json".into()).await,
        Err(AgentHostError::InvalidInput { .. })
    ));

    // App-initiated disconnect: no on_closed, calls fail with NotConnected.
    phone.disconnect();
    assert!(matches!(
        phone.call(m::SESSION_LIST.into(), "{}".into()).await,
        Err(AgentHostError::NotConnected)
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(n) = notes.try_recv() {
        assert!(
            !matches!(n, Note::Closed(..)),
            "on_closed after disconnect()"
        );
    }

    // While the phone is away, the desktop runs another turn.
    desk.prompt_with_id(&sid, "p-2", "while you were away")
        .await
        .unwrap();
    let deadline = Instant::now() + T;
    loop {
        let s = desk.get_session(&sid).await.unwrap();
        if s.state == SessionState::Idle && s.last_seq > last_seq + 2 {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Reconnect and resume from the last seen seq: no gaps, no duplicates.
    let (listener, mut notes) = recorder();
    let phone = RemoteClient::connect(creds.clone(), listener)
        .await
        .unwrap();
    call(
        &phone,
        m::EVENTS_SUBSCRIBE,
        json!({"session_id": sid, "after_seq": last_seq}),
    )
    .await
    .unwrap();
    let (missed, _) = events_until(&mut notes, is_turn_end).await;
    let seqs: Vec<i64> = missed.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs[0], last_seq + 1, "{seqs:?}");
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "gaps: {seqs:?}");
    assert!(missed
        .iter()
        .any(|e| e["type"] == "prompt" && e["payload"]["prompt_id"] == "p-2"));

    // Revocation on the desktop: told, then closed with 4401, within 2 s.
    let t0 = Instant::now();
    desk.request(m::DEVICE_REVOKE, json!({"device_id": creds.device_id}))
        .await
        .unwrap();
    let mut told = false;
    let code = loop {
        match tokio::time::timeout(Duration::from_secs(3), notes.recv()).await {
            Ok(Some(Note::Notification(method, _))) if method == m::N_DEVICE_REVOKED => told = true,
            Ok(Some(Note::Notification(..))) => {}
            Ok(Some(Note::Closed(_, code))) => break code,
            other => panic!("no on_closed after revoke: {other:?}"),
        }
    };
    assert!(
        t0.elapsed() < Duration::from_millis(2500),
        "took {:?}",
        t0.elapsed()
    );
    assert!(told, "device.revoked not delivered");
    assert_eq!(
        code,
        agenthost_ffi::protocol_constants().close_device_revoked
    );
    assert!(!phone.is_connected());
    assert!(phone
        .call(m::SESSION_LIST.into(), "{}".into())
        .await
        .is_err());

    // A revoked device cannot come back.
    let (listener, _n) = recorder();
    match RemoteClient::connect(creds, listener).await {
        Err(AgentHostError::Closed { code, .. }) => assert_eq!(code, 4403),
        Err(e) => panic!("{e:?}"),
        Ok(_) => panic!("revoked device connected"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_and_reused_pairing() {
    let (th, _relay) = host_and_relay().await;
    let (desk, mut dn) = th.desktop().await;
    let start: PairStartResult = desk.call(m::DEVICE_PAIR_START, json!({})).await.unwrap();
    let qr = start.qr_payload.clone();
    let (tx, _rx) = mpsc::unbounded_channel();
    let phone =
        tokio::spawn(
            async move { pair(qr, "Unexpected".into(), Arc::new(CodeListener(tx))).await },
        );
    loop {
        let n = tokio::time::timeout(T, dn.recv()).await.unwrap().unwrap();
        if n.method == m::N_PAIRING_PENDING {
            desk.request(
                m::DEVICE_PAIR_CONFIRM,
                json!({"pairing_id": start.pairing_id, "accept": false}),
            )
            .await
            .unwrap();
            break;
        }
    }
    match phone.await.unwrap() {
        Err(AgentHostError::Pairing { .. }) => {}
        other => panic!("{other:?}"),
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    match pair(start.qr_payload, "Again".into(), Arc::new(CodeListener(tx))).await {
        Err(AgentHostError::Closed { code, .. }) => assert_eq!(code, 4403),
        other => panic!("{other:?}"),
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(matches!(
        pair("not a qr".into(), "x".into(), Arc::new(CodeListener(tx))).await,
        Err(AgentHostError::InvalidInput { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_going_offline_is_reported() {
    let (th, _relay) = host_and_relay().await;
    let creds = pair_via_ffi(&th, true).await.unwrap();
    let (listener, mut notes) = recorder();
    let phone = RemoteClient::connect(creds.clone(), listener)
        .await
        .unwrap();
    call(&phone, m::SESSION_LIST, json!({})).await.unwrap();
    th.stop().await;
    // The open connection reports closure (4404 once the relay sees the host go).
    let closed = loop {
        match tokio::time::timeout(Duration::from_secs(10), notes.recv()).await {
            Ok(Some(Note::Closed(r, c))) => break (r, c),
            Ok(Some(_)) => {}
            other => panic!("no on_closed: {other:?}"),
        }
    };
    assert!(!phone.is_connected(), "{closed:?}");
    assert_eq!(closed.1, 4404, "{closed:?}");
    // New connections are refused with 4404 (host offline).
    let mut last = String::new();
    for _ in 0..40 {
        let (listener, _n) = recorder();
        match RemoteClient::connect(creds.clone(), listener).await {
            Err(AgentHostError::Closed { code: 4404, .. }) => return,
            Err(e) => last = e.to_string(),
            Ok(_) => last = "connected".into(),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("never refused with 4404; last: {last}");
}
