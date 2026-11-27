//! The desktop's model and bridge against a real host (in process, fake agent).

use crate::bridge;
use crate::commands::{self, Cmd};
use crate::model::{self, Conn, Screen};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fake_agent() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let st = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "agenthost-fake-agent"]).current_dir(&root).status().unwrap();
    assert!(st.success());
    root.join("target/debug/agenthost-fake-agent").canonicalize().unwrap()
}

#[path = "../../crates/host/tests/common/mcp_server.rs"]
#[allow(dead_code)]
mod mcp_server;

/// Tests that start a host share AGENTHOST_SOCKET, so they take turns.
static HOST_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[track_caller]
fn pump(app: &mut model::App, until: impl Fn(&model::App) -> bool) {
    let t0 = Instant::now();
    while !until(app) {
        app.drain();
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out; conn={:?} error={:?}", app.conn, app.error);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn desktop_model_runs_a_session_against_a_real_host() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahd").tempdir_in("/tmp").unwrap();
    let mut cfg = agenthost_host::HostConfig::in_dir(dir.path());
    cfg.enable_fake_agent = true;
    cfg.fake_agent_unsandboxed = true;
    cfg.fake_agent_path = Some(fake_agent());
    cfg.home_dir = dir.path().join("home");
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let socket = cfg.socket_path.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _host = rt.block_on(async {
        let h = agenthost_host::Host::open(cfg).unwrap();
        agenthost_host::start(h.clone()).await.unwrap();
        h
    });
    std::env::set_var("AGENTHOST_SOCKET", &socket);

    let mut app = model::init();
    app.bridge = Some(bridge::start(Arc::new(|| {}), Default::default()));
    pump(&mut app, |a| a.conn == Conn::Connected && a.info.is_some());

    // Add a workspace, start a session, send a prompt: all through the UI's own actions.
    app.ws_path.replace_text(work.to_str().unwrap());
    app.act("Add workspace", agenthost_proto::methods::WORKSPACE_ADD, serde_json::json!({"path": work}), bridge::Refresh::Workspaces);
    // Quick tasks is always listed; wait for the folder just added.
    pump(&mut app, |a| a.workspaces.iter().any(|w| w.kind != WorkspaceKind::Scratch));
    app.new_ws = app.workspaces.iter().find(|w| w.kind != WorkspaceKind::Scratch).map(|w| w.workspace_id.clone());
    app.new_agent = Some("fake".into());
    app.call(bridge::Tag::SessionCreated, agenthost_proto::methods::SESSION_CREATE, serde_json::json!({"workspace_id": app.new_ws, "agent": "fake"}));
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if a.session(id).map(|s| s.state == SessionState::Idle).unwrap_or(false)));
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    app.prompt.replace_text("hello desktop");
    app.send_prompt();
    // Shown before the host has answered: the message, and that the agent is on it.
    let pending = &app.transcripts[&sid].pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].payload["text"], "hello desktop");
    assert_eq!(model::quiet_work(SessionState::Idle, &app.transcripts[&sid].events, true), Some("Working…"));
    pump(&mut app, |a| {
        a.transcripts.get(&sid).map(|t| t.events.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: hello desktop")).unwrap_or(false)
    });
    // The host's copy replaced it: one bubble, not two.
    assert!(app.transcripts[&sid].pending.is_empty());
    assert_eq!(app.transcripts[&sid].events.iter().filter(|e| e.kind == EventType::Prompt).count(), 1);
    // Seqs are contiguous: nothing missed, nothing doubled.
    let seqs: Vec<i64> = app.transcripts[&sid].events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    assert!(app.prompt.text.is_empty());
    assert!(app.error.is_none(), "{:?}", app.error);
}

#[test]
fn working_shows_while_the_agent_is_quiet() {
    use model::quiet_work;
    let ev = |seq: i64, kind: EventType, payload: serde_json::Value| Event { session_id: "s".into(), seq, kind, payload, ts: 0 };
    let prompt = ev(1, EventType::Prompt, serde_json::json!({"text": "hi"}));
    let running = ev(2, EventType::SessionState, serde_json::json!({"state": "running"}));
    let call = ev(3, EventType::ToolCall, serde_json::json!({"tool_call_id": "c1", "name": "Bash"}));
    let done = ev(4, EventType::ToolResult, serde_json::json!({"tool_call_id": "c1", "status": "completed"}));
    let end = ev(5, EventType::TurnEnd, serde_json::json!({"stop_reason": "end_turn"}));
    // Sent, not yet recorded; recorded, the turn not yet started; started.
    assert_eq!(quiet_work(SessionState::Idle, &[], true), Some("Working…"));
    assert_eq!(quiet_work(SessionState::Idle, &[prompt.clone()], false), Some("Working…"));
    assert_eq!(quiet_work(SessionState::Running, &[prompt.clone(), running.clone()], false), Some("Working…"));
    assert_eq!(quiet_work(SessionState::Starting, &[], true), Some("Starting…"));
    // A running tool shows its own spinner; once it's done the agent is quiet again.
    assert_eq!(quiet_work(SessionState::Running, &[prompt.clone(), running.clone(), call.clone()], false), None);
    assert_eq!(quiet_work(SessionState::Running, &[prompt.clone(), running.clone(), call.clone(), done.clone()], false), Some("Working…"));
    // Waiting for you, or done: nothing.
    assert_eq!(quiet_work(SessionState::WaitingApproval, &[prompt.clone()], false), None);
    assert_eq!(quiet_work(SessionState::Idle, &[prompt, running, call, done, end], false), None);
}

#[test]
fn reply_links_open_only_for_web_and_mail() {
    use crate::view::is_web_link;
    assert!(is_web_link("https://example.com"));
    assert!(is_web_link("HTTP://example.com"));
    assert!(is_web_link("mailto:a@example.com"));
    assert!(!is_web_link("file:///etc/passwd"));
    assert!(!is_web_link("javascript:alert(1)"));
    assert!(!is_web_link("vscode://open?x"));
    assert!(!is_web_link("relative/path.md"));
}

/// Needs a microphone: `cargo test mic_records -- --ignored`.
#[test]
#[ignore]
fn mic_records_a_wav() {
    let r = crate::audio::Recorder::start().expect("microphone");
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let wav = r.finish().expect("audio");
    assert_eq!(&wav[..4], b"RIFF");
    assert!(wav.len() > 16_000, "{}", wav.len());
}

#[test]
fn wav_header_is_valid() {
    let w = crate::audio::wav16(16000, &[0.0, 0.5, -0.5, 1.5]);
    assert_eq!(&w[..4], b"RIFF");
    assert_eq!(&w[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16000);
    assert_eq!(w.len(), 44 + 8);
    assert_eq!(i16::from_le_bytes(w[50..52].try_into().unwrap()), i16::MAX, "clamped");
}

/// The real app (view, poll, bridge) in a headless runtime against a real
/// host: a reply shows while it streams and stays once it's done, with no
/// reconnect or reopen in between.
#[test]
fn replies_show_live_in_the_window() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahw").tempdir_in("/tmp").unwrap();
    let mut cfg = agenthost_host::HostConfig::in_dir(dir.path());
    cfg.enable_fake_agent = true;
    cfg.fake_agent_unsandboxed = true;
    cfg.fake_agent_path = Some(fake_agent());
    cfg.home_dir = dir.path().join("home");
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    cfg.agent_env_overrides.push(("FAKE_AGENT_CHUNK_MS".into(), "15".into()));
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let socket = cfg.socket_path.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _host = rt.block_on(async {
        let h = agenthost_host::Host::open(cfg).unwrap();
        agenthost_host::start(h.clone()).await.unwrap();
        h
    });
    std::env::set_var("AGENTHOST_SOCKET", &socket);
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");

    let mut ui = crate::app().headless_runtime((1180.0, 760.0), rune::Env::default());
    let mut frames = |ui: &mut rune::Runtime<model::App>, until: &dyn Fn(&rune::Runtime<model::App>) -> bool, what: &str| {
        let t0 = Instant::now();
        loop {
            ui.frame(0.016);
            if until(ui) {
                return;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(16));
        }
    };
    frames(&mut ui, &|u| u.find("nav-new").is_some() && u.find("sidebar-search").is_some(), "first frame");
    // Drive the app through its own state, as its handlers do.
    let ws = work.clone();
    ui.update(move |s, _| s.act("Add workspace", agenthost_proto::methods::WORKSPACE_ADD, serde_json::json!({"path": ws}), bridge::Refresh::Workspaces));
    let ready = std::rc::Rc::new(std::cell::Cell::new(false));
    // Poll state through update: true once a workspace exists.
    let check = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let t0 = Instant::now();
    while !check(&mut ui, &|s| s.workspaces.iter().any(|w| w.kind != WorkspaceKind::Scratch) && s.conn == Conn::Connected) {
        ui.frame(0.016);
        std::thread::sleep(Duration::from_millis(16));
        assert!(t0.elapsed() < Duration::from_secs(20), "workspace");
    }
    ready.set(true);
    ui.update(|s, _| {
        let ws = s.workspaces.iter().find(|w| w.kind != WorkspaceKind::Scratch).unwrap().workspace_id.clone();
        s.call(bridge::Tag::SessionCreated, agenthost_proto::methods::SESSION_CREATE, serde_json::json!({"workspace_id": ws, "agent": "fake"}));
    });
    let t0 = Instant::now();
    while !check(&mut ui, &|s| matches!(&s.screen, Screen::Session(id) if s.session(id).map(|x| x.state == SessionState::Idle).unwrap_or(false))) {
        ui.frame(0.016);
        std::thread::sleep(Duration::from_millis(16));
        assert!(t0.elapsed() < Duration::from_secs(20), "session idle");
    }
    // A long reply streams in many chunks.
    let long = "word ".repeat(300);
    let prompt = long.trim().to_string();
    ui.update(move |s, _| {
        s.prompt.replace_text(&prompt);
        s.send_prompt();
    });
    // The very next frame, before any reply from the host: "Working…".
    ui.frame(0.016);
    assert!(ui.find("working").is_some(), "nothing on screen says the agent is working");
    let mut saw_live = false;
    let t0 = Instant::now();
    loop {
        ui.frame(0.016);
        saw_live |= ui.find("live").is_some();
        let n = {
            let r = std::rc::Rc::new(std::cell::Cell::new(0usize));
            let r2 = r.clone();
            ui.update_if(|s, _| {
                if let Screen::Session(id) = &s.screen {
                    r2.set(s.transcripts.get(id).map(|t| t.live.len()).unwrap_or(0));
                }
                false
            });
            r.get()
        };
        if n > 0 && std::env::var_os("SHOW_LIVE").is_some() {
            eprintln!("live len {n} on screen {}", ui.find("live").is_some());
        }
        let done = check(&mut ui, &|s| {
            let Screen::Session(id) = &s.screen else { return false };
            s.transcripts.get(id).map(|t| t.events.iter().any(|e| e.kind == EventType::TurnEnd)).unwrap_or(false)
        });
        if done {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "reply");
        std::thread::sleep(Duration::from_millis(16));
    }
    for _ in 0..5 {
        ui.frame(0.016);
    }
    let ev_on = {
        let r = std::rc::Rc::new(std::cell::Cell::new(0i64));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            let Screen::Session(id) = &s.screen else { return false };
            r2.set(s.transcripts[id].events.iter().rev().find(|e| e.kind == EventType::AgentMessage).map(|e| e.seq).unwrap_or(0));
            false
        });
        r.get()
    };
    eprintln!("final ev-{ev_on} on screen: {} ; prompt row ev-4: {}", ui.find(&format!("ev-{ev_on}")).is_some(), ui.find("ev-4").is_some());
    assert!(saw_live, "the streaming reply never showed");
    let msg_seq = {
        let r = std::rc::Rc::new(std::cell::Cell::new(0i64));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            let Screen::Session(id) = &s.screen else { return false };
            r2.set(s.transcripts[id].events.iter().rev().find(|e| e.kind == EventType::AgentMessage).map(|e| e.seq).unwrap_or(0));
            false
        });
        r.get()
    };
    assert!(msg_seq > 0);
    assert!(ui.find(&format!("ev-{msg_seq}")).is_some(), "the finished reply is not on screen");
}

/// A stream that dies silently (here: unsubscribed behind the app's back)
/// heals by itself: the host's session updates show we're behind, and the
/// app subscribes again from where it is.
#[test]
fn a_lost_event_stream_heals_without_reopening() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahl").tempdir_in("/tmp").unwrap();
    let mut cfg = agenthost_host::HostConfig::in_dir(dir.path());
    cfg.enable_fake_agent = true;
    cfg.fake_agent_unsandboxed = true;
    cfg.fake_agent_path = Some(fake_agent());
    cfg.home_dir = dir.path().join("home");
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let socket = cfg.socket_path.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _host = rt.block_on(async {
        let h = agenthost_host::Host::open(cfg).unwrap();
        agenthost_host::start(h.clone()).await.unwrap();
        h
    });
    // Each test sets the socket it uses right before connecting.
    let socket2 = socket.clone();
    let mut app = model::init();
    std::env::set_var("AGENTHOST_SOCKET", &socket2);
    app.bridge = Some(bridge::start(Arc::new(|| {}), Default::default()));
    let step = |a: &mut model::App| {
        a.drain();
        a.catch_up();
    };
    let wait = |a: &mut model::App, f: &dyn Fn(&model::App) -> bool| {
        let t0 = Instant::now();
        while !f(a) {
            step(a);
            assert!(t0.elapsed() < Duration::from_secs(20), "timed out; error={:?}", a.error);
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait(&mut app, &|a| a.conn == Conn::Connected);
    app.act("Add workspace", agenthost_proto::methods::WORKSPACE_ADD, serde_json::json!({"path": work}), bridge::Refresh::Workspaces);
    wait(&mut app, &|a| a.workspaces.iter().any(|w| w.kind != WorkspaceKind::Scratch));
    let ws = app.workspaces.iter().find(|w| w.kind != WorkspaceKind::Scratch).unwrap().workspace_id.clone();
    app.call(bridge::Tag::SessionCreated, agenthost_proto::methods::SESSION_CREATE, serde_json::json!({"workspace_id": ws, "agent": "fake"}));
    wait(&mut app, &|a| matches!(&a.screen, Screen::Session(id) if a.session(id).map(|s| s.state == SessionState::Idle).unwrap_or(false) && a.sub_ids.contains_key(id)));
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    // Kill the stream without the app knowing.
    let sub = app.sub_ids[&sid].clone();
    app.call(bridge::Tag::Quiet, agenthost_proto::methods::EVENTS_UNSUBSCRIBE, serde_json::json!({"subscription_id": sub}));
    std::thread::sleep(Duration::from_millis(200));
    app.prompt.replace_text("are you there");
    app.send_prompt();
    wait(&mut app, &|a| {
        a.transcripts.get(&sid).map(|t| t.events.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: are you there")).unwrap_or(false)
    });
    let seqs: Vec<i64> = app.transcripts[&sid].events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
}

/// A host on a temp dir with the fake agent; returns it with its socket path.
fn start_host(rt: &tokio::runtime::Runtime, dir: &std::path::Path, relay: Option<String>) -> (Arc<agenthost_host::Host>, std::path::PathBuf) {
    let mut cfg = agenthost_host::HostConfig::in_dir(dir);
    cfg.enable_fake_agent = true;
    cfg.fake_agent_unsandboxed = true;
    cfg.fake_agent_path = Some(fake_agent());
    cfg.home_dir = dir.join("home");
    cfg.relay_url = relay;
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    let socket = cfg.socket_path.clone();
    let h = rt.block_on(async {
        let h = agenthost_host::Host::open(cfg).unwrap();
        agenthost_host::start(h.clone()).await.unwrap();
        h
    });
    (h, socket)
}

fn connect(socket: &std::path::Path) -> model::App {
    std::env::set_var("AGENTHOST_SOCKET", socket);
    let mut app = model::init();
    app.bridge = Some(bridge::start(Arc::new(|| {}), Default::default()));
    pump(&mut app, |a| a.conn == Conn::Connected && a.info.is_some() && !a.workspaces.is_empty());
    app
}

/// ⌘N's sheet: pick a workspace and a login, Start. The session opens and takes a prompt.
#[test]
fn the_new_session_sheet_starts_a_session_you_can_prompt() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahs").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), None);
    let mut app = connect(&socket);
    let mut tx = rune::Tx::default();

    // ⇧⌘N preselects Quick tasks, which needs no folder.
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    assert!(app.new_open);
    assert_eq!(app.new_ws.as_deref(), Some(QUICK_WORKSPACE_ID));
    app.new_agent = Some("fake".into());
    app.start_from_sheet();
    assert!(!app.new_open, "the sheet closes on Start");
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if a.session(id).is_some_and(|s| s.state == SessionState::Idle)));
    app.prompt.replace_text("hello from the sheet");
    app.send_prompt();
    pump(&mut app, |a| {
        let Screen::Session(id) = &a.screen else { return false };
        a.transcripts.get(id).is_some_and(|t| t.events.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: hello from the sheet"))
    });
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    assert_eq!(app.session(&sid).unwrap().workspace_id, QUICK_WORKSPACE_ID);
    // Listed under Quick tasks, first in the sidebar, and ⌘1 jumps to it.
    let groups = app.sidebar_groups();
    assert_eq!(groups[0].0.kind, WorkspaceKind::Scratch);
    app.screen = Screen::Home;
    commands::run(&mut app, Cmd::Jump(1), &mut tx);
    assert_eq!(app.screen, Screen::Session(sid));
    assert!(app.error.is_none(), "{:?}", app.error);
}

/// A write tool waits for approval; the inline card's Approve (⌥⌘A / Alt+A) lets it run.
#[test]
fn an_approval_waits_inline_and_the_approve_shortcut_runs_the_tool() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("aha").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(mcp_server::start(Some("s3cret-desktop-token")));
    let (_host, socket) = start_host(&rt, dir.path(), None);
    let mut app = connect(&socket);
    let mut tx = rune::Tx::default();

    // The app no longer offers bearer tokens; this test server needs one, so add it directly.
    app.call(bridge::Tag::ConnectorAdded, agenthost_proto::methods::CONNECTOR_ADD, serde_json::json!({"url": server.url, "auth": "bearer", "token": "s3cret-desktop-token"}));
    pump(&mut app, |a| a.connectors.iter().any(|c| c.status == "ok" && !c.tools.is_empty()));
    let conn = app.connectors[0].clone();
    assert_eq!(app.sel_conn.as_deref(), Some(conn.connector_id.as_str()));

    app.open_new_session(true);
    app.new_agent = Some("fake".into());
    app.start_from_sheet();
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if a.session(id).is_some_and(|s| s.state == SessionState::Idle)));
    app.prompt.replace_text(&format!(r#"mcp {}__create_note {{"text":"buy milk"}}"#, connector_slug(&conn.name)));
    app.send_prompt();
    pump(&mut app, |a| a.current_approval().is_some());
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    assert!(server.state.calls.lock().unwrap().is_empty(), "called before approval");
    // The toolbar and sidebar say it needs you.
    pump(&mut app, |a| a.session(&sid).is_some_and(|s| s.state == SessionState::WaitingApproval));

    commands::run(&mut app, Cmd::Approve, &mut tx);
    pump(&mut app, |a| {
        a.transcripts.get(&sid).is_some_and(|t| t.events.iter().any(|e| e.kind == EventType::ToolResult && e.payload["status"] == "completed"))
    });
    pump(&mut app, |a| a.approvals.is_empty());
    assert_eq!(server.state.calls.lock().unwrap().len(), 1);
    assert!(app.error.is_none(), "{:?}", app.error);
}

/// Settings › Devices: Pair iPhone… shows the QR, the phone's code appears to compare, and
/// "Codes Match, Pair" adds the device.
#[test]
fn pairing_shows_the_code_and_pairs_the_phone() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahp").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay = rt.block_on(agenthost_host::relay_dev::DevRelay::start("127.0.0.1:0")).unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), Some(relay.url()));
    let mut app = connect(&socket);
    let mut tx = rune::Tx::default();
    pump(&mut app, |a| {
        if !a.info.as_ref().is_some_and(|i| i.relay.connected) {
            a.refresh(bridge::Refresh::HostInfo);
            std::thread::sleep(Duration::from_millis(50));
            return false;
        }
        true
    });

    commands::run(&mut app, Cmd::PairPhone, &mut tx);
    assert_eq!(app.settings, Some(model::Tab::Devices));
    pump(&mut app, |a| a.pairing.is_some());
    let qr = app.pairing.as_ref().unwrap().qr.clone();
    let (code_tx, code_rx) = std::sync::mpsc::channel::<String>();
    let phone = rt.spawn(async move {
        agenthost_cli::remote::pair(&qr, "Test phone", move |c| {
            let _ = code_tx.send(c.to_string());
        })
        .await
    });
    pump(&mut app, |a| a.pairing.as_ref().is_some_and(|p| p.pending.is_some()));
    let pending = app.pairing.as_ref().unwrap().pending.clone().unwrap();
    assert_eq!(pending.device_name, "Test phone");
    assert_eq!(code_rx.recv_timeout(Duration::from_secs(20)).unwrap(), pending.short_code, "both screens show the same code");

    app.confirm_pairing(true);
    pump(&mut app, |a| a.devices.iter().any(|d| d.name == "Test phone") && a.pairing.as_ref().is_some_and(|p| p.done.is_some()));
    assert!(rt.block_on(phone).unwrap().is_ok());
    assert!(app.error.is_none(), "{:?}", app.error);
}

/// The sidebar's order, numbering and search, and the keys that walk it.
#[test]
fn sidebar_groups_numbers_and_search() {
    let mut app = model::init();
    let scene = crate::demo::SCENES.iter().find(|(n, _)| *n == "home").unwrap().1;
    scene(&mut app);
    let mut tx = rune::Tx::default();
    let names: Vec<&str> = app.sidebar_groups().iter().map(|(w, _)| w.name.as_str()).collect();
    assert_eq!(names, ["Quick tasks", "notes", "relay"], "Quick tasks first, then by name; empty workspaces left out");
    // Ended and failed sessions get no number.
    assert_eq!(app.numbered(), ["ses_q", "ses_1", "ses_2", "ses_3"]);
    // One status vocabulary.
    let word = |id: &str| model::Status::of(app.session(id).unwrap().state).word();
    assert_eq!([word("ses_q"), word("ses_1"), word("ses_2"), word("ses_5"), word("ses_4")], ["Your turn", "Needs you", "Working", "Failed", "Ended"]);
    commands::run(&mut app, Cmd::Jump(2), &mut tx);
    assert_eq!(app.screen, Screen::Session("ses_1".into()));
    commands::run(&mut app, Cmd::NextSession, &mut tx);
    assert_eq!(app.screen, Screen::Session("ses_2".into()));
    commands::run(&mut app, Cmd::PrevSession, &mut tx);
    assert_eq!(app.screen, Screen::Session("ses_1".into()));
    // Logins are always labelled.
    assert_eq!(app.login_label(app.session("ses_1").unwrap()), "Claude Code · Work");
    app.search.replace_text("oauth");
    assert_eq!(app.sidebar_order(), ["ses_2"]);
    // Escape-free overlays: ⌘, opens Settings, ⌘N swaps it for the sheet.
    commands::run(&mut app, Cmd::Settings, &mut tx);
    assert_eq!(app.settings, Some(model::Tab::General));
    commands::run(&mut app, Cmd::NewSession, &mut tx);
    assert!(app.new_open && app.settings.is_none());
}

/// Archive: a live session asks first, leaves the list, shows in Settings › Archived, opens
/// read-only, and comes back when unarchived. The first session in a workspace makes its
/// login the default, the sheet preselects it, and "Make … the default" changes it.
#[test]
fn archiving_and_default_logins() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahr").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), None);
    let mut app = connect(&socket);
    let mut tx = rune::Tx::default();
    let idle = |a: &model::App| matches!(&a.screen, Screen::Session(id) if a.session(id).is_some_and(|s| s.state == SessionState::Idle));

    // The first quick task sets Quick tasks' default login to the fake agent.
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    app.new_agent = Some("fake".into());
    app.start_from_sheet();
    pump(&mut app, idle);
    let Screen::Session(first) = app.screen.clone() else { unreachable!() };
    pump(&mut app, |a| a.default_login(QUICK_WORKSPACE_ID).as_deref() == Some("fake"));
    app.new_agent = None;
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    assert_eq!(app.new_agent.as_deref(), Some("fake"), "the sheet preselects the workspace's default");
    app.start_from_sheet();
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if *id != first) && idle(a));
    let Screen::Session(second) = app.screen.clone() else { unreachable!() };

    // Clearing the default from Settings, then "Make … the default" sets it again.
    app.set_default_login(QUICK_WORKSPACE_ID, None);
    pump(&mut app, |a| a.workspace(QUICK_WORKSPACE_ID).is_some_and(|w| w.default_agent.is_none()));
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    app.new_agent = Some("fake".into());
    app.make_default = true;
    app.start_from_sheet();
    pump(&mut app, |a| a.default_login(QUICK_WORKSPACE_ID).as_deref() == Some("fake"));
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if *id != first && *id != second) && idle(a));
    let Screen::Session(third) = app.screen.clone() else { unreachable!() };

    // Archiving the open, live session asks first; confirming selects another one.
    commands::run(&mut app, Cmd::Archive, &mut tx);
    assert_eq!(app.confirm_archive.as_deref(), Some(third.as_str()), "a live session asks first");
    app.archive(&third);
    assert!(app.confirm_archive.is_none());
    assert_ne!(app.screen, Screen::Session(third.clone()), "another session is selected");
    pump(&mut app, |a| !a.sessions.iter().any(|s| s.session_id == third));
    app.open_settings(model::Tab::Archived);
    pump(&mut app, |a| a.archived.iter().any(|s| s.session_id == third && s.archived_at.is_some() && s.state.is_terminal()));
    assert!(!app.sidebar_order().contains(&third));
    // Open shows it read-only; Unarchive brings it back to the list.
    app.close_overlays();
    app.open_session(&third);
    assert!(app.current().is_some_and(|s| s.archived_at.is_some()));
    app.unarchive(&third);
    pump(&mut app, |a| a.sessions.iter().any(|s| s.session_id == third) && !a.archived.iter().any(|s| s.session_id == third));
    // Ended sessions archive at once.
    app.act("End session", agenthost_proto::methods::SESSION_END, serde_json::json!({"session_id": first}), bridge::Refresh::None);
    pump(&mut app, |a| a.session(&first).is_some_and(|s| s.state.is_terminal()));
    app.archive_ended(QUICK_WORKSPACE_ID);
    assert!(app.confirm_archive.is_none());
    pump(&mut app, |a| !a.sessions.iter().any(|s| s.session_id == first));
    assert!(app.error.is_none(), "{:?}", app.error);
}

/// A finished turn you haven't looked at is unread (a green dot) until you open the
/// session; the first list ever marks everything seen, and leaving a session doesn't make it
/// unread again.
#[test]
fn a_finished_turn_is_unread_until_opened() {
    let mut app = demo("home");
    let unread = |app: &model::App, id: &str| app.unread(app.session(id).unwrap());
    assert!(!unread(&app, "ses_q"), "nothing is unread before the first list");
    app.mark_seen(true);
    assert!(!unread(&app, "ses_q"), "the first list marks every session seen");
    // The agent works on ses_q and finishes while another session is open.
    app.open_session("ses_1");
    let x = app.sessions.iter_mut().find(|x| x.session_id == "ses_q").unwrap();
    x.last_seq += 5;
    assert!(unread(&app, "ses_q"));
    assert!(!unread(&app, "ses_1"), "only an idle session is unread: ses_1 needs you");
    app.open_session("ses_q");
    assert!(!unread(&app, "ses_q"), "open is seen");
    app.open_session("ses_1");
    assert!(!unread(&app, "ses_q"), "and stays seen");
}

fn demo(scene: &str) -> model::App {
    let mut app = model::init();
    (crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1)(&mut app);
    app
}

#[test]
fn btw_parses_only_the_command() {
    use model::parse_btw;
    assert_eq!(parse_btw("/btw why is it slow?"), Some("why is it slow?"));
    assert_eq!(parse_btw("  /btw   spaced  "), Some("spaced"));
    assert_eq!(parse_btw("/btw"), Some(""));
    assert_eq!(parse_btw("/btwx no"), None);
    assert_eq!(parse_btw("tell me /btw later"), None);
    assert_eq!(model::slash_suggestions("/b", true), vec![("/btw", "Ask a side question without interrupting")]);
    assert!(model::slash_suggestions("/b", false).is_empty(), "only for agents that answer side questions");
    assert!(model::slash_suggestions("/btw why", true).is_empty());
    assert!(model::slash_suggestions("/x", true).is_empty());
}

#[test]
fn btw_asks_on_the_side_and_opens_the_pane() {
    let mut app = demo("composer-pickers");
    app.prompt.replace_text("/btw which tests touch the relay?");
    app.send_prompt();
    assert!(app.prompt.text.is_empty());
    assert_eq!(app.side_pending.get("ses_1").map(String::as_str), Some("which tests touch the relay?"));
    assert!(app.panel_open && app.side_visible());
    // An empty side question asks for one instead of sending.
    app.prompt.replace_text("/btw");
    app.send_prompt();
    assert!(app.notice.is_some());
    // A reply names the side conversation and selects its tab.
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::SideAsked("ses_1".into()), result: Ok(serde_json::json!({"side_id": "side_z"})) });
    assert!(app.side_pending.is_empty());
    assert_eq!(app.side_tab.get("ses_1").map(String::as_str), Some("side_z"));
    // Agents without side questions get the text as a normal prompt (no side ask).
    let mut app = demo("composer-pickers");
    app.screen = Screen::Session("ses_2".into());
    app.prompt.replace_text("/btw hi");
    app.send_prompt();
    assert!(app.side_pending.is_empty());
}

#[test]
fn live_dictation_writes_words_as_they_come_and_return_sends() {
    use crate::model::{LiveDictation, Mic};
    let live = |gen, done, text: &str| bridge::FromHost::LiveText { gen, done, result: Ok(text.into()) };
    let mut app = demo("composer-pickers");
    app.screen = Screen::Session("ses_2".into());
    // Words go at the caret, spaced from what is typed, and are replaced as they firm up.
    app.prompt.replace_text("Fix the bug");
    app.live = Some(LiveDictation { gen: 1, start: app.prompt.range().0, shown: String::new() });
    app.apply(live(1, false, "in the"));
    assert_eq!(app.prompt.text, "Fix the bug in the");
    app.apply(live(1, false, "in the parser,"));
    assert_eq!(app.prompt.text, "Fix the bug in the parser,");
    assert_eq!(app.prompt.range(), (app.prompt.text.len(), app.prompt.text.len()));
    // A reply for an earlier dictation is dropped.
    app.apply(live(0, false, "stale"));
    assert_eq!(app.prompt.text, "Fix the bug in the parser,");
    // Return while the last words are on their way: nothing is sent yet...
    app.mic = Mic::Transcribing;
    app.send_prompt();
    assert!(app.send_after_dictation);
    assert_eq!(app.prompt.text, "Fix the bug in the parser,");
    // ...then the final text lands and the prompt goes.
    app.apply(live(1, true, "in the parser, please."));
    assert!(app.prompt.text.is_empty(), "sent: {:?}", app.prompt.text);
    assert!(app.live.is_none() && matches!(app.mic, Mic::Idle) && !app.send_after_dictation);

    // In the middle of a line: a space before, none needed after.
    app.prompt.replace_text("Hello world");
    app.prompt.set_caret(5);
    app.live = Some(LiveDictation { gen: 2, start: 5, shown: String::new() });
    app.apply(live(2, false, "there"));
    assert_eq!(app.prompt.text, "Hello there world");
    // Typing after the words while talking keeps the typing and the caret.
    app.prompt.set_caret(app.prompt.text.len());
    app.prompt.insert("!");
    app.apply(live(2, false, "there, big"));
    assert_eq!(app.prompt.text, "Hello there, big world!");
    assert_eq!(app.prompt.range().0, app.prompt.text.len());
    // A failure keeps the words heard and sends nothing.
    app.mic = Mic::Transcribing;
    app.send_after_dictation = true;
    app.apply(bridge::FromHost::LiveText { gen: 2, done: true, result: Err("the host went away".into()) });
    assert_eq!(app.prompt.text, "Hello there, big world!");
    assert!(app.error.is_some() && app.live.is_none() && !app.send_after_dictation);
}

#[test]
fn side_threads_come_from_events_and_deltas() {
    let mut app = demo("side-panel");
    let threads = app.side_threads("ses_1");
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0].title(), "Why does wait_state poll instead of subscribing to events?");
    assert!(!threads[0].streaming() && threads[0].turns[0].answer.contains("deterministic"));
    assert!(threads[1].streaming() && threads[1].live.starts_with("Probably not"));
    // A streamed chunk lands on its side conversation, not the main reply.
    let note = |method: &str, params: serde_json::Value| bridge::FromHost::Note(agenthost_proto::jsonrpc::Notification { jsonrpc: "2.0".into(), method: method.into(), params });
    app.apply(note(agenthost_proto::methods::N_SESSION_DELTA, serde_json::json!({"session_id": "ses_1", "kind": "side.message", "text": " in between.", "side_id": "side_b"})));
    assert!(app.side_threads("ses_1")[1].live.ends_with("in between."));
    assert!(app.transcripts["ses_1"].live.is_empty());
    // The finished message replaces the stream; side.end closes the turn.
    let seq = app.transcripts["ses_1"].last_seq;
    for (i, (kind, payload)) in [("side.message", serde_json::json!({"side_id": "side_b", "text": "Probably not."})), ("side.end", serde_json::json!({"side_id": "side_b", "stop_reason": "end_turn"}))].into_iter().enumerate() {
        let ev = serde_json::json!({"session_id": "ses_1", "seq": seq + 1 + i as i64, "type": kind, "payload": payload, "ts": 0});
        app.apply(note(agenthost_proto::methods::N_EVENT, ev));
    }
    let th = &app.side_threads("ses_1")[1];
    assert!(!th.streaming(), "{th:?}");
    assert_eq!(th.turns[0].answer, "Probably not.");
    assert!(app.transcripts["ses_1"].side_live.is_empty());
    // Closing a tab hides it here; closing the last one hides the pane.
    app.close_side_tab("ses_1", "side_b");
    assert_eq!(app.side_selected("ses_1").as_deref(), Some("side_a"));
    app.close_side_tab("ses_1", "side_a");
    assert!(!app.side_visible() && !app.panel_open);
    assert_eq!(app.side_threads("ses_1").len(), 2, "the conversations stay");
}

#[test]
fn toasts_expire_after_ten_seconds_but_not_while_hovered() {
    use std::time::Duration;
    let mut app = model::init();
    let t0 = Instant::now();
    app.notice = Some("Added Linear.".into());
    app.flush_messages(t0);
    assert!(app.notice.is_none());
    assert_eq!(app.toasts.len(), 1);
    assert_eq!(app.tick_toasts(t0 + Duration::from_secs(4)), Some(Duration::from_secs(6)));
    // Under the pointer the clock stops.
    app.hover_toasts(true, t0 + Duration::from_secs(4));
    assert_eq!(app.tick_toasts(t0 + Duration::from_secs(30)), None);
    assert_eq!(app.toasts.len(), 1);
    app.hover_toasts(false, t0 + Duration::from_secs(30));
    assert_eq!(app.tick_toasts(t0 + Duration::from_secs(35)), Some(Duration::from_secs(1)));
    app.tick_toasts(t0 + Duration::from_secs(36));
    assert!(app.toasts.is_empty());
    // The same message twice is one toast; dismiss removes it.
    app.push_toast(model::ToastKind::Error, "x".into(), None, t0);
    app.push_toast(model::ToastKind::Error, "x".into(), None, t0);
    assert_eq!(app.toasts.len(), 1);
    let id = app.toasts[0].id;
    app.dismiss_toast(id);
    assert!(app.toasts.is_empty());
}

#[test]
fn approvals_elsewhere_get_a_toast_with_show() {
    let mut app = demo("home");
    let a = PendingApproval {
        approval_id: "apr_9".into(),
        session_id: "ses_2".into(),
        connector_id: "con_gh".into(),
        connector_name: "GitHub".into(),
        tool: "create_issue".into(),
        arguments: serde_json::json!({}),
        requested_at: 0,
        expires_at: 0,
        summary: None,
    };
    let reply = |v| bridge::FromHost::Reply { tag: bridge::Tag::Approvals, result: Ok(v) };
    app.apply(reply(serde_json::json!([a])));
    assert_eq!(app.toasts.len(), 1);
    assert_eq!(app.toasts[0].text, "Codex wants to create issue on GitHub");
    assert!(matches!(&app.toasts[0].action, Some(model::ToastAction::ShowApproval { session_id, .. }) if session_id == "ses_2"));
    // Show opens the session.
    let id = app.toasts[0].id;
    app.toast_action(id, &mut rune::Tx::default());
    assert_eq!(app.screen, Screen::Session("ses_2".into()));
    // Once answered, its toast goes.
    app.apply(reply(serde_json::json!([a])));
    assert!(app.toasts.is_empty(), "no second toast for the same approval");
    app.push_toast(model::ToastKind::Approval, "again".into(), Some(model::ToastAction::ShowApproval { session_id: "ses_2".into(), approval_id: "apr_9".into() }), Instant::now());
    app.apply(reply(serde_json::json!([])));
    assert!(app.toasts.is_empty());
}

#[test]
fn pastes_attach_files_and_images_but_keep_text() {
    use model::{paste_as, PasteAs};
    use rune::{ClipboardContent, ClipboardImage};
    let png = crate::brand::png_of_data_url(crate::demo::ICON_LINEAR).unwrap();
    let img = ClipboardImage::from_png(png.clone()).unwrap();
    assert_eq!(paste_as(&ClipboardContent::new("hello")), PasteAs::Text);
    assert_eq!(paste_as(&ClipboardContent::default().with_image(img.clone())), PasteAs::Image(png.clone()));
    // A spreadsheet's cells come with a picture of them: paste the text.
    assert_eq!(paste_as(&ClipboardContent::new("a\tb").with_image(img.clone())), PasteAs::Text);
    // A browser's "Copy Image".
    assert_eq!(paste_as(&ClipboardContent::new("Logo").with_html("<img src=x>").with_image(img.clone())), PasteAs::Image(png));
    // Finder: the files, not their names or icon.
    let f = std::path::PathBuf::from("/tmp/report.pdf");
    assert_eq!(paste_as(&ClipboardContent::new("report.pdf").with_image(img).with_files([f.clone()])), PasteAs::Files(vec![f]));
    assert_eq!(model::pasted_image_name(&[]), "Pasted image.png");
    assert_eq!(model::pasted_image_name(&["Pasted image.png", "Pasted image 2.png"]), "Pasted image 3.png");

    let mut app = demo("composer-pickers");
    let img = ClipboardImage::from_png(crate::brand::png_of_data_url(crate::demo::ICON_SENTRY).unwrap()).unwrap();
    assert!(app.paste_into_composer(&ClipboardContent::default().with_image(img.clone())));
    assert!(app.paste_into_composer(&ClipboardContent::default().with_image(img)));
    let names: Vec<&str> = app.files["ses_1"].iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["Pasted image.png", "Pasted image 2.png"]);
    assert!(!app.paste_into_composer(&ClipboardContent::new("text")));
}

#[test]
fn connector_search_waits_for_typing_to_pause() {
    let mut app = demo("connector-search-empty");
    let t0 = Instant::now();
    app.conn_search.replace_text("lin");
    app.search_edited(t0);
    assert_eq!(app.search_if_due(t0 + Duration::from_millis(100)), Some(Duration::from_millis(200)));
    app.conn_search.replace_text("linear");
    app.search_edited(t0 + Duration::from_millis(200));
    assert!(app.search_if_due(t0 + Duration::from_millis(400)).is_some(), "the timer restarted");
    assert_eq!(app.search_if_due(t0 + Duration::from_millis(500)), None);
    assert!(app.searching && app.search_query == "linear");
    // A late answer for an older query is ignored.
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::ConnectorSearch("lin".into()), result: Ok(serde_json::json!([])) });
    assert!(app.searching);
    let sug = serde_json::json!([{"domain": "linear.app", "name": "Linear", "url": "https://mcp.linear.app/mcp", "auth": "oauth"}, {"domain": "stripe.com", "name": "Stripe", "url": "https://mcp.stripe.com", "auth": "api_key"}]);
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::ConnectorSearch("linear".into()), result: Ok(sug) });
    assert!(!app.searching);
    assert_eq!(app.suggestions.len(), 2);
    assert!(model::suggestion_supported(&app.suggestions[0]) && !model::suggestion_supported(&app.suggestions[1]));
    // An API-key server can't be added; an OAuth one closes the form.
    let stripe = app.suggestions[1].clone();
    app.add_suggestion(&stripe);
    assert!(app.adding_connector);
    let linear = app.suggestions[0].clone();
    app.add_suggestion(&linear);
    assert!(!app.adding_connector && app.suggestions.is_empty());
}

#[test]
fn renaming_a_connector_shows_the_hosts_refusal() {
    let mut app = demo("connector-detail");
    app.start_rename("con_sw");
    assert_eq!(app.rename.text, "Sentry (work)");
    app.rename.replace_text("  ");
    app.rename_connector("con_sw");
    assert!(app.rename_error.is_some(), "an empty name is refused here");
    app.rename.replace_text("work");
    app.rename_connector("con_sw");
    assert!(app.rename_error.is_none());
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::Rename("con_sw".into()), result: Err(agenthost_proto::jsonrpc::RpcError { code: -32602, message: "name taken".into(), data: None }) });
    assert_eq!(app.rename_error, Some(("con_sw".into(), "name taken".into())));
    assert_eq!(app.renaming.as_deref(), Some("con_sw"), "the field stays open to fix it");
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::Rename("con_sw".into()), result: Ok(serde_json::json!({})) });
    assert!(app.renaming.is_none() && app.rename_error.is_none());
    assert_eq!(connector_slug("project-x"), "project_x");
}

#[test]
fn model_and_effort_come_from_config_options() {
    let app = demo("composer-pickers");
    let s = app.session("ses_1").unwrap();
    assert_eq!(model::App::config_option(s, "model").map(|o| o.current_value.as_str()), Some("opus"));
    assert_eq!(model::App::config_option(s, "thought_level").map(|o| o.id.as_str()), Some("effort"));
    assert!(model::App::config_option(app.session("ses_2").unwrap(), "model").is_none());
    // The chip shows for a session with settings, not for one without.
    assert!(model::App::has_model_settings(s));
    assert!(!model::App::has_model_settings(app.session("ses_2").unwrap()));
}

#[test]
fn fast_mode_is_an_on_off_setting() {
    let app = demo("model-popover");
    let s = app.session("ses_1").unwrap();
    let fast = model::App::fast_option(s).expect("the demo's fast option");
    assert_eq!((fast.id.as_str(), fast.current_value.as_str()), ("fast", "on"));
    let v = |value: &str| SessionConfigValue { value: value.into(), name: value.into(), description: None };
    let opt = |id: &str, category: Option<&str>, values: &[&str]| SessionConfigOption {
        id: id.into(),
        name: id.into(),
        description: None,
        category: category.map(String::from),
        current_value: values.first().map(|x| x.to_string()).unwrap_or_default(),
        options: values.iter().map(|x| v(x)).collect(),
    };
    let mut sess = s.clone();
    // By category, in either order.
    sess.config_options = vec![opt("speed", Some("model_config"), &["off", "on"])];
    assert_eq!(model::App::fast_option(&sess).map(|o| o.id.as_str()), Some("speed"));
    // Not with other values, or another id and category.
    sess.config_options = vec![opt("fast", None, &["on", "off", "auto"])];
    assert!(model::App::fast_option(&sess).is_none());
    sess.config_options = vec![opt("turbo", Some("mode"), &["on", "off"])];
    assert!(model::App::fast_option(&sess).is_none());
    // Fast mode alone is enough for the chip.
    sess.config_options = vec![opt("fast", None, &["off", "on"])];
    assert!(model::App::has_model_settings(&sess));
}

#[test]
fn effort_steps_through_its_levels() {
    let app = demo("model-popover");
    let e = model::App::config_option(app.session("ses_1").unwrap(), "thought_level").unwrap();
    assert_eq!(e.options.iter().map(|v| v.value.as_str()).collect::<Vec<_>>(), ["low", "medium", "high", "max"]);
    let at = model::stop_of(e);
    assert_eq!(at, 2, "high");
    assert_eq!(model::step_stop(4, at, 1), 3);
    assert_eq!(model::step_stop(4, 3, 1), 3, "clamps at Max");
    assert_eq!(model::step_stop(4, at, -10), 0, "Page Down to Low");
    assert_eq!(model::step_stop(0, 0, 1), 0);
    // A drag lands on the nearest stop.
    assert_eq!(model::stop_at(4, 0.0, 300.0), 0);
    assert_eq!(model::stop_at(4, 140.0, 300.0), 1);
    assert_eq!(model::stop_at(4, 160.0, 300.0), 2);
    assert_eq!(model::stop_at(4, 999.0, 300.0), 3);
    assert_eq!(model::stop_at(4, -5.0, 300.0), 0);
    assert_eq!(model::stop_at(1, 50.0, 300.0), 0);
    assert!(demo("composer").menu.is_none());
    assert_eq!(demo("model-popover").menu, Some(model::Menu::Model));
}

#[test]
fn connector_icons_decode_from_data_urls() {
    assert!(crate::brand::data_url_pixels(crate::demo::ICON_LINEAR).is_some());
    assert!(crate::brand::data_url_pixels("data:image/png;base64,AAAA").is_none());
    assert!(crate::brand::data_url_pixels("https://example.com/x.png").is_none());
}


#[test]
fn the_model_chip_drops_a_trailing_parenthetical() {
    use crate::session::chip_label;
    assert_eq!(chip_label("Default (recommended)"), "Default");
    assert_eq!(chip_label("Opus 4.5"), "Opus 4.5");
    assert_eq!(chip_label("Sonnet 4.5 (1M context)"), "Sonnet 4.5");
    assert_eq!(chip_label("(recommended)"), "(recommended)", "nothing would be left");
    assert_eq!(chip_label("gpt-5 (high) fast"), "gpt-5 (high) fast", "only a trailing one");
    // The popover keeps the full name.
    let app = demo("composer-default-model");
    let o = model::App::config_option(app.session("ses_1").unwrap(), "model").unwrap();
    assert_eq!(o.options.iter().find(|v| v.value == o.current_value).map(|v| v.name.as_str()), Some("Default (recommended)"));
}

#[test]
fn an_untitled_session_is_a_new_session() {
    let app = demo("header-titled");
    let x = app.session("ses_1").unwrap();
    assert_eq!(app.session_title(x), "Fix the flaky resume test");
    assert_eq!(app.session_context(x), "Claude Code · Work · relay");
    let app = demo("header-quick-untitled");
    let x = app.session("ses_q").unwrap();
    assert_eq!(app.session_title(x), "New session");
    assert_eq!(app.session_context(x), "Claude Code · Personal · Quick tasks");
    let app = demo("header-untitled");
    let x = app.session("ses_2").unwrap();
    assert_eq!(app.session_title(x), "New session");
    assert!(app.session_context(x).ends_with("relay"), "{}", app.session_context(x));
    let mut blank = x.clone();
    blank.title = Some("   ".into());
    assert_eq!(app.session_title(&blank), "New session");
    // Ended sessions keep the workspace and branch.
    let app = demo("header-ended");
    let x = app.session("ses_1").unwrap();
    assert!(x.state.is_terminal() && x.worktree.is_some());
    assert_eq!(app.session_context(x), "Claude Code · Work · relay");
}

#[test]
fn renaming_a_session_in_the_header() {
    let mut app = demo("header-titled");
    let mut tx = rune::Tx::default();
    commands::run(&mut app, Cmd::RenameSession, &mut tx);
    assert_eq!(app.session_rename.as_deref(), Some("ses_1"));
    assert_eq!(app.title_edit.text, "Fix the flaky resume test");
    // Unchanged: Return just closes the field.
    app.save_session_rename();
    assert!(app.session_rename.is_none());
    // Too long: refused here, the field stays.
    app.start_session_rename();
    app.title_edit.replace_text("x".repeat(MAX_SESSION_TITLE + 1));
    app.save_session_rename();
    assert!(app.error.take().is_some());
    assert_eq!(app.session_rename.as_deref(), Some("ses_1"));
    // The host's refusal is a toast and the field stays open.
    app.title_edit.replace_text("Flaky resume test");
    app.save_session_rename();
    let err = agenthost_proto::jsonrpc::RpcError { code: -32602, message: "no such session".into(), data: None };
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::SessionRenamed("ses_1".into()), result: Err(err) });
    assert!(app.error.as_deref().is_some_and(|e| e.contains("no such session")));
    assert_eq!(app.session_rename.as_deref(), Some("ses_1"));
    // The reply is the renamed session.
    let mut renamed = app.session("ses_1").unwrap().clone();
    renamed.title = Some("Flaky resume test".into());
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::SessionRenamed("ses_1".into()), result: Ok(serde_json::to_value(&renamed).unwrap()) });
    assert!(app.session_rename.is_none());
    assert_eq!(app.session_title(app.session("ses_1").unwrap()), "Flaky resume test");
    // Escape cancels; opening another session closes the field.
    app.start_session_rename();
    app.cancel_session_rename();
    assert!(app.session_rename.is_none());
    app.start_session_rename();
    app.open_session("ses_2");
    assert!(app.session_rename.is_none());
    // Archived sessions can't be renamed.
    let mut app = demo("archived-open");
    app.start_session_rename();
    assert!(app.session_rename.is_none());
    // The menu bar has both items.
    let titles: Vec<_> = commands::menu_table().into_iter().filter_map(|(t, ..)| t).collect();
    assert!(titles.contains(&"Rename Session…") && titles.contains(&"Copy Branch Name"));
}

#[test]
fn copying_the_branch_says_so() {
    let mut app = demo("header-titled");
    let mut tx = rune::Tx::default();
    commands::run(&mut app, Cmd::CopyBranch, &mut tx);
    assert_eq!(app.notice.as_deref(), Some("Copied branch name"));
    // A session without a worktree has no branch to copy.
    let mut app = demo("header-quick-untitled");
    commands::run(&mut app, Cmd::CopyBranch, &mut tx);
    assert!(app.notice.is_none());
}

/// `session.rename` against a real host: a title, then a blank one clears it.
#[test]
fn session_rename_round_trips_through_the_host() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahn").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), None);
    let mut app = connect(&socket);
    let mut tx = rune::Tx::default();
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    app.new_agent = Some("fake".into());
    app.start_from_sheet();
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if a.session(id).is_some_and(|s| s.state == SessionState::Idle)));
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    assert_eq!(app.session_title(app.session(&sid).unwrap()), "New session");
    commands::run(&mut app, Cmd::RenameSession, &mut tx);
    app.title_edit.replace_text("  Tidy the build  ");
    app.save_session_rename();
    pump(&mut app, |a| a.session_rename.is_none() && a.session(&sid).is_some_and(|s| s.title.as_deref() == Some("Tidy the build")));
    commands::run(&mut app, Cmd::RenameSession, &mut tx);
    app.title_edit.replace_text("");
    app.save_session_rename();
    pump(&mut app, |a| a.session_rename.is_none() && a.session(&sid).is_some_and(|s| s.title.is_none()));
    assert_eq!(app.session_title(app.session(&sid).unwrap()), "New session");
    assert!(app.error.is_none(), "{:?}", app.error);
}

/// `session.pin` against a real host: the row moves to Pinned at once, the host keeps it,
/// and unpinning sends it back to its workspace.
#[test]
fn pins_round_trip_through_the_host() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahp").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), None);
    let mut app = connect(&socket);
    let mut tx = rune::Tx::default();
    assert!(app.has_pins(), "the host is on protocol 8");
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    app.new_agent = Some("fake".into());
    app.start_from_sheet();
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if a.session(id).is_some_and(|s| s.state == SessionState::Idle)));
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    app.set_pinned(&sid, true);
    assert_eq!(app.pinned_sessions().len(), 1, "it moves at once");
    // The host's own copy says so too (a fresh list, not ours).
    app.call(bridge::Tag::Sessions, agenthost_proto::methods::SESSION_LIST, serde_json::json!({}));
    pump(&mut app, |a| a.session(&sid).is_some_and(|s| s.pinned_at.is_some()) && a.error.is_none());
    app.set_pinned(&sid, false);
    pump(&mut app, |a| a.session(&sid).is_some_and(|s| s.pinned_at.is_none()));
    assert!(app.sidebar_groups().iter().any(|(_, l)| l.iter().any(|x| x.session_id == sid)));
    assert!(app.error.is_none(), "{:?}", app.error);
}

#[test]
fn subagents_group_children_under_their_calls() {
    let app = demo("subagents");
    let agents = app.subagents("ses_6");
    assert_eq!(agents.len(), 2);
    let (done, live) = (&agents[0], &agents[1]);
    assert_eq!((done.title(), done.agent_type.as_str(), done.status.as_str()), ("Review auth handling", "Explore", "completed"));
    assert_eq!(done.steps(), 9);
    assert!(done.report.starts_with("Found **2 issues**"));
    assert!(done.items.iter().any(|i| matches!(i, model::SubItem::Message(m) if m.contains("compares the token"))));
    assert_eq!((live.title(), live.status.as_str(), live.steps()), ("Profile the hot paths", "running", 4));
    assert!(live.prompt.starts_with("Measure request latency"));
    // The message sits between the third and fourth steps.
    assert!(matches!(&live.items[3], model::SubItem::Message(_)) && matches!(&live.items[4], model::SubItem::Step { call_id, .. } if call_id == "p4"));
    // Without a description it's "Subagent"; tool_name alone marks the call.
    let ev = |seq, kind, p: serde_json::Value| Event { session_id: "x".into(), seq, kind, payload: p, ts: seq * 1000 };
    let evs = vec![
        ev(1, EventType::ToolCall, serde_json::json!({"tool_call_id": "a", "tool_name": "Task", "name": "Task"})),
        ev(2, EventType::ToolCall, serde_json::json!({"tool_call_id": "c", "parent_tool_call_id": "a", "name": "ls"})),
        ev(3, EventType::ToolCall, serde_json::json!({"tool_call_id": "d", "name": "pwd"})),
    ];
    let a = model::subagents_of(&evs, true);
    assert_eq!((a.len(), a[0].title(), a[0].steps()), (1, "Subagent", 1));
    // A turn that ended without its result stopped the agent.
    assert_eq!(model::subagents_of(&evs, false)[0].status, "stopped");
}

#[test]
fn subagent_children_leave_the_main_transcript() {
    let app = demo("subagents");
    let events = &app.transcripts["ses_6"].events;
    let children: Vec<_> = events.iter().filter(|e| model::is_child_event(e)).collect();
    // 9 + 4 calls, 9 + 3 results, 2 messages.
    assert_eq!(children.len(), 27);
    assert!(children.iter().all(|e| e.kind != EventType::ToolCall || !model::is_subagent_call(&e.payload)));
    // The subagent calls themselves and their results stay in the main flow (as the cards).
    assert!(events.iter().filter(|e| model::is_subagent_call(&e.payload)).all(|e| !model::is_child_event(e)));
    assert!(!events.iter().any(|e| e.kind == EventType::ToolResult && e.payload["tool_call_id"] == "ag1" && model::is_child_event(e)));
}

#[test]
fn running_agents_and_commands_are_counted() {
    let mut app = demo("subagents");
    assert_eq!(app.running_work("ses_6"), (1, 0));
    assert_eq!(model::running_label(1, 0).as_deref(), Some("1 agent running"));
    assert_eq!(model::running_label(2, 0).as_deref(), Some("2 agents running"));
    assert_eq!(model::running_label(1, 1).as_deref(), Some("1 agent and 1 command running"));
    assert_eq!(model::running_label(0, 2).as_deref(), Some("2 commands running"));
    assert_eq!(model::running_label(0, 0), None);
    // A background command still running counts too.
    let tr = app.transcripts.get_mut("ses_6").unwrap();
    let seq = tr.last_seq + 1;
    tr.events.push(Event { session_id: "ses_6".into(), seq, kind: EventType::ToolCall, payload: serde_json::json!({"tool_call_id": "bg", "name": "cargo watch", "kind": "execute", "background": true}), ts: 0 });
    assert_eq!(app.running_work("ses_6"), (1, 1));
    // When the session stops, nothing runs.
    app.sessions.iter_mut().find(|x| x.session_id == "ses_6").unwrap().state = SessionState::Idle;
    assert_eq!(app.running_work("ses_6"), (0, 0));
    assert_eq!(app.subagents("ses_6")[1].status, "stopped");
}

#[test]
fn agent_panel_opens_closes_and_follows_events() {
    let mut app = demo("subagents");
    app.open_agent("ses_6", "ag2");
    assert!(app.agent_visible() && app.trailing_visible());
    // It stands in for the side pane while it's open.
    app.show_panel(model::PanelTab::Side);
    assert!(!app.side_visible());
    // A new step and the result arrive: the panel's agent has them.
    let note = |params: serde_json::Value| bridge::FromHost::Note(agenthost_proto::jsonrpc::Notification { jsonrpc: "2.0".into(), method: agenthost_proto::methods::N_EVENT.into(), params });
    let seq = app.transcripts["ses_6"].last_seq;
    app.apply(note(serde_json::json!({"session_id": "ses_6", "seq": seq + 1, "type": "subagent.message", "payload": {"parent_tool_call_id": "ag2", "text": "The lookup costs 40 µs per frame."}, "ts": 0})));
    app.apply(note(serde_json::json!({"session_id": "ses_6", "seq": seq + 2, "type": "tool.result", "payload": {"tool_call_id": "ag2", "status": "completed", "output": "**tunnel.rs** is the slowest handler."}, "ts": 0})));
    let a = app.subagent("ses_6", "ag2").unwrap();
    assert!(matches!(a.items.last(), Some(model::SubItem::Message(m)) if m.contains("40 µs")));
    assert_eq!((a.status.as_str(), a.report.as_str()), ("completed", "**tunnel.rs** is the slowest handler."));
    assert_eq!(app.running_work("ses_6"), (0, 0));
    assert_eq!(model::plain_lines(&a.report), vec!["tunnel.rs is the slowest handler."]);
    // Escape closes it, and the side pane (once it has questions) comes back.
    app.side_pending.insert("ses_6".into(), "why?".into());
    assert!(!app.side_visible());
    let mut tx = rune::Tx::default();
    let esc = rune::KeyEvent { key: rune::Key::Escape, mods: rune::Mods::default() };
    assert!(commands::on_key(&mut app, &esc, &mut tx));
    assert!(!app.agent_visible() && app.side_visible());
    // Another session closes it.
    app.open_agent("ses_6", "ag1");
    app.open_session("ses_1");
    assert!(app.agent_open.is_none());
}

#[test]
fn markdown_reports_read_as_plain_lines() {
    assert_eq!(model::plain_lines("## Findings\n\n- **One** [link](https://x.y) `code`\n\n```\nx\n```\n---"), vec!["Findings", "One link code", "x"]);
    assert_eq!(model::span_label(38_000), "38s");
    assert_eq!(model::span_label(72_000), "1m 12s");
}

/// Settings › Connectors › Built-in tools: opening the tab lists the host's own tools, a
/// change round-trips through the real host, and installing can't be set to run unasked.
#[test]
fn built_in_tool_policies_round_trip_through_the_host() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahb").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), None);
    let mut app = connect(&socket);
    app.builtins.clear();
    app.open_settings(model::Tab::Connectors);
    pump(&mut app, |a| !a.builtins.is_empty());
    let pr = app.builtin("create_pull_request").unwrap().clone();
    assert_eq!((pr.policy, pr.can_auto), (ToolPolicy::Ask, true));
    assert!(!app.builtin("install_tool").unwrap().can_auto);

    app.set_builtin_policy("create_pull_request", ToolPolicy::Auto);
    pump(&mut app, |a| a.builtin("create_pull_request").is_some_and(|b| b.policy == ToolPolicy::Auto));
    assert!(app.toasts.is_empty(), "a change in Settings needs no toast");
    // Reopening Settings reads it back from the host.
    app.builtins.clear();
    app.open_settings(model::Tab::Connectors);
    pump(&mut app, |a| a.builtin("create_pull_request").is_some_and(|b| b.policy == ToolPolicy::Auto));
    app.set_builtin_policy("create_pull_request", ToolPolicy::Ask);
    pump(&mut app, |a| a.builtin("create_pull_request").is_some_and(|b| b.policy == ToolPolicy::Ask));

    // The host refuses installing without asking: an error toast, and the list stays.
    app.set_builtin_policy("install_tool", ToolPolicy::Auto);
    pump(&mut app, |a| a.error.is_some());
    assert!(app.error.as_deref().unwrap().starts_with("Could not change the setting"), "{:?}", app.error);
    app.error = None;
    pump(&mut app, |a| a.builtin("install_tool").is_some());
    assert_eq!(app.builtin("install_tool").unwrap().policy, ToolPolicy::Ask);

    // "Approve and Don't Ask Again"'s call: the reply sets the policy and says where to undo it.
    app.call(bridge::Tag::BuiltinSet { announce: Some("git_push".into()) }, agenthost_proto::methods::BUILTIN_SET_POLICY, serde_json::json!({"name": "git_push", "policy": "auto"}));
    pump(&mut app, |a| !a.toasts.is_empty());
    assert_eq!(app.builtin("git_push").unwrap().policy, ToolPolicy::Auto);
    assert!(app.toasts[0].text.starts_with("Pushes now run without asking"), "{}", app.toasts[0].text);
    assert!(app.error.is_none(), "{:?}", app.error);
}

/// The card offers "Approve and Don't Ask Again" for pull requests and plain pushes that ask,
/// never for a force-push, installing, or a connector's tool.
#[test]
fn dont_ask_again_is_offered_only_where_settings_could_allow_it() {
    let offered = |scene: &str| {
        let app = demo(scene);
        let a = app.approvals[0].clone();
        app.can_stop_asking(&a)
    };
    assert!(offered("pr-approval"));
    assert!(offered("push-approval"));
    assert!(!offered("force-push-approval"), "a force-push always asks");
    assert!(!offered("install-approval"), "installing always asks");
    assert!(offered("command-approval"), "a command offers its prefix");
    assert!(!offered("session"), "a connector's tool has its own policies");
    // Pushes already run unasked in the base demo: nothing to offer.
    let mut app = demo("push-approval");
    app.builtins[1].policy = ToolPolicy::Auto;
    let a = app.approvals[0].clone();
    assert!(!app.can_stop_asking(&a));
    // A command whose prefix already runs unasked, or a shell (no prefix offered): no offer.
    let mut app = demo("command-approval");
    app.builtins[3].allowed.push(vec!["gh".into(), "pr".into(), "edit".into()]);
    let a = app.approvals[0].clone();
    assert!(!app.can_stop_asking(&a));
    let mut app = demo("command-approval");
    app.approvals[0].arguments["allow_rule"] = serde_json::Value::Null;
    let a = app.approvals[0].clone();
    assert!(!app.can_stop_asking(&a));
    // A host without built-in tool settings: no offer.
    let mut app = demo("pr-approval");
    app.builtins.clear();
    let a = app.approvals[0].clone();
    assert!(!app.can_stop_asking(&a));
}

/// After "Approve and Don't Ask Again": the host's reply sets the policy, a toast says pull
/// requests now open without asking, and its Settings button opens Settings › Connectors.
#[test]
fn dont_ask_again_toasts_and_its_button_opens_settings() {
    let mut app = demo("pr-approval");
    let mut list = app.builtins.clone();
    list[0].policy = ToolPolicy::Auto;
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::BuiltinSet { announce: Some("create_pull_request".into()) }, result: Ok(serde_json::to_value(&list).unwrap()) });
    assert_eq!(app.builtin("create_pull_request").unwrap().policy, ToolPolicy::Auto);
    assert_eq!(app.toasts.len(), 1);
    assert!(app.toasts[0].text.starts_with("Pull requests now open without asking — change this in "), "{}", app.toasts[0].text);
    assert_eq!(app.toasts[0].action, Some(model::ToastAction::OpenSettings(model::Tab::Connectors)));
    // An approvals refresh keeps it.
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::Approvals, result: Ok(serde_json::json!([])) });
    assert_eq!(app.toasts.len(), 1);
    let id = app.toasts[0].id;
    app.toast_action(id, &mut rune::Tx::default());
    assert_eq!(app.settings, Some(model::Tab::Connectors));
    assert!(app.toasts.is_empty());
    // A refusal: an error, no toast saying it worked.
    let mut app = demo("pr-approval");
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::BuiltinSet { announce: Some("create_pull_request".into()) }, result: Err(agenthost_proto::jsonrpc::RpcError { code: -32602, message: "nope".into(), data: None }) });
    assert!(app.toasts.is_empty());
    assert_eq!(app.error.as_deref(), Some("Could not change the setting: nope"));
}

/// "Always Allow “gh pr edit”": the host's reply lists the prefix, a toast names it and
/// where to undo it; Ask First in Settings takes it off again.
#[test]
fn always_allowing_a_command_names_it_and_settings_can_undo_it() {
    let mut app = demo("command-approval");
    let rule: Vec<String> = vec!["gh".into(), "pr".into(), "edit".into()];
    assert_eq!(model::allow_rule(&app.approvals[0]), Some(rule.clone()));
    let mut list = app.builtins.clone();
    list[3].allowed.push(rule.clone());
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::CommandSet { announce: Some(rule.clone()) }, result: Ok(serde_json::to_value(&list).unwrap()) });
    assert!(app.builtin("run_command").unwrap().allowed.contains(&rule));
    assert_eq!(app.toasts.len(), 1);
    assert!(app.toasts[0].text.starts_with("\u{201c}gh pr edit\u{201d} commands now run without asking — change this in "), "{}", app.toasts[0].text);
    assert_eq!(app.toasts[0].action, Some(model::ToastAction::OpenSettings(model::Tab::Connectors)));
    // Taking it off again (Settings' Ask First) needs no toast.
    let mut app = demo("command-approval");
    let list = app.builtins.clone();
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::CommandSet { announce: None }, result: Ok(serde_json::to_value(&list).unwrap()) });
    assert!(app.toasts.is_empty());
    // A refusal: an error, no toast saying it worked.
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::CommandSet { announce: Some(rule) }, result: Err(agenthost_proto::jsonrpc::RpcError { code: -32602, message: "bash always asks".into(), data: None }) });
    assert!(app.toasts.is_empty());
    assert_eq!(app.error.as_deref(), Some("Could not change the setting: bash always asks"));
}

#[test]
fn a_run_of_tool_calls_is_summed_up_in_one_line() {
    let call = |kind: &str, source: &str| Event { session_id: "s".into(), seq: 1, kind: EventType::ToolCall, payload: serde_json::json!({"kind": kind, "source": source}), ts: 0 };
    let (a, b, c, d, e) = (call("execute", "sandbox"), call("execute", "sandbox"), call("read", "sandbox"), call("edit", "sandbox"), call("other", "mcp"));
    assert_eq!(crate::session::activity_title(&[&a, &b, &c, &d, &e]), "Ran 2 commands, read 1 file, edited 1 file, used 1 tool");
    assert_eq!(crate::session::activity_title(&[&c]), "Read 1 file");
    // A connector's read is a tool, not a file.
    assert_eq!(crate::session::activity_title(&[&call("read", "mcp")]), "Used 1 tool");
    assert_eq!(crate::session::activity_title(&[]), "Thought");
}

#[test]
fn a_multi_line_command_shows_its_first_line() {
    assert_eq!(crate::session::one_line("python3 - <<'EOF'\np='x'\nEOF"), "python3 - <<'EOF' …");
    assert_eq!(crate::session::one_line("  ls -la  "), "ls -la");
}

// ---- automations -------------------------------------------------------------------------

fn auto_note(ch: AutomationChanged) -> bridge::FromHost {
    bridge::FromHost::Note(agenthost_proto::jsonrpc::Notification { jsonrpc: "2.0".into(), method: agenthost_proto::methods::N_AUTOMATION_CHANGED.into(), params: serde_json::to_value(ch).unwrap() })
}

fn agents_of(app: &model::App) -> Vec<AgentStatus> {
    app.info.as_ref().unwrap().agents.clone()
}

#[test]
fn schedule_presets_build_cron_and_read_back() {
    use model::{parse_time, preset_cron, preset_of, Preset};
    assert_eq!(parse_time("09:00"), Some((9, 0)));
    assert_eq!(parse_time(" 9:5 "), Some((9, 5)));
    assert_eq!(parse_time("21.15"), Some((21, 15)));
    assert_eq!(parse_time("7"), Some((7, 0)));
    for bad in ["24:00", "9:60", "nine", "", "9:"] {
        assert_eq!(parse_time(bad), None, "{bad:?}");
    }
    assert_eq!(preset_cron(Preset::Hourly, "", 0, "", ""), Ok("0 * * * *".into()));
    assert_eq!(preset_cron(Preset::Daily, "09:30", 0, "", ""), Ok("30 9 * * *".into()));
    assert_eq!(preset_cron(Preset::Weekdays, "7:05", 0, "", ""), Ok("5 7 * * 1-5".into()));
    assert_eq!(preset_cron(Preset::Weekly, "18:00", 5, "", ""), Ok("0 18 * * 5".into()));
    assert_eq!(preset_cron(Preset::Monthly, "02:00", 0, " 15 ", ""), Ok("0 2 15 * *".into()));
    assert!(preset_cron(Preset::Monthly, "02:00", 0, "32", "").is_err());
    assert!(preset_cron(Preset::Monthly, "02:00", 0, "0", "").is_err());
    assert!(preset_cron(Preset::Daily, "25:00", 0, "", "").is_err());
    assert_eq!(preset_cron(Preset::Custom, "", 0, "", "  0  9 * *   1-5 "), Ok("0 9 * * 1-5".into()), "spaces tidied");
    assert!(preset_cron(Preset::Custom, "", 0, "", "  ").is_err());
    // An existing cron opens as the preset that makes it; anything else as Custom.
    for (p, time, wd, day) in [(Preset::Daily, "09:30", 1, "1"), (Preset::Weekdays, "07:05", 1, "1"), (Preset::Weekly, "18:00", 5, "1"), (Preset::Monthly, "02:00", 1, "15")] {
        let cron = preset_cron(p, time, wd, day, "").unwrap();
        assert_eq!(preset_of(&cron), (p, time.to_string(), wd, day.to_string()), "{cron}");
    }
    assert_eq!(preset_of("0 * * * *").0, Preset::Hourly);
    assert_eq!(preset_of("0 9 * * 7"), (Preset::Weekly, "09:00".into(), 0, "1".into()), "7 is Sunday too");
    for other in ["*/15 * * * *", "@daily", "0 9 1 6 *", "0 9 * * 1,3", "0 9-17 * * *", "nonsense"] {
        assert_eq!(preset_of(other).0, Preset::Custom, "{other}");
    }
}

#[test]
fn the_automation_editor_checks_its_fields_and_builds_params() {
    use model::{draft_params, TriggerKind};
    let mut app = demo("automations");
    app.new_automation();
    assert!(app.auto_open);
    // The first folder and its default login.
    assert_eq!(app.draft.workspace.as_deref(), Some("ws_relay"));
    assert_eq!(app.draft.login.as_deref(), Some("prof_work"));
    app.save_automation();
    assert_eq!(app.draft.error.as_deref(), Some("Give the automation a name."));
    app.draft.name.replace_text("Nightly");
    app.save_automation();
    assert_eq!(app.draft.error.as_deref(), Some("Say what each run should do."));
    // An example fills the prompt and keeps the typed name.
    app.draft_use_example(0);
    assert_eq!(app.draft.prompt.text, SCHEDULE_EXAMPLES[0].1);
    assert_eq!(app.draft.name.text, "Nightly");
    app.draft.max_minutes.replace_text("721");
    app.save_automation();
    assert!(app.draft.error.as_deref().is_some_and(|e| e.contains("1 to 720")), "{:?}", app.draft.error);
    app.draft.max_minutes.replace_text("90");
    app.draft_toggle_approve("git_push");
    app.draft_toggle_approve("create_pull_request");
    let p = draft_params(&app.draft, &agents_of(&app)).unwrap();
    assert_eq!(p.trigger, AutomationTrigger::Schedule { cron: "0 9 * * *".into() });
    assert_eq!((p.agent, p.profile_id.as_deref(), p.max_minutes), (AgentKind::Claude, Some("prof_work"), Some(90)));
    assert_eq!(p.auto_approve, vec!["create_pull_request", "git_push"], "in the host's order");
    assert_eq!(p.webhook_secret, None);
    // A webhook: its secret goes, and filters are field = value lines.
    app.draft_set_trigger(TriggerKind::Webhook);
    app.draft.secret.replace_text("  s3cret ");
    app.draft.filters.replace_text("payload.action = opened\n\nnot a rule");
    assert!(draft_params(&app.draft, &agents_of(&app)).unwrap_err().contains("not a rule"));
    app.draft.filters.replace_text("payload.action = opened\n headers.x-github-event=issues ");
    let p = draft_params(&app.draft, &agents_of(&app)).unwrap();
    let AutomationTrigger::Webhook { source, filter, dedupe_key, cooldown_minutes } = &p.trigger else { panic!("{:?}", p.trigger) };
    assert_eq!((*source, dedupe_key, *cooldown_minutes), (WebhookSource::Sentry, &None, 1440));
    assert_eq!(filter, &vec![WebhookMatch { path: "payload.action".into(), equals: "opened".into() }, WebhookMatch { path: "headers.x-github-event".into(), equals: "issues".into() }]);
    assert_eq!(p.webhook_secret.as_deref(), Some("s3cret"));
    // By hand: the webhook's prompt goes, and so does the secret typed for it.
    app.draft_set_trigger(TriggerKind::Manual);
    assert_eq!(app.draft.prompt.text, "");
    app.draft.prompt.replace_text("Summarise this week's commits.");
    let p = draft_params(&app.draft, &agents_of(&app)).unwrap();
    assert_eq!((p.trigger, p.webhook_secret), (AutomationTrigger::Manual, None));
}

#[test]
fn a_new_webhook_gets_its_senders_prompt_until_you_write_your_own() {
    use model::TriggerKind;
    let mut app = demo("automations");
    app.new_automation();
    app.draft_set_trigger(TriggerKind::Webhook);
    assert_eq!(app.draft.prompt.text, SENTRY_PROMPT);
    app.draft_set_source(WebhookSource::Github);
    assert_eq!(app.draft.prompt.text, GITHUB_PROMPT);
    // Back to a schedule: the webhook prompt goes (its {{fields}} mean nothing there).
    app.draft_set_trigger(TriggerKind::Schedule);
    assert_eq!(app.draft.prompt.text, "");
    // An example counts as untouched too.
    app.draft_use_example(1);
    app.draft_set_trigger(TriggerKind::Webhook);
    assert_eq!(app.draft.prompt.text, GITHUB_PROMPT);
    // Words of your own stay.
    app.draft.prompt.replace_text("Triage the issue and label it.");
    app.draft_set_source(WebhookSource::Generic);
    app.draft_set_trigger(TriggerKind::Schedule);
    assert_eq!(app.draft.prompt.text, "Triage the issue and label it.");
    // Editing an existing automation never rewrites its prompt.
    app.edit_automation("auto_sentry");
    app.draft_set_source(WebhookSource::Github);
    assert_eq!(app.draft.prompt.text, SENTRY_PROMPT);
}

#[test]
fn an_update_sends_only_what_changed() {
    use model::{draft_params, update_diff};
    let mut app = demo("automations");
    let a = app.automation("auto_deps").unwrap().clone();
    app.edit_automation("auto_deps");
    assert_eq!(app.draft.preset, model::Preset::Daily);
    assert_eq!(app.draft.time.text, "03:00");
    let none = AutomationUpdateParams { automation_id: "auto_deps".into(), ..Default::default() };
    assert_eq!(update_diff(&a, &draft_params(&app.draft, &agents_of(&app)).unwrap(), false), none, "nothing changed");
    // Saving no change just closes the editor.
    app.save_automation();
    assert!(!app.auto_open && !app.draft.saving);
    // A new time and the agent's own login: the trigger and the profile, nothing else.
    app.edit_automation("auto_deps");
    app.draft.time.replace_text("04:15");
    app.draft.login = Some("claude".into());
    let u = update_diff(&a, &draft_params(&app.draft, &agents_of(&app)).unwrap(), false);
    let v = serde_json::to_value(&u).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["automation_id", "profile_id", "trigger"], "{v}");
    assert_eq!(u.trigger, Some(AutomationTrigger::Schedule { cron: "15 4 * * *".into() }));
    assert_eq!(u.profile_id.as_deref(), Some("claude"));
    // Another agent: its kind and its login go together.
    app.draft.login = Some("codex".into());
    let u = update_diff(&a, &draft_params(&app.draft, &agents_of(&app)).unwrap(), false);
    assert_eq!((u.agent, u.profile_id.as_deref()), (Some(AgentKind::Codex), Some("codex")));
    // The default login stored as no profile reads the same as its key.
    let weekly = app.automation("auto_tidy").unwrap().clone();
    app.edit_automation("auto_tidy");
    assert_eq!(app.draft.login.as_deref(), Some("claude"));
    assert_eq!(app.draft.preset, model::Preset::Weekly);
    assert!(!app.draft.enabled);
    let u = update_diff(&weekly, &draft_params(&app.draft, &agents_of(&app)).unwrap(), false);
    assert_eq!(u, AutomationUpdateParams { automation_id: "auto_tidy".into(), ..Default::default() });
    // The secret: blank keeps it, Remove clears it, a new one replaces it.
    let sentry = app.automation("auto_sentry").unwrap().clone();
    app.edit_automation("auto_sentry");
    assert!(!app.draft.advanced, "dedupe and filters stay folded when unused");
    let diff = |app: &model::App, remove| update_diff(&sentry, &draft_params(&app.draft, &agents_of(app)).unwrap(), remove);
    assert_eq!(diff(&app, false), AutomationUpdateParams { automation_id: "auto_sentry".into(), ..Default::default() });
    assert_eq!(diff(&app, true).webhook_secret.as_deref(), Some(""));
    app.draft.secret.replace_text("new-secret");
    assert_eq!(diff(&app, true).webhook_secret.as_deref(), Some("new-secret"));
    assert_eq!(diff(&app, false).trigger, None);
}

#[test]
fn saving_shows_the_hosts_refusal_in_the_editor() {
    let mut app = demo("automations");
    app.new_automation();
    app.draft.name.replace_text("Nightly");
    app.draft_use_example(0);
    app.save_automation();
    assert!(app.draft.saving && app.draft.error.is_none());
    let refused = agenthost_proto::jsonrpc::RpcError { code: -32602, message: "a webhook trigger needs the desktop".into(), data: None };
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::AutomationSaved, result: Err(refused) });
    assert!(app.auto_open, "the editor stays open to fix it");
    assert_eq!(app.draft.error.as_deref(), Some("a webhook trigger needs the desktop"));
    assert!(!app.draft.saving);
    // Saved: the editor closes on the new automation.
    let mut a = app.automation("auto_deps").unwrap().clone();
    a.automation_id = "auto_new".into();
    a.name = "Nightly".into();
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::AutomationSaved, result: Ok(serde_json::to_value(&a).unwrap()) });
    assert!(!app.auto_open);
    assert_eq!(app.screen, Screen::Automations);
    assert_eq!(app.selected_automation().map(|a| a.name.as_str()), Some("Nightly"));
    assert_eq!(app.automations.len(), 4);
}

#[test]
fn the_schedule_check_keeps_only_the_answer_for_whats_typed() {
    let mut app = demo("automations");
    app.new_automation();
    assert_eq!(app.draft.checked, "0 9 * * *");
    let reply = |cron: &str, text: &str| bridge::FromHost::Reply { tag: bridge::Tag::ScheduleCheck(cron.into()), result: Ok(serde_json::json!({"text": text, "next": [1, 2]})) };
    app.apply(reply("0 8 * * *", "Every day at 08:00"));
    assert!(app.draft.check.is_none(), "an answer for an older schedule is dropped");
    app.apply(reply("0 9 * * *", "Every day at 09:00"));
    assert_eq!(app.draft.check.as_ref().and_then(|c| c.text.as_deref()), Some("Every day at 09:00"));
    // A time that makes no cron says so at once, and Save won't go.
    app.draft.time.replace_text("25:00");
    app.recheck_schedule();
    assert!(app.draft.check.as_ref().and_then(|c| c.error.as_deref()).is_some_and(|e| e.contains("HH:MM")));
    app.draft.time.replace_text("10:00");
    app.recheck_schedule();
    assert_eq!(app.draft.checked, "0 10 * * *");
    assert!(app.draft.check.is_none(), "waiting for the host");
    // The host refuses a custom cron: its words show, and Save stops on them.
    app.draft_set_preset(model::Preset::Custom);
    assert_eq!(app.draft.cron.text, "0 10 * * *", "Custom starts from the preset's cron");
    app.draft.cron.replace_text("0 25 * * *");
    app.recheck_schedule();
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::ScheduleCheck("0 25 * * *".into()), result: Ok(serde_json::json!({"error": "hour 25 is out of range"})) });
    app.draft.name.replace_text("x");
    app.draft.prompt.replace_text("y");
    app.save_automation();
    assert_eq!(app.draft.error.as_deref(), Some("hour 25 is out of range"));
    assert!(!app.draft.saving);
}

#[test]
fn automation_changes_update_the_list_and_its_runs() {
    let mut app = demo("automations");
    assert!(app.can_run_now(app.automation("auto_deps").unwrap()));
    // A run starts: the row says so, its runs list leads with it, Run Now waits.
    let mut a = app.automation("auto_deps").unwrap().clone();
    let run = AutomationRun { run_id: "run_d4".into(), automation_id: "auto_deps".into(), session_id: Some("ses_r5".into()), trigger: RunTrigger::Manual, scheduled_for: None, started_at: crate::view::now_ms(), finished_at: None, outcome: RunOutcome::Running, summary: None, detail: None };
    a.last_run = Some(run.clone());
    app.apply(auto_note(AutomationChanged { automation_id: "auto_deps".into(), automation: Some(a.clone()), removed: false }));
    assert_eq!(app.runs["auto_deps"][0].run_id, "run_d4");
    assert_eq!(app.runs["auto_deps"].len(), 4);
    assert_eq!(app.automations_running(), 1);
    assert!(!app.can_run_now(app.automation("auto_deps").unwrap()));
    assert_eq!(crate::automations::summary(&app), "3 automations · 1 running");
    // It finishes: the same run, updated in place.
    let done = AutomationRun { outcome: RunOutcome::Succeeded, finished_at: Some(run.started_at + 60_000), ..run };
    a.last_run = Some(done);
    app.apply(auto_note(AutomationChanged { automation_id: "auto_deps".into(), automation: Some(a), removed: false }));
    assert_eq!(app.runs["auto_deps"].len(), 4);
    assert_eq!(app.runs["auto_deps"][0].outcome, RunOutcome::Succeeded);
    assert!(app.can_run_now(app.automation("auto_deps").unwrap()));
    // One made elsewhere (the CLI, a phone) joins the list.
    let mut other = app.automation("auto_tidy").unwrap().clone();
    other.automation_id = "auto_cli".into();
    app.apply(auto_note(AutomationChanged { automation_id: "auto_cli".into(), automation: Some(other), removed: false }));
    assert_eq!(app.automations.len(), 4);
    // Deleted elsewhere while you edit it: it goes, and so does the editor.
    app.edit_automation("auto_deps");
    app.apply(auto_note(AutomationChanged { automation_id: "auto_deps".into(), automation: None, removed: true }));
    assert!(app.automation("auto_deps").is_none() && !app.runs.contains_key("auto_deps"));
    assert!(!app.auto_open && app.notice.is_some());
    assert_eq!(app.selected_automation().map(|a| a.automation_id.as_str()), Some("auto_sentry"), "the next one shows");
}

#[test]
fn delete_and_new_url_ask_first() {
    let mut app = demo("automations-webhook");
    app.delete_automation("auto_sentry");
    assert_eq!(app.confirm_delete_auto.as_deref(), Some("auto_sentry"));
    assert!(app.automation("auto_sentry").is_some(), "nothing yet");
    app.delete_automation("auto_sentry");
    assert!(app.confirm_delete_auto.is_none() && app.automation("auto_sentry").is_none());
    let mut app = demo("automations-webhook");
    app.rotate_webhook("auto_sentry");
    assert_eq!(app.confirm_rotate.as_deref(), Some("auto_sentry"));
    app.rotate_webhook("auto_sentry");
    assert!(app.confirm_rotate.is_none());
}

#[test]
fn automation_rows_say_what_starts_them_and_when() {
    use crate::look::Look;
    use crate::view::when_label;
    let now = 1_800_000_000_000;
    assert_eq!(when_label(now + 30_000, now, Look::Mac), "now");
    assert_eq!(when_label(now + 5 * 60_000, now, Look::Mac), "in 5m");
    assert_eq!(when_label(now + (4 * 60 + 10) * 60_000, now, Look::Mac), "in 4h 10m");
    assert_eq!(when_label(now + 7 * 3_600_000, now, Look::Mac), "in 7h");
    assert_eq!(when_label(now - 2 * 3_600_000 - 5 * 60_000, now, Look::Mac), "2h ago");
    assert_eq!(when_label(now + 3 * 86_400_000, now, Look::Gnome), "in 3 d");
    assert_eq!(when_label(now - 19 * 3_600_000, now, Look::Gnome), "19 h ago");
    let app = demo("automations");
    let line = |id: &str| model::trigger_line(app.automation(id).unwrap());
    assert_eq!(line("auto_deps"), "Every day at 03:00");
    assert_eq!(line("auto_sentry"), "Sentry webhook");
    let mut a = app.automation("auto_sentry").unwrap().clone();
    a.trigger = AutomationTrigger::Webhook { source: WebhookSource::Generic, filter: vec![], dedupe_key: None, cooldown_minutes: 10 };
    assert_eq!(model::trigger_line(&a), "Webhook");
    a.trigger = AutomationTrigger::Manual;
    assert_eq!(model::trigger_line(&a), "By hand");
    a.trigger = AutomationTrigger::Schedule { cron: "0 9 * * 1-5".into() };
    a.schedule_text = None;
    assert_eq!(model::trigger_line(&a), "0 9 * * 1-5", "the cron when the host gave no words");
    // Runs are sessions; the sidebar knows which.
    assert!(app.sessions.iter().any(|x| x.automation_id.as_deref() == Some("auto_deps")));
}

#[test]
fn opening_a_run_opens_its_session() {
    let mut app = demo("automations");
    app.open_run("ses_r3");
    assert_eq!(app.screen, Screen::Session("ses_r3".into()));
    app.open_automations();
    assert_eq!(app.screen, Screen::Automations);
    assert_eq!(app.sel_auto.as_deref(), Some("auto_deps"), "back where you were");
    // An older host: no automations to show.
    let mut old = demo("automations");
    old.info.as_mut().unwrap().protocol_version = 4;
    assert!(!old.host_has_automations());
}

#[test]
fn every_automation_scene_builds() {
    for (name, _) in crate::demo::SCENES.iter().filter(|(n, _)| n.starts_with("automation")) {
        let app = demo(name);
        assert!(app.screen == Screen::Automations, "{name}");
    }
}

/// The Automations screens lay out in both looks (the headless runtime needs no GPU), and
/// clicking through them does what it says.
#[test]
fn automation_screens_lay_out_and_click_through() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 1800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    let scenes: [(&str, &[&str]); 10] = [
        ("automations", &["nav-automations", "automation-list", "auto-auto_deps", "auto-enabled-auto_deps", "automation-detail", "run-now", "edit-automation", "runs", "run-run_d3", "run-run_d1", "ses_r3"]),
        ("automations-webhook", &["webhook", "webhook-url", "copy-url", "rotate-url", "last-delivery"]),
        ("automations-empty", &["empty-new-automation"]),
        ("automation-new", &["automation-editor", "auto-name", "auto-ws", "auto-login", "auto-preset", "auto-time", "schedule-check", "schedule-examples", "auto-prompt", "approve-git_push", "auto-max", "save-automation"]),
        ("automation-new-cron-error", &["auto-cron", "schedule-error"]),
        ("automation-new-webhook", &["auto-source", "auto-secret", "editor-webhook-url", "auto-advanced"]),
        ("automation-edit-webhook", &["editor-copy-url", "editor-rotate-url", "secret-remove", "auto-dedupe", "auto-cooldown", "auto-filters", "delete-automation"]),
        ("automation-picker", &["auto-preset-menu"]),
        ("automation-confirm-delete", &["delete-alert", "confirm-delete-alert"]),
        ("automation-confirm-rotate", &["rotate-alert", "confirm-rotate-alert"]),
    ];
    for look in [Look::Mac, Look::Gnome] {
        for (scene, keys) in scenes {
            let ui = open(scene, look);
            for k in keys {
                assert!(ui.find(k).is_some(), "{look:?} {scene}: {k} is not on screen");
            }
        }
    }
    let state = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let mut ui = open("automations", Look::Gnome);
    // A run opens its session; the sidebar brings the screen back.
    click(&mut ui, "run-run_d3");
    assert!(state(&mut ui, &|s| s.screen == Screen::Session("ses_r3".into())));
    click(&mut ui, "nav-automations");
    assert!(state(&mut ui, &|s| s.screen == Screen::Automations));
    click(&mut ui, "auto-auto_sentry");
    assert!(state(&mut ui, &|s| s.sel_auto.as_deref() == Some("auto_sentry")));
    assert!(ui.find("webhook-url").is_some());
    // New Automation, a webhook: Sentry's prompt; GitHub's when the sender changes.
    click(&mut ui, "new-automation");
    assert!(state(&mut ui, &|s| s.auto_open && s.draft.id.is_none()));
    click(&mut ui, "auto-trigger-webhook");
    assert!(state(&mut ui, &|s| s.draft.trigger == model::TriggerKind::Webhook && s.draft.prompt.text == SENTRY_PROMPT));
    click(&mut ui, "auto-source-github");
    assert!(state(&mut ui, &|s| s.draft.prompt.text == GITHUB_PROMPT));
    // The in-app picker (no native menus here) opens under its button and picks.
    click(&mut ui, "auto-trigger-schedule");
    if !rune::popup_menu_supported() {
        click(&mut ui, "auto-preset");
        assert!(ui.find("auto-preset-menu").is_some());
    }
    ui.update(|s, _| s.draft_set_preset(model::Preset::Weekly));
    ui.frame(0.016);
    assert!(ui.find("auto-weekday").is_some() && ui.find("auto-time").is_some());
    // Saving with no name says so in the editor.
    click(&mut ui, "save-automation");
    assert!(state(&mut ui, &|s| s.auto_open && s.draft.error.as_deref() == Some("Give the automation a name.")));
    assert!(ui.find("auto-error").is_some());
    click(&mut ui, "cancel-automation");
    assert!(state(&mut ui, &|s| !s.auto_open));
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

#[test]
fn sent_images_are_fetched_once_and_let_go_with_their_session() {
    let mut app = demo("sent-images");
    app.images.clear();
    app.fetch_images();
    let mut ids: Vec<&String> = app.images.keys().collect();
    ids.sort();
    assert_eq!(ids, ["att_s1", "att_s2"], "images only, not the log file");
    assert!(app.images.values().all(|i| matches!(i, model::SentImage::Loading)));
    // An answer decodes; a failure says why and is retried after a reconnect.
    app.image_cache = Some(rune::ImageCache::new(Arc::new(|| {})));
    app.apply(bridge::FromHost::AttachmentRead { attachment_id: "att_s1".into(), result: Ok(crate::brand::APP_ICON_PNG.to_vec()) });
    app.apply(bridge::FromHost::AttachmentRead { attachment_id: "att_s2".into(), result: Err("no such attachment".into()) });
    let model::SentImage::Ready(img) = &app.images["att_s1"] else { panic!("not decoded") };
    assert_eq!(img.wait().ok(), Some([512, 512]));
    assert!(matches!(&app.images["att_s2"], model::SentImage::Failed(e) if e == "no such attachment"));
    // A late answer for an image no longer wanted is dropped.
    app.apply(bridge::FromHost::AttachmentRead { attachment_id: "att_gone".into(), result: Ok(vec![]) });
    assert!(!app.images.contains_key("att_gone"));
    // Older hosts don't keep images: nothing is asked for.
    let mut old = demo("sent-images");
    old.images.clear();
    old.info.as_mut().unwrap().protocol_version = 6;
    old.fetch_images();
    assert!(old.images.is_empty());
    // Another session: its images only.
    app.open_session("ses_2");
    app.fetch_images();
    assert!(app.images.is_empty());
}

/// A sent screenshot shows as a picture; clicking it opens the viewer, Escape closes it.
#[test]
fn a_sent_image_opens_in_the_viewer() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 900.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "sent-images").unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
            for i in s.images.values() {
                if let model::SentImage::Ready(img) = i {
                    let _ = img.wait();
                }
            }
        });
        ui.frame(0.016);
        ui.frame(0.016);
        for k in ["img-att_s1", "img-att_s2"] {
            assert!(ui.find(k).is_some(), "{look:?}: {k} is not on screen");
        }
        assert!(ui.find("viewer").is_none());
        let (x, y) = ui.find("img-att_s1").unwrap();
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(ui.find("viewer-image").is_some(), "{look:?}: the viewer did not open");
        ui.key(rune::KeyEvent { key: rune::Key::Escape, mods: rune::Mods::default() });
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(ui.find("viewer").is_none(), "{look:?}: Escape did not close the viewer");
    }
}

/// Files an agent sent show in its reply, outside the folded tool calls: the recording as
/// its still, the screenshot as a picture, the CSV as a chip. Clicking the recording fetches
/// it from the host for that session; a second click while it comes does nothing more.
#[test]
fn agent_files_show_in_the_reply_and_a_recording_downloads() {
    use crate::agentfile::Download;
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let read = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 1800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "agent-files").unwrap().1;
        let (fake, from_host, mut to_host) = bridge::Bridge::fake();
        ui.update(move |s, _| {
            f(s);
            s.look = look;
            s.bridge = Some(fake);
            for i in s.images.values() {
                if let model::SentImage::Ready(img) = i {
                    let _ = img.wait();
                }
            }
        });
        ui.frame(0.016);
        ui.frame(0.016);
        for k in ["agent-file-att_rec", "agent-file-open-att_rec", "agent-file-att_shot", "agent-file-open-att_shot", "agent-file-att_csv", "agent-file-open-att_csv"] {
            assert!(ui.find(k).is_some(), "{look:?}: {k} is not on screen");
        }
        assert!(ui.find("agent-file-status-att_rec").is_none(), "{look:?}: nothing to say before a click");

        click(&mut ui, "agent-file-open-att_rec");
        let mut asked = vec![];
        while let Ok(cmd) = to_host.try_recv() {
            if let bridge::ToHost::Download { session_id, attachment, dir } = cmd {
                asked.push((session_id, attachment.attachment_id, attachment.size, dir.ends_with("AgentOS")));
            }
        }
        assert_eq!(asked, [("ses_1".to_string(), "att_rec".to_string(), 4_812_331, true)], "{look:?}");
        assert!(read(&mut ui, &|s| matches!(s.downloads.get("att_rec"), Some(Download::Fetching { done: 0, .. }))), "{look:?}");
        // Progress shows under it as the pieces arrive.
        from_host.send(bridge::FromHost::DownloadProgress { attachment_id: "att_rec".into(), done: 524_288, size: 4_812_331 }).unwrap();
        for _ in 0..3 {
            ui.frame(0.016);
        }
        assert!(ui.find("agent-file-status-att_rec").is_some(), "{look:?}: no progress");
        assert!(read(&mut ui, &|s| matches!(s.downloads.get("att_rec"), Some(Download::Fetching { done: 524_288, .. }))), "{look:?}");
        click(&mut ui, "agent-file-open-att_rec");
        assert!(to_host.try_recv().is_err(), "{look:?}: a second click while it downloads asks again");
    }
}

/// A saved file opens as is the next time; a script is only shown in its folder; a failure
/// says why and the next click tries again.
#[test]
fn agent_files_open_once_saved() {
    use crate::agentfile::{Download, Opening};
    let mut app = demo("agent-files");
    let (fake, _from_host, mut to_host) = bridge::Bridge::fake();
    app.bridge = Some(fake);
    let att = |id: &str, name: &str| Attachment { attachment_id: id.into(), name: name.into(), mime: guess_mime(name).into(), size: 3 };
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new().prefix("agentfiles").tempdir_in(&base).unwrap();
    let saved = crate::agentfile::save_unique(dir.path(), "flaky-runs.csv", b"a,b").unwrap();

    app.open_agent_file("ses_1", &att("att_csv", "flaky-runs.csv"));
    assert!(matches!(to_host.try_recv(), Ok(bridge::ToHost::Download { .. })));
    app.apply(bridge::FromHost::Downloaded { attachment_id: "att_csv".into(), result: Ok(saved.clone()) });
    assert_eq!(app.downloads["att_csv"], Download::Saved(saved.clone()));
    assert_eq!(std::mem::take(&mut app.open_files), [Opening::Open(saved.clone())]);
    // Again: the saved copy, not another download.
    app.open_agent_file("ses_1", &att("att_csv", "flaky-runs.csv"));
    assert!(to_host.try_recv().is_err());
    assert_eq!(std::mem::take(&mut app.open_files), [Opening::Open(saved.clone())]);
    // Deleted since: fetched again.
    std::fs::remove_file(&saved).unwrap();
    app.open_agent_file("ses_1", &att("att_csv", "flaky-runs.csv"));
    assert!(matches!(to_host.try_recv(), Ok(bridge::ToHost::Download { .. })));

    // A script is saved but only shown in its folder.
    let script = crate::agentfile::save_unique(dir.path(), "fix.sh", b"rm").unwrap();
    app.open_agent_file("ses_1", &att("att_sh", "fix.sh"));
    app.apply(bridge::FromHost::Downloaded { attachment_id: "att_sh".into(), result: Ok(script.clone()) });
    assert_eq!(std::mem::take(&mut app.open_files), [Opening::Reveal(script)]);
    assert!(app.notice.as_deref().is_some_and(|n| n.contains("saved but not opened")));

    // A failure: the error, then a click asks again.
    app.open_agent_file("ses_1", &att("att_rec", "Sign-up flow.mp4"));
    app.apply(bridge::FromHost::Downloaded { attachment_id: "att_rec".into(), result: Err("the connection to the host closed".into()) });
    assert!(matches!(&app.downloads["att_rec"], Download::Failed(_)));
    assert_eq!(app.error.as_deref(), Some("Couldn't download Sign-up flow.mp4: the connection to the host closed"));
    while to_host.try_recv().is_ok() {}
    app.open_agent_file("ses_1", &att("att_rec", "Sign-up flow.mp4"));
    assert!(matches!(to_host.try_recv(), Ok(bridge::ToHost::Download { .. })));
    // An answer nobody asked for (from a host since left) is dropped.
    app.apply(bridge::FromHost::Downloaded { attachment_id: "att_other".into(), result: Ok(dir.path().join("x")) });
    assert!(!app.downloads.contains_key("att_other") && app.open_files.is_empty());
}

/// The transcript asks for the pictures agents sent and their videos' stills, not the
/// videos or other files themselves.
#[test]
fn agent_file_pictures_are_fetched() {
    let mut app = demo("agent-files");
    app.images.clear();
    app.fetch_images();
    let mut ids: Vec<&String> = app.images.keys().collect();
    ids.sort();
    assert_eq!(ids, ["att_rec_poster", "att_shot"]);
}

/// What you type in a session stays with it while you look at another.
#[test]
fn each_session_keeps_its_unsent_message() {
    let mut app = demo("composer-pickers");
    app.open_session("ses_1");
    app.prompt.replace_text("half a thought");
    app.open_session("ses_2");
    assert_eq!(app.prompt.text, "", "another session starts empty");
    app.prompt.replace_text("for ses_2");
    app.screen = Screen::Automations;
    app.open_session("ses_1");
    assert_eq!(app.prompt.text, "half a thought");
    app.open_session("ses_2");
    assert_eq!(app.prompt.text, "for ses_2");
    // Sent: nothing comes back.
    app.send_prompt();
    app.open_session("ses_1");
    app.open_session("ses_2");
    assert_eq!(app.prompt.text, "");
}

// ---------------------------------------------------------------------------
// Remote access: the app as a paired device
// ---------------------------------------------------------------------------

/// Set env vars for one test and take them away at its end, whatever happens.
struct EnvGuard(Vec<&'static str>);
impl EnvGuard {
    fn new(vars: Vec<(&'static str, String)>) -> Self {
        for (k, v) in &vars {
            std::env::set_var(k, v);
        }
        EnvGuard(vars.into_iter().map(|(k, _)| k).collect())
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for k in &self.0 {
            std::env::remove_var(k);
        }
    }
}

/// The pairing payload a host's `agenthost device pair` shows, once its relay
/// is up (fetched the way the host's own CLI does).
fn pair_payload(rt: &tokio::runtime::Runtime, socket: &std::path::Path) -> String {
    rt.block_on(async {
        loop {
            let (c, _) = agenthost_cli::Client::connect(socket).await.unwrap();
            let info: HostInfo = c.call(m::HOST_INFO, json!({})).await.unwrap();
            if !info.relay.connected {
                drop(c);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            let r: PairStartResult = c.call(m::DEVICE_PAIR_START, json!({})).await.unwrap();
            return r.qr_payload;
        }
    })
}

/// Accepts a pairing on the host side, as someone at its keyboard would.
fn accept_pairing(rt: &tokio::runtime::Runtime, socket: std::path::PathBuf) {
    rt.spawn(async move {
        let (c, mut notes) = agenthost_cli::Client::connect(&socket).await.unwrap();
        while let Some(n) = notes.recv().await {
            if n.method == m::N_PAIRING_PENDING {
                let pid = n.params["pairing_id"].as_str().unwrap().to_string();
                c.request(m::DEVICE_PAIR_CONFIRM, json!({"pairing_id": pid, "accept": true})).await.unwrap();
                break;
            }
        }
    });
}

/// The pair request the form is waiting on.
fn pair_attempt(app: &model::App) -> u64 {
    app.pair_remote.as_ref().map_or(0, |p| p.attempt)
}

/// A paired host's keys, as the pairing handshake hands them over.
fn sample_state() -> agenthost_cli::remote::DeviceState {
    agenthost_cli::remote::DeviceState {
        relay_url: "wss://relay.example.com".into(),
        relay_id: "relay_x".into(),
        host_key: "aG9zdA==".into(),
        host_name: "workstation".into(),
        device_id: "dev_1".into(),
        host_id: "host_1".into(),
        noise_private: "AAAA".into(),
        relay_private: "AAAA".into(),
    }
}

/// The pairing form's round trip, with no host needed: the code to compare
/// shows while the host decides, the keys are kept on accept, and a laptop
/// with nothing local of its own switches at once — and remembers it.
#[test]
fn remote_pairing_result_switches_a_laptop_with_no_local_host() {
    // The env vars below are the whole process's: no other test may start a
    // bridge (which reads them) meanwhile.
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahmp").tempdir_in("/tmp").unwrap();
    // A laptop that has remembered nothing yet: no config folder exists, and
    // the keys are the first thing kept in it.
    let cfg = dir.path().join("cfg");
    let _env = EnvGuard::new(vec![
        ("AGENTHOST_DESKTOP_DIR", cfg.to_string_lossy().into_owned()),
        ("AGENTHOST_REMOTE", cfg.join("remote.json").to_string_lossy().into_owned()),
    ]);
    let mut app = model::init();
    app.conn = Conn::Down("no host here".into());

    // A stray code with no form open is dropped.
    app.apply(bridge::FromHost::PairCode { attempt: 0, code: "314 159".into() });
    assert!(app.pair_remote.is_none());

    app.start_remote_pair();
    assert!(app.pair_remote.as_ref().is_some_and(|p| p.payload.focused), "the form asks for the payload first");
    app.pair_remote.as_mut().unwrap().payload.replace_text("agenthost://pair?…");
    app.send_remote_pair();
    assert!(app.pair_remote.as_ref().is_some_and(|p| p.busy));
    let first = pair_attempt(&app);
    app.apply(bridge::FromHost::PairCode { attempt: first, code: "314 159".into() });
    assert!(app.pair_remote.as_ref().is_some_and(|p| p.code.as_deref() == Some("314 159")));

    // Closing the form gives that request up: a late answer to it keeps no
    // keys and switches nothing, with the form closed or open on a new try.
    app.cancel_remote_pair();
    app.apply(bridge::FromHost::PairResult { attempt: first, result: Ok(sample_state()) });
    assert!(app.remote_host.is_none() && !bridge::remote_state_path().exists(), "a cancelled pairing keeps nothing");
    assert!(matches!(app.conn, Conn::Down(_)) && !crate::prefs::load().prefer_remote);
    app.start_remote_pair();
    app.pair_remote.as_mut().unwrap().payload.replace_text("agenthost://pair?…");
    app.send_remote_pair();
    app.apply(bridge::FromHost::PairCode { attempt: first, code: "271 828".into() });
    app.apply(bridge::FromHost::PairResult { attempt: first, result: Err("too late".into()) });
    assert!(app.pair_remote.as_ref().is_some_and(|p| p.busy && p.code.is_none() && p.error.is_none()), "the new try is not answered by the old one");

    // The host accepts.
    app.apply(bridge::FromHost::PairResult { attempt: pair_attempt(&app), result: Ok(sample_state()) });
    assert!(app.pair_remote.is_none(), "the form closes on success");
    assert_eq!(app.remote_host.as_deref(), Some("workstation"));
    let saved = agenthost_cli::remote::DeviceState::load(&bridge::remote_state_path()).unwrap();
    assert_eq!((saved.host_name.as_str(), saved.device_id.as_str()), ("workstation", "dev_1"));
    assert!(app.conn == Conn::Connecting, "a laptop with no host of its own goes there at once");
    assert!(crate::prefs::load().prefer_remote, "and comes back there next launch");
    assert!(app.notice.as_deref().is_some_and(|n| n.contains("Paired with workstation")));

    // The bridge says where it is headed before it gets there, so a paired
    // host that is asleep is still shown as the paired host, read-only.
    app.apply(bridge::FromHost::Link { remote: true, switched: true });
    assert!(app.link_remote && !app.admin);
    app.apply(bridge::FromHost::Disconnected("the host is offline (code 4404)".into()));
    assert!(app.link_remote, "being down does not change which host this is");
    app.apply(bridge::FromHost::Link { remote: false, switched: true });
    assert!(!app.link_remote && app.admin);
}

/// The same result on a computer whose own host is in use: nothing switches,
/// and the next launch is still this computer's.
#[test]
fn remote_pairing_result_leaves_a_live_local_host_alone() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahmq").tempdir_in("/tmp").unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    let _env = EnvGuard::new(vec![
        ("AGENTHOST_DESKTOP_DIR", cfg.to_string_lossy().into_owned()),
        ("AGENTHOST_REMOTE", dir.path().join("remote.json").to_string_lossy().into_owned()),
    ]);
    let mut app = model::init();
    app.conn = Conn::Connected;

    app.start_remote_pair();
    app.pair_remote.as_mut().unwrap().payload.replace_text("agenthost://pair?…");
    app.send_remote_pair();
    app.apply(bridge::FromHost::PairResult { attempt: pair_attempt(&app), result: Ok(sample_state()) });

    // Still on the local host; the keys are kept but nothing is hijacked.
    assert!(app.conn == Conn::Connected);
    assert!(!crate::prefs::load().prefer_remote, "the next launch is not taken over");
    assert!(agenthost_cli::remote::DeviceState::load(&bridge::remote_state_path()).is_ok());
    assert!(app.notice.as_deref().is_some_and(|n| n.contains("Settings")), "the notice points at the switch");

    // A refusal instead: the form stays open, says why, and can try again.
    let mut app2 = model::init();
    app2.conn = Conn::Down("no host here".into());
    app2.start_remote_pair();
    app2.pair_remote.as_mut().unwrap().payload.replace_text("agenthost://pair?…");
    app2.send_remote_pair();
    app2.apply(bridge::FromHost::PairResult { attempt: pair_attempt(&app2), result: Err("pairing refused: not today".into()) });
    assert!(app2.pair_remote.as_ref().is_some_and(|p| !p.busy && p.error.as_deref() == Some("pairing refused: not today")));
    assert!(app2.remote_host.is_none(), "no keys were kept for a refused pairing");
}

/// A laptop with no local host pairs with a remote one through the dev relay
/// (paste the code, compare the 6 digits, the host accepts), connects by
/// itself, runs a session over the tunnel, and cannot manage the host: a
/// paired device gets sessions and approvals, never policy. Revoking it there
/// ends the connection for good.
#[test]
fn remote_pairing_connects_runs_and_stays_read_only() {    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahr").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay = rt.block_on(agenthost_host::relay_dev::DevRelay::start("127.0.0.1:0")).unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), Some(relay.url()));

    // The app's own files (the paired keys, its preferences) stay in the test
    // dir, and there is no local host: a socket file nothing answers, so the
    // app doesn't start a daemon of its own.
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    let _env = EnvGuard::new(vec![("AGENTHOST_DESKTOP_DIR", cfg.to_string_lossy().into_owned())]);
    let dead = dir.path().join("dead.sock");
    std::fs::write(&dead, b"").unwrap();
    std::env::set_var("AGENTHOST_SOCKET", &dead);
    let mut app = model::init();
    app.bridge = Some(bridge::start(Arc::new(|| {}), Default::default()));
    pump(&mut app, |a| matches!(a.conn, Conn::Down(_)));

    let qr = pair_payload(&rt, &socket);
    accept_pairing(&rt, socket.clone());
    app.start_remote_pair();
    assert!(app.pair_remote.as_ref().is_some_and(|p| p.name.text.starts_with("Desktop")), "the form names this app");
    app.pair_remote.as_mut().unwrap().payload.replace_text(&qr);
    app.send_remote_pair();
    pump(&mut app, |a| {
        // The code to compare shows while the host waits; then the keys arrive.
        a.pair_remote.is_none() && a.remote_host.is_some()
    });

    // Nothing local was in use, so the app went there by itself — and will
    // next launch too. (Wait for the host's workspaces as well: the quick
    // task below starts in one.)
    pump(&mut app, |a| a.conn == Conn::Connected && a.link_remote && !a.admin && a.info.is_some() && !a.workspaces.is_empty());
    assert!(crate::prefs::load().prefer_remote, "a laptop with no host of its own comes back to the paired one");
    let state = agenthost_cli::remote::DeviceState::load(&bridge::remote_state_path()).unwrap();
    assert_eq!(state.host_name, app.remote_host.clone().unwrap());

    // A session runs over the tunnel like over the socket.
    let mut tx = rune::Tx::default();
    commands::run(&mut app, Cmd::QuickTask, &mut tx);
    app.new_agent = Some("fake".into());
    app.start_from_sheet();
    pump(&mut app, |a| matches!(&a.screen, Screen::Session(id) if a.session(id).is_some_and(|s| s.state == SessionState::Idle)));
    let Screen::Session(sid) = app.screen.clone() else { unreachable!() };
    app.prompt.replace_text("hello from the laptop");
    app.send_prompt();
    pump(&mut app, |a| {
        a.transcripts.get(&sid).is_some_and(|t| t.events.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: hello from the laptop"))
    });

    // Managing the host is refused — by the host, whatever the UI allows.
    app.error = None;
    app.act("Add workspace", m::WORKSPACE_ADD, json!({"path": dir.path().join("nope")}), bridge::Refresh::None);
    pump(&mut app, |a| a.error.as_deref().is_some_and(|e| e.contains("may not call workspace.add")));

    // Revoked on the host: the tunnel says why, and no retry brings it back.
    let device_id = agenthost_cli::remote::DeviceState::load(&bridge::remote_state_path()).unwrap().device_id;
    rt.block_on(async {
        let (c, _) = agenthost_cli::Client::connect(&socket).await.unwrap();
        c.request(m::DEVICE_REVOKE, json!({"device_id": device_id})).await.unwrap();
    });
    pump(&mut app, |a| matches!(&a.conn, Conn::Down(e) if e.contains("removed")));
    std::thread::sleep(Duration::from_millis(700));
    app.drain();
    assert!(matches!(app.conn, Conn::Down(_)), "a removed device is not retried");
}

/// Paired from a computer whose own host is in use: nothing switches by
/// itself; "Connect to <host>" is the choice, remembered for next launch, and
/// "Use this computer" is the way back.
#[test]
fn pairing_with_a_local_host_leaves_the_switch_to_you() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::Builder::new().prefix("ahpr").tempdir_in("/tmp").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay = rt.block_on(agenthost_host::relay_dev::DevRelay::start("127.0.0.1:0")).unwrap();
    let (_host, socket) = start_host(&rt, dir.path(), Some(relay.url()));
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    let _env = EnvGuard::new(vec![("AGENTHOST_DESKTOP_DIR", cfg.to_string_lossy().into_owned())]);
    let mut app = connect(&socket);

    let qr = pair_payload(&rt, &socket);
    accept_pairing(&rt, socket.clone());
    app.start_remote_pair();
    app.pair_remote.as_mut().unwrap().payload.replace_text(&qr);
    app.send_remote_pair();
    pump(&mut app, |a| a.pair_remote.is_none() && a.remote_host.is_some());

    // Still on the local host, which keeps its rights: the switch is a choice.
    assert!(app.conn == Conn::Connected && !app.link_remote && app.admin);
    assert!(!crate::prefs::load().prefer_remote, "nothing hijacks the next launch");
    assert!(app.notice.as_deref().is_some_and(|n| n.contains("Settings")), "the notice points at the switch");

    // The choice, once made, is remembered.
    app.connect_remote();
    pump(&mut app, |a| a.conn == Conn::Connected && a.link_remote && !a.admin);
    assert!(crate::prefs::load().prefer_remote);

    // And back again, by choice too.
    app.use_local_host();
    pump(&mut app, |a| a.conn == Conn::Connected && !a.link_remote && a.admin);
    assert!(!crate::prefs::load().prefer_remote);
}

/// The compact sidebar's model: Pinned first (in the order pinned), groups of the newest few
/// with "Show N more", collapsed groups out of the walking order, and search showing all.
#[test]
fn compact_sidebar_pins_collapses_and_shows_more() {
    let mut app = model::init();
    let scene = crate::demo::SCENES.iter().find(|(n, _)| *n == "sidebar").unwrap().1;
    scene(&mut app);
    let ids = |l: Vec<&agenthost_proto::Session>| l.into_iter().map(|x| x.session_id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(app.pinned_sessions()), ["ses_3"]);
    let relay = || app.sidebar_groups().into_iter().find(|(w, _)| w.workspace_id == "ws_relay").map(|(_, l)| l.len()).unwrap();
    assert_eq!(relay(), 12, "the pinned one leaves its group");
    let group = |a: &model::App| {
        let groups = a.sidebar_groups();
        let (_, l) = groups.iter().find(|(w, _)| w.workspace_id == "ws_relay").unwrap();
        let (rows, hidden) = a.group_rows("ws_relay", l);
        (rows.len(), hidden)
    };
    assert_eq!(group(&app), (model::GROUP_SHOWN, 4));
    // Pinned sessions come first in the order and the numbers.
    assert_eq!(app.sidebar_order()[0], "ses_3");
    assert_eq!(app.numbered()[0], "ses_3");
    // A session that's working always shows, however old.
    app.sessions.iter_mut().find(|x| x.session_id == "ses_s11").unwrap().state = SessionState::Running;
    assert_eq!(group(&app), (model::GROUP_SHOWN + 1, 3));
    app.show_all.insert("ws_relay".into());
    assert_eq!(group(&app), (12, 0));
    // Collapsed: no rows, nothing to walk or number; a search shows the matches anyway.
    app.toggle_group("ws_relay");
    assert_eq!(group(&app), (0, 0));
    assert!(!app.sidebar_order().contains(&"ses_1".to_string()));
    app.search.replace_text("oauth");
    assert_eq!(app.sidebar_order(), ["ses_2"]);
    app.search.replace_text("");
    app.toggle_group("ws_relay");
    assert!(app.sidebar_order().contains(&"ses_1".to_string()));
    // Pinning another puts it after the first; unpinning sends it home.
    app.set_pinned("ses_1", true);
    assert_eq!(ids(app.pinned_sessions()), ["ses_3", "ses_1"]);
    app.set_pinned("ses_3", false);
    assert_eq!(ids(app.pinned_sessions()), ["ses_1"]);
    assert!(app.sidebar_groups().iter().any(|(_, l)| l.iter().any(|x| x.session_id == "ses_3")));
    // Escape in the search field clears it, then closes it.
    app.open_search();
    app.search.replace_text("x");
    app.close_search();
    assert!(app.search_open && app.search.text.is_empty());
    app.close_search();
    assert!(!app.search_open);
    // A host before protocol 8 can't pin.
    assert!(app.has_pins());
    app.info.as_mut().unwrap().protocol_version = 7;
    assert!(!app.has_pins());
}

/// In the New Session sheet ⌘1…9 (Ctrl+1…9) start a session in the workspace on that row,
/// as its default login (else the login picked), and each row shows its key. No session is
/// jumped to; with no login to start as, the row is picked and the sheet stays.
#[test]
fn the_sheets_numbers_start_in_its_workspaces() {
    use crate::look::Look;
    use rune::{Key, KeyEvent, Mods};
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let none = Mods::default();
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "new").unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
        });
        ui.frame(0.016);
        ui.frame(0.016);
        let read = |ui: &mut rune::Runtime<model::App>| {
            let r = std::rc::Rc::new(std::cell::RefCell::new((vec![], None, None, false, Screen::Home)));
            let r2 = r.clone();
            ui.update_if(move |s, _| {
                *r2.borrow_mut() = (s.sheet_workspaces().iter().map(|w| w.workspace_id.clone()).collect::<Vec<_>>(), s.new_ws.clone(), s.new_agent.clone(), s.new_open, s.screen.clone());
                false
            });
            let got = r.borrow().clone();
            got
        };
        let (order, _, _, _, screen) = read(&mut ui);
        assert!(order.len() >= 3 && order.len() < 9, "{look:?}: the scene has a few workspaces");
        for n in 1..=order.len() {
            assert!(ui.find(&format!("pick-key-{n}")).is_some(), "{look:?}: row {n} shows its key");
        }
        assert!(ui.find(&format!("pick-key-{}", order.len() + 1)).is_none(), "{look:?}");
        // The Mac look's ⌘ is the logo key on this (Linux) build.
        let press = |ui: &mut rune::Runtime<model::App>, n: usize| {
            let c = char::from_digit(n as u32, 10).unwrap();
            let ev = if look == Look::Mac { KeyEvent { key: Key::Text(c.to_string()), mods: Mods { logo: true, ..none } } } else { KeyEvent { key: Key::Command(c), mods: Mods { cmd: true, ctrl: true, ..none } } };
            ui.key(ev);
            ui.frame(0.016);
            ui.frame(0.016);
        };
        let reopen = |ui: &mut rune::Runtime<model::App>| {
            ui.update(|s, _| s.open_new_session(false));
            ui.frame(0.016);
            ui.frame(0.016);
        };
        for n in [3, 1, 2] {
            // A number past the list does nothing.
            press(&mut ui, 9);
            assert!(read(&mut ui).3, "{look:?}: 9 has no row");
            press(&mut ui, n);
            let (_, ws, login, open, now) = read(&mut ui);
            assert_eq!(ws.as_deref(), Some(order[n - 1].as_str()), "{look:?}: {n} starts in row {n}");
            assert!(login.is_some() && !open, "{look:?}: started, so the sheet closed");
            assert!(ui.find("new-session").is_none(), "{look:?}");
            assert!(now == screen, "{look:?}: no session was jumped to");
            reopen(&mut ui);
        }
        // The relay workspace starts as its default login whatever was picked.
        let relay = order.iter().position(|w| w == "ws_relay").unwrap() + 1;
        ui.update(|s, _| s.new_agent = Some("codex".into()));
        press(&mut ui, relay);
        let (_, ws, login, open, _) = read(&mut ui);
        assert!(ws.as_deref() == Some("ws_relay") && login.as_deref() == Some("prof_work") && !open, "{look:?}: {login:?}");
        // No login to start as: the row is picked and the sheet stays to ask.
        reopen(&mut ui);
        ui.update(|s, _| {
            for a in &mut s.info.as_mut().unwrap().agents {
                a.available = false;
            }
            s.new_agent = None;
        });
        ui.frame(0.016);
        let other = if relay == 3 { 2 } else { 3 };
        press(&mut ui, other);
        let (_, ws, login, open, _) = read(&mut ui);
        assert!(ws.as_deref() == Some(order[other - 1].as_str()) && login.is_none() && open, "{look:?}: {ws:?} {login:?} {open}");
    }
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// A workspace's "+" starts at once as the workspace's default login; with no login to use,
/// it opens the sheet instead.
#[test]
fn a_workspaces_plus_starts_without_the_sheet() {
    let mut app = demo("sidebar");
    app.new_agent = Some("codex".into());
    app.open_new_session_in("ws_relay");
    assert!(!app.new_open);
    assert_eq!(app.new_ws.as_deref(), Some("ws_relay"));
    assert_eq!(app.new_agent.as_deref(), Some("prof_work"), "the workspace's default");
    app.new_agent = Some("codex".into());
    app.open_new_session_in("ws_notes");
    assert!(!app.new_open);
    assert_eq!(app.new_agent.as_deref(), Some("codex"), "no default: the login picked last");
    for a in &mut app.info.as_mut().unwrap().agents {
        a.available = false;
    }
    app.open_new_session_in("ws_notes");
    assert!(app.new_open, "nothing to start as: the sheet asks");
    assert_eq!(app.new_ws.as_deref(), Some("ws_notes"));
}

/// The compact sidebar lays out in both looks and its buttons do what they say: search,
/// collapse, "Show N more", a workspace's "+", and a hovered row's Pin and Archive.
#[test]
fn compact_sidebar_lays_out_and_clicks_through() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 1000.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    let state = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = open("sidebar", look);
        for k in ["toggle-sidebar", "sidebar-search", "nav-new", "nav-automations", "add-folder", "group-ws_relay", "group-new-ws_relay", "more-ws_relay", "ses_3", "ses_s4"] {
            assert!(ui.find(k).is_some(), "{look:?}: {k} is not on screen");
        }
        assert!(ui.find("search-field").is_none(), "{look:?}: search is behind its button");
        assert!(ui.find("pin-ses_2").is_none(), "{look:?}: Pin shows on hover only");
        // The search button opens the field in the nav's place.
        click(&mut ui, "sidebar-search");
        assert!(state(&mut ui, &|s| s.search_open));
        assert!(ui.find("search-field").is_some() && ui.find("nav-new").is_none(), "{look:?}");
        click(&mut ui, "sidebar-search");
        assert!(state(&mut ui, &|s| !s.search_open));
        // Show more, then collapse.
        assert!(ui.find("ses_s11").is_none(), "{look:?}: past the newest few");
        click(&mut ui, "more-ws_relay");
        assert!(ui.find("ses_s11").is_some(), "{look:?}: Show more shows the rest");
        click(&mut ui, "group-ws_relay");
        assert!(state(&mut ui, &|s| s.collapsed.contains("ws_relay")));
        assert!(ui.find("ses_1").is_none() && ui.find("ses_3").is_some(), "{look:?}: the pinned one stays");
        // A workspace's "+" starts a session there without the sheet.
        click(&mut ui, "group-new-ws_notes");
        assert!(state(&mut ui, &|s| !s.new_open && s.new_ws.as_deref() == Some("ws_notes") && s.new_agent.is_some()), "{look:?}");

        // The sidebar's edge drags it wider, within limits; a double press puts it back.
        let mut ui = open("sidebar", look);
        let (x, y) = ui.find("sidebar-grip").expect("the sidebar's edge");
        ui.pointer_down(x, y);
        ui.pointer_move(x + 60.0, y);
        ui.frame(0.016);
        ui.pointer_up(x + 60.0, y);
        ui.frame(0.016);
        let wide = crate::look::Theme::new(look).sidebar_w + 60.0;
        assert!(state(&mut ui, &|s| s.sidebar_w.is_some_and(|w| (w - wide).abs() < 1.0)), "{look:?}: dragged 60 pt wider");
        let (x, y) = ui.find("sidebar-grip").unwrap();
        ui.pointer_down(x, y);
        ui.pointer_move(x + 1000.0, y);
        ui.pointer_up(x + 1000.0, y);
        ui.frame(0.016);
        assert!(state(&mut ui, &|s| s.sidebar_w == Some(model::SIDEBAR_MAX_W)), "{look:?}: no wider than the most");

        // Hovered: Pin and Archive in place of the age; pressing one doesn't open the row.
        let mut ui = open("sidebar-hover", look);
        assert!(ui.find("pin-ses_2").is_some() && ui.find("archive-ses_2").is_some(), "{look:?}");
        click(&mut ui, "archive-ses_2");
        assert!(state(&mut ui, &|s| s.confirm_archive.as_deref() == Some("ses_2")), "{look:?}: a live one asks first");
        assert!(state(&mut ui, &|s| s.screen == Screen::Session("ses_1".into())), "{look:?}: the row didn't open");
        let mut ui = open("sidebar-hover", look);
        click(&mut ui, "pin-ses_2");
        assert!(state(&mut ui, &|s| s.session("ses_2").is_some_and(|x| x.pinned_at.is_some()) && s.pinned_sessions().len() == 2), "{look:?}");
        assert!(state(&mut ui, &|s| s.screen == Screen::Session("ses_1".into())), "{look:?}: the row didn't open");
    }
}

// ---- notes -------------------------------------------------------------------------------

fn note_changed(ch: NoteChanged) -> bridge::FromHost {
    bridge::FromHost::Note(agenthost_proto::jsonrpc::Notification { jsonrpc: "2.0".into(), method: agenthost_proto::methods::N_NOTE_CHANGED.into(), params: serde_json::to_value(ch).unwrap() })
}

#[test]
fn notes_follow_the_host_but_keep_unsaved_edits() {
    let mut app = demo("notes");
    assert_eq!(app.notes.iter().map(|n| n.note_id.as_str()).collect::<Vec<_>>(), ["note_rate", "note_oauth", "note_icons", "note_flaky"], "open newest first, then done");
    assert_eq!(crate::notes::summary(&app), "3 open notes");
    assert!(!app.note_dirty());
    // Changed on the phone while shown here, unedited: the editor shows the new text.
    let mut n = app.note("note_rate").unwrap().clone();
    n.text = "Rate-limit pairing attempts\nPer device key.".into();
    app.apply(note_changed(NoteChanged { note_id: n.note_id.clone(), note: Some(n.clone()), removed: false }));
    assert_eq!(app.note_edit.text, "Rate-limit pairing attempts\nPer device key.");
    assert!(!app.note_dirty());
    // Edited here first: the edit stays, still unsaved.
    app.note_edit.replace_text("Rate-limit pairing attempts\nPer device key, 5 a minute.");
    n.text = "Something else".into();
    app.apply(note_changed(NoteChanged { note_id: n.note_id.clone(), note: Some(n.clone()), removed: false }));
    assert_eq!(app.note_edit.text, "Rate-limit pairing attempts\nPer device key, 5 a minute.");
    assert!(app.note_dirty());
    // Picking another note saves it (shown at once) and shows the other.
    app.select_note("note_icons");
    assert_eq!(app.note("note_rate").unwrap().text, "Rate-limit pairing attempts\nPer device key, 5 a minute.");
    assert_eq!(app.note_edit.text, "Try a darker app icon\nGlass rim, no green.");
    // Trailing space and blank lines alone aren't a change; a blank note isn't saved.
    app.note_edit.replace_text("Try a darker app icon\nGlass rim, no green.\n\n  ");
    assert!(!app.note_dirty());
    app.note_edit.replace_text("   \n");
    assert!(!app.note_dirty());
    // Done moves it after the open ones; deleted elsewhere, it leaves the editor.
    app.set_note_done("note_rate", true);
    assert_eq!(app.notes.iter().map(|n| n.note_id.as_str()).collect::<Vec<_>>(), ["note_oauth", "note_icons", "note_rate", "note_flaky"], "done ones last, the latest done first");
    app.apply(note_changed(NoteChanged { note_id: "note_icons".into(), note: None, removed: true }));
    assert!(app.note("note_icons").is_none() && app.sel_note.is_none() && app.note_edit.text.is_empty());
    // Delete asks first.
    app.delete_note("note_oauth");
    assert!(app.note("note_oauth").is_some() && app.confirm_delete_note.as_deref() == Some("note_oauth"));
    app.delete_note("note_oauth");
    assert!(app.note("note_oauth").is_none() && app.confirm_delete_note.is_none());
}

#[test]
fn a_new_note_is_adopted_once_saved() {
    let mut app = demo("notes-new");
    assert_eq!(app.new_note_ws.as_deref(), Some("ws_demo"));
    assert!(app.note_dirty());
    app.save_note();
    assert!(app.note_saving);
    // A second Save before the answer sends nothing more.
    app.save_note();
    let saved = Note { note_id: "note_new".into(), workspace_id: "ws_demo".into(), text: "Show the relay's latency in Settings\nNext to Connected.".into(), done_at: None, session_ids: vec![], created_at: crate::view::now_ms(), updated_at: crate::view::now_ms() };
    let gen = app.note_gen;
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::NoteSaved { new_gen: Some(gen) }, result: Ok(serde_json::to_value(&saved).unwrap()) });
    assert!(!app.note_saving && app.new_note_ws.is_none());
    assert_eq!(app.sel_note.as_deref(), Some("note_new"));
    assert!(!app.note_dirty());
    // An answer for a new note begun before this one only adds it to the list.
    app.new_note(Some("ws_relay"));
    let old = Note { note_id: "note_late".into(), text: "Late".into(), ..saved };
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::NoteSaved { new_gen: Some(gen) }, result: Ok(serde_json::to_value(&old).unwrap()) });
    assert!(app.note("note_late").is_some());
    assert_eq!(app.new_note_ws.as_deref(), Some("ws_relay"), "still writing the new one");
}

#[test]
fn start_session_from_a_note_fills_the_composer() {
    let mut app = demo("notes-start");
    assert!(app.new_open);
    assert_eq!(app.new_ws.as_deref(), Some("ws_relay"));
    assert_eq!(app.start_note.as_ref().map(|(id, ws, _)| (id.as_str(), ws.as_str())), Some(("note_rate", "ws_relay")));
    let text = app.start_note.as_ref().unwrap().2.clone();
    assert!(text.starts_with("Rate-limit pairing attempts\n"));
    // Another New Session forgets the note.
    let mut other = demo("notes-start");
    other.open_new_session(false);
    assert!(other.start_note.is_none());
    // The host's answer opens the session with the note waiting in the composer.
    let mut s = app.session("ses_3").unwrap().clone();
    s.session_id = "ses_from_note".into();
    s.title = None;
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::SessionFromNote(text.clone()), result: Ok(serde_json::to_value(&s).unwrap()) });
    assert_eq!(app.screen, Screen::Session("ses_from_note".into()));
    assert_eq!(app.prompt.text, text);
}

/// The Notes screens lay out in both looks, and clicking through them does what it says.
#[test]
fn note_screens_lay_out_and_click_through() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 1200.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    let scenes: [(&str, &[&str], &[&str]); 6] = [
        ("notes", &["nav-notes", "note-list", "note-group-ws_relay", "note-new-ws_relay", "note-note_rate", "note-done-note_rate", "note-text", "note-save", "note-start", "note-toggle-done", "delete-note", "note-sessions", "show-done"], &["note-note_flaky"]),
        ("notes-session", &["note-session-ses_2", "note-note_flaky"], &[]),
        ("notes-new", &["note-row-new", "note-ws", "note-save", "note-group-ws_demo"], &["note-start"]),
        ("notes-start", &["new-session", "start-session"], &[]),
        ("notes-confirm-delete", &["delete-note-alert", "confirm-delete-note"], &[]),
        ("notes-old-host", &[], &["nav-notes", "note-list"]),
    ];
    for look in [Look::Mac, Look::Gnome] {
        for (scene, there, absent) in scenes {
            let ui = open(scene, look);
            for k in there {
                assert!(ui.find(k).is_some(), "{look:?} {scene}: {k} is not on screen");
            }
            for k in absent {
                assert!(ui.find(k).is_none(), "{look:?} {scene}: {k} should not be on screen");
            }
        }
    }
    let state = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let mut ui = open("notes", Look::Gnome);
    // Another note shows in the editor; its session opens; the sidebar brings Notes back.
    click(&mut ui, "note-note_oauth");
    assert!(state(&mut ui, &|s| s.sel_note.as_deref() == Some("note_oauth") && s.note_edit.text.starts_with("Refresh OAuth")));
    click(&mut ui, "note-session-ses_2");
    assert!(state(&mut ui, &|s| s.screen == Screen::Session("ses_2".into())));
    click(&mut ui, "nav-notes");
    assert!(state(&mut ui, &|s| s.screen == Screen::Notes && s.sel_note.as_deref() == Some("note_oauth")));
    // Its checkbox marks it done: it leaves the list until done ones are shown.
    click(&mut ui, "note-done-note_oauth");
    assert!(state(&mut ui, &|s| s.note("note_oauth").is_some_and(|n| n.done_at.is_some())));
    assert!(ui.find("note-note_oauth").is_none());
    click(&mut ui, "show-done-box");
    assert!(ui.find("note-note_oauth").is_some() && ui.find("note-note_flaky").is_some());
    // A workspace's "+" starts a new note there; Start Session opens the sheet on the note's workspace.
    click(&mut ui, "note-new-ws_relay");
    assert!(state(&mut ui, &|s| s.new_note_ws.as_deref() == Some("ws_relay") && s.note_edit.text.is_empty()));
    assert!(ui.find("note-row-new").is_some());
    // Return in the note starts a new line.
    click(&mut ui, "note-text");
    ui.update(|s, _| s.note_edit.replace_text("Idea"));
    ui.frame(0.016);
    ui.key(rune::KeyEvent { key: rune::Key::Enter, mods: rune::Mods::default() });
    ui.frame(0.016);
    assert!(state(&mut ui, &|s| s.note_edit.text == "Idea\n"), "Return breaks the line");
    click(&mut ui, "note-note_rate");
    click(&mut ui, "note-start");
    assert!(state(&mut ui, &|s| s.new_open && s.start_note.as_ref().is_some_and(|(id, _, _)| id == "note_rate")));
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// The terminals follow what the host says: output lands on the right screen, a shell you
/// left closes its tab, and the last tab closing hides the panel.
#[test]
fn terminals_follow_the_host() {
    use crate::bridge::TermCmd;
    use base64::Engine as _;
    let mut app = demo("terminal");
    assert_eq!(app.panel_shown(), Some(model::PanelTab::Terminal));
    assert_eq!(app.terms.tabs.len(), 2);
    let screen = app.terms.current().unwrap().emu.screen_text();
    assert!(screen.contains("test resume_twice ... FAILED") && screen.contains("日本語 ✓"), "{screen}");
    assert_eq!(app.terms.tabs[0].title(), "ses_1", "a terminal is named for its folder");
    assert_eq!(app.terms.tabs[1].title(), "npm run dev", "or for what its shell calls itself");

    let note = |method: &str, params: serde_json::Value| bridge::FromHost::Note(agenthost_proto::jsonrpc::Notification { jsonrpc: "2.0".into(), method: method.into(), params });
    let output = |id: &str, offset: u64, text: &str, replay: bool| {
        let data = base64::engine::general_purpose::STANDARD.encode(text);
        note(m::N_TERMINAL_OUTPUT, json!({"terminal_id": id, "offset": offset, "data": data, "replay": replay}))
    };
    // Output for the terminal that isn't showing goes to its own screen.
    app.apply(output("term_2", 500, "second line\r\n", false));
    assert!(app.terms.tabs[1].emu.screen_text().ends_with("second line"));
    assert!(!app.terms.current().unwrap().emu.screen_text().contains("second line"));

    // A program asking where the cursor is gets its answer as input; one asked in a
    // replay of old output does not get a second.
    app.terms.sent.clear();
    app.apply(output("term_1", 2000, "\x1b[6n", true));
    assert!(app.terms.sent.is_empty(), "{:?}", app.terms.sent);
    app.apply(output("term_1", 2004, "\x1b[6n", false));
    assert!(matches!(&app.terms.sent[..], [TermCmd::Input { id, data }] if id == "term_1" && data.starts_with(b"\x1b[") && data.ends_with(b"R")), "{:?}", app.terms.sent);

    // What has been drawn is acknowledged now and then, so the host sends more.
    app.terms.sent.clear();
    let big = "x".repeat(40_000);
    app.apply(output("term_1", 3000, &big, false));
    assert!(app.terms.sent.is_empty(), "not for every piece");
    app.apply(output("term_1", 43_000, &big, false));
    assert_eq!(app.terms.sent, vec![TermCmd::Ack { id: "term_1".into(), offset: 83_000 }]);

    // Typing goes to the terminal that shows, and brings its view back to the prompt.
    app.terms.sent.clear();
    app.terminal_wheel(1000.0, 17.0);
    assert!(app.terms.current().unwrap().emu.scrolled() > 0);
    app.terminal_input(b"ls\r".to_vec());
    assert_eq!(app.terms.sent, vec![TermCmd::Input { id: "term_1".into(), data: b"ls\r".to_vec() }]);
    assert_eq!(app.terms.current().unwrap().emu.scrolled(), 0);

    // The second terminal's shell ends (you typed `exit`): its tab goes when the host
    // says the terminal is closed.
    let info = |id: &str, exited: bool, removed: bool| {
        let mut t = json!({"terminal_id": id, "cwd": "/home/nico/Code/relay", "shell": "/bin/zsh", "cols": 80, "rows": 24, "created_at": 1000});
        if exited {
            t["exited"] = json!({"code": 0});
        }
        note(m::N_TERMINAL_CHANGED, json!({"terminal": t, "removed": removed}))
    };
    app.apply(info("term_2", true, false));
    assert_eq!(app.terms.tabs.len(), 2, "until the host confirms");
    app.apply(info("term_2", true, true));
    assert_eq!(app.terms.tabs.len(), 1);
    assert_eq!(app.terms.selected.as_deref(), Some("term_1"));
    assert!(app.panel_open);
    // The selected one closing selects a neighbour; the last one closing hides the panel.
    app.apply(info("term_1", false, true));
    assert!(app.terms.tabs.is_empty() && app.terms.selected.is_none());
    assert!(!app.panel_open && app.panel_shown().is_none());

    // A terminal opened from another window shows up as a tab without stealing the view.
    app.apply(info("term_9", false, false));
    assert_eq!(app.terms.tabs.len(), 1);
    assert!(!app.panel_open);

    // A new connection starts from the host's list again.
    app.apply(bridge::FromHost::Connected { admin: true, remote: None });
    assert!(app.terms.tabs.is_empty());
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::Terminals, result: Ok(json!({"terminals": [{"terminal_id": "term_9", "cwd": "/srv", "shell": "/bin/sh", "cols": 80, "rows": 24, "created_at": 1}]})) });
    assert_eq!(app.terms.tabs.len(), 1);
    assert_eq!(app.terms.selected.as_deref(), Some("term_9"));
}

/// The panel is the side questions' and the terminal's: each shows where it belongs.
#[test]
fn the_right_panel_shows_one_tab_at_a_time() {
    use model::PanelTab;
    let mut app = demo("terminal-side");
    assert_eq!(app.panel_shown(), Some(PanelTab::Terminal));
    assert_eq!(crate::panel::tabs(&app), vec![PanelTab::Terminal, PanelTab::Side]);
    assert!(app.trailing_visible() && !app.side_visible());
    app.show_panel(PanelTab::Side);
    assert!(app.side_visible());
    // Side questions belong to their session: elsewhere the panel waits, and comes back.
    app.screen = Screen::Home;
    assert_eq!(app.panel_shown(), None);
    assert_eq!(crate::panel::tabs(&app), vec![PanelTab::Terminal]);
    app.screen = Screen::Session("ses_1".into());
    assert!(app.side_visible());
    // The terminal is the computer's: it stays beside any screen.
    assert!(app.toggle_terminal());
    app.screen = Screen::Home;
    assert_eq!(app.panel_shown(), Some(PanelTab::Terminal));
    assert!(!app.toggle_terminal());
    assert_eq!(app.panel_shown(), None);
    // The terminal's width drags; the side questions have a width of their own, which
    // drags too. A dragged width goes back to the look's.
    let t = crate::look::Theme::new(crate::look::Look::Gnome);
    app.toggle_terminal();
    app.resize_panel(Some(10_000.0), false);
    assert_eq!(app.panel_width(&t), crate::terminal::PANEL_MAX_W);
    app.resize_panel(Some(700.0), false);
    assert_eq!((app.panel_width(&t), app.trailing_width(&t)), (700.0, 700.0));
    app.screen = Screen::Session("ses_1".into());
    app.show_panel(PanelTab::Side);
    assert_eq!(app.panel_width(&t), crate::side::width(&t));
    app.resize_panel(Some(520.0), false);
    assert_eq!((app.panel_width(&t), app.trailing_width(&t)), (520.0, 520.0));
    app.resize_panel(Some(10.0), false);
    assert_eq!(app.panel_width(&t), crate::terminal::PANEL_MIN_W);
    app.show_panel(PanelTab::Terminal);
    assert_eq!(app.panel_width(&t), 700.0, "the terminal's width is as it was");
    app.show_panel(PanelTab::Side);
    app.resize_panel(None, false);
    assert_eq!((app.panel_width(&t), app.side_w, app.panel_w), (crate::side::width(&t), None, Some(700.0)));

    // A new terminal starts in the open session's own folder (its worktree).
    let mut app = demo("terminal");
    if let Some(x) = app.sessions.iter_mut().find(|x| x.session_id == "ses_1") {
        x.worktree = Some(SessionWorktree { path: "/wt/ses_1".into(), branch: "b".into() });
    }
    // A paired device gets no terminal: the host would refuse it.
    app.admin = false;
    assert!(!app.terminal_allowed());
    app.admin = true;
    assert!(app.terminal_allowed());
    if let Some(i) = app.info.as_mut() {
        i.protocol_version = 9;
    }
    assert!(!app.terminal_allowed(), "nor does a host from before terminals");
}

/// The Terminal tab lays out in both looks, and the keyboard is the shell's while it has
/// the focus: every key in order, Tab and Escape included.
#[test]
fn the_terminal_lays_out_and_takes_the_keyboard() {
    use crate::bridge::TermCmd;
    use crate::look::Look;
    use rune::{Key, KeyEvent, Mods};
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    let scenes: [(&str, &[&str], &[&str]); 5] = [
        ("terminal", &["panel", "panel-tabs", "panel-tab-terminal", "panel-hide", "panel-grip", "term-tabs", "term-tab-term_1", "term-tab-term_2", "term-close-term_2", "term-new", "term-view", "term-grid", "toggle-terminal"], &["panel-tab-side", "term-ended", "term-note"]),
        ("terminal-home", &["panel", "term-view", "term-grid", "toggle-terminal"], &["panel-tab-side"]),
        ("terminal-side", &["panel-tab-terminal", "panel-tab-side", "term-view", "toggle-side"], &["side-pane"]),
        ("terminal-ended", &["term-tab-term_3", "term-view", "term-ended"], &[]),
        ("terminal-remote", &["panel", "term-note"], &["term-view", "term-tabs", "toggle-terminal"]),
    ];
    for look in [Look::Mac, Look::Gnome] {
        for (scene, there, absent) in scenes {
            let ui = open(scene, look);
            for k in there {
                assert!(ui.find(k).is_some(), "{look:?} {scene}: {k} is not on screen");
            }
            for k in absent {
                assert!(ui.find(k).is_none(), "{look:?} {scene}: {k} should not be on screen");
            }
        }
    }
    let read = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let key = |ui: &mut rune::Runtime<model::App>, key: Key, mods: Mods| {
        ui.key(KeyEvent { key, mods });
        ui.frame(0.016);
    };
    let none = Mods::default();
    let ctrl = Mods { ctrl: true, cmd: true, ..none };

    for look in [Look::Mac, Look::Gnome] {
        let mut ui = open("terminal", look);
        // Laying the pane out fits the terminal to it, and tells the host its size.
        assert!(read(&mut ui, &|s| s.terms.fit.is_some_and(|(c, r)| c >= 30 && r >= 20)), "{look:?}: the pane sized the terminal");
        assert!(read(&mut ui, &|s| {
            let fit = s.terms.fit.unwrap();
            s.terms.current().unwrap().emu.size() == fit && s.terms.sent.iter().any(|c| *c == TermCmd::Resize { id: "term_1".into(), cols: fit.0, rows: fit.1 })
        }));
        // The panel is the window's trailing pane, and the cells sit inside it.
        let rect = |ui: &rune::Runtime<model::App>, key: &str| ui.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key} is not on screen")).rect;
        let (panel, toolbar, view, grid) = (rect(&ui, "panel"), rect(&ui, "toolbar"), rect(&ui, "term-view"), rect(&ui, "term-grid"));
        assert_eq!((panel[0] + panel[2], panel[1], panel[3]), (1280.0, 0.0, 800.0), "{look:?}: {panel:?}");
        assert_eq!(toolbar[0] + toolbar[2], panel[0], "{look:?}: the main pane ends where the panel begins");
        assert!(grid[0] >= view[0] && grid[1] >= view[1] && grid[0] + grid[2] <= view[0] + view[2] && grid[1] + grid[3] <= view[1] + view[3], "{look:?}: {grid:?} in {view:?}");
        assert!(view[2] - grid[2] < 30.0 && view[3] - grid[3] < 40.0, "{look:?}: the cells fill the pane: {grid:?} in {view:?}");
        assert!(ui.elements().iter().any(|e| e.key.as_deref() == Some("term-grid") && e.role == "terminal"));
        // A click in the terminal gives it the keyboard.
        click(&mut ui, "term-view");
        assert!(ui.elements().iter().any(|e| e.key.as_deref() == Some("term-view") && e.focused), "{look:?}: the click focused the terminal");
        ui.update(|s, _| s.terms.sent.clear());
        key(&mut ui, Key::Text("l".into()), none);
        key(&mut ui, Key::Text("s".into()), none);
        key(&mut ui, Key::Text(" ".into()), none);
        key(&mut ui, Key::Tab, none);
        key(&mut ui, Key::Enter, none);
        key(&mut ui, Key::Escape, none);
        key(&mut ui, Key::Up, none);
        let typed = |s: &model::App| -> Vec<u8> {
            s.terms.sent.iter().flat_map(|c| match c {
                TermCmd::Input { id, data } if id == "term_1" => data.clone(),
                other => panic!("unexpected {other:?}"),
            }).collect()
        };
        assert!(read(&mut ui, &|s| typed(s) == b"ls \t\r\x1b\x1b[A"), "{look:?}: every key went to the shell, in order");
        assert!(read(&mut ui, &|s| s.panel_shown() == Some(model::PanelTab::Terminal) && s.screen == Screen::Session("ses_1".into())), "{look:?}: and did nothing else");
        // Ctrl+C is the shell's (on a Mac it arrives as Control with the letter).
        ui.update(|s, _| s.terms.sent.clear());
        if look == Look::Mac {
            key(&mut ui, Key::Text("c".into()), Mods { ctrl: true, ..none });
        } else {
            key(&mut ui, Key::Command('c'), ctrl);
        }
        assert!(read(&mut ui, &|s| typed(s) == b"\x03"), "{look:?}: Ctrl+C interrupts");

        // The other terminal's tab; then "+" asks the host for a third.
        click(&mut ui, "term-tab-term_2");
        assert!(read(&mut ui, &|s| s.terms.selected.as_deref() == Some("term_2")));
        ui.update(|s, _| s.terms.sent.clear());
        key(&mut ui, Key::Text("q".into()), none);
        assert!(read(&mut ui, &|s| s.terms.sent == vec![TermCmd::Input { id: "term_2".into(), data: b"q".to_vec() }]), "{look:?}: the tab you clicked has the keyboard");
        click(&mut ui, "term-new");
        assert!(read(&mut ui, &|s| s.terms.opening));

        // Ctrl+` hides the terminal from inside it, and brings it back with the keyboard.
        let backtick = if look == Look::Mac { KeyEvent { key: Key::Text("`".into()), mods: Mods { ctrl: true, ..none } } } else { KeyEvent { key: Key::Command('`'), mods: ctrl } };
        ui.key(backtick.clone());
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.panel_shown().is_none()), "{look:?}: Ctrl+` hides the terminal");
        assert!(ui.find("panel").is_none());
        ui.key(backtick);
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(ui.find("term-view").is_some(), "{look:?}: and shows it again");
        ui.update(|s, _| s.terms.sent.clear());
        key(&mut ui, Key::Text("w".into()), none);
        assert!(read(&mut ui, &|s| s.terms.sent == vec![TermCmd::Input { id: "term_2".into(), data: b"w".to_vec() }]), "{look:?}: with the keyboard in it");

        // The panel's own button hides it; the toolbar's shows it.
        click(&mut ui, "panel-hide");
        assert!(ui.find("panel").is_none());
        click(&mut ui, "toggle-terminal");
        assert!(ui.find("term-view").is_some());
    }

    // The first terminal: the button asks the host for a shell, and when it has started
    // the keyboard is already in it.
    let mut ui = open("terminal", Look::Gnome);
    ui.update(|s, _| {
        s.terminals_reset();
        s.terms.selected = None;
        s.panel_open = false;
    });
    ui.frame(0.016);
    click(&mut ui, "toggle-terminal");
    assert!(read(&mut ui, &|s| s.terms.opening && s.terms.tabs.is_empty()));
    assert!(ui.find("term-note").is_some() && ui.find("term-view").is_none(), "it says the shell is starting");
    ui.update(|s, _| s.apply(bridge::FromHost::Reply { tag: bridge::Tag::TerminalOpened, result: Ok(json!({"terminal_id": "term_new", "cwd": "/home/nico/Code/relay", "shell": "/bin/zsh", "cols": 80, "rows": 24, "created_at": 1})) }));
    ui.frame(0.016);
    ui.frame(0.016);
    assert!(ui.find("term-tab-term_new").is_some() && ui.find("term-view").is_some());
    ui.update(|s, _| s.terms.sent.clear());
    key(&mut ui, Key::Text("x".into()), none);
    assert!(read(&mut ui, &|s| s.terms.sent == vec![TermCmd::Input { id: "term_new".into(), data: b"x".to_vec() }]), "the new terminal has the keyboard");

    // The panel's tabs switch what it shows.
    let mut ui = open("terminal-side", Look::Gnome);
    click(&mut ui, "panel-tab-side");
    assert!(ui.find("side-pane").is_some() && ui.find("term-view").is_none());
    // The side questions' edge drags them wider (leftwards); a double press puts it back.
    let own = crate::side::width(&crate::look::Theme::new(Look::Gnome));
    // The panel moves to the side questions' width over a few frames.
    for _ in 0..60 {
        ui.frame(0.016);
    }
    let (x, y) = ui.find("panel-grip").expect("the side questions' edge");
    assert!((x - (1280.0 - own)).abs() < 1.5, "the look's own width to begin with: {x}");
    ui.pointer_down(x, y);
    ui.pointer_move(x - 120.0, y);
    ui.frame(0.016);
    ui.pointer_up(x - 120.0, y);
    ui.frame(0.016);
    assert!(read(&mut ui, &|s| s.side_w.is_some_and(|w| (w - (own + 120.0)).abs() < 1.0) && s.panel_w.is_none()), "dragged 120 pt wider");
    let (x2, y2) = ui.find("panel-grip").unwrap();
    assert!((x - x2 - 120.0).abs() < 1.5, "the edge moved with the pointer: {x} to {x2}");
    ui.pointer_down(x2, y2);
    ui.pointer_up(x2, y2);
    ui.pointer_down(x2, y2);
    ui.pointer_up(x2, y2);
    ui.frame(0.016);
    assert!(read(&mut ui, &|s| s.side_w.is_none()), "a double press puts the look's width back");
    click(&mut ui, "panel-tab-terminal");
    assert!(ui.find("term-view").is_some() && ui.find("side-pane").is_none());
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// The terminal against a real host and a real shell, joined in process (no socket): what
/// the app sends is what the host takes, and what the shell prints is what the screen shows.
#[test]
fn a_real_shell_draws_in_the_terminal() {
    use crate::bridge::TermCmd;
    use agenthost_host::api::{serve_connection, Principal};
    use agenthost_proto::jsonrpc::{Message, Request};
    use base64::Engine as _;
    use tokio::sync::mpsc;

    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    let dir = tempfile::Builder::new().prefix("ahd").tempdir_in(root).unwrap();
    let mut cfg = agenthost_host::HostConfig::in_dir(dir.path());
    cfg.home_dir = dir.path().join("home");
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::env::set_var("AGENTHOST_TERMINAL_SHELL", "/bin/sh");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let host = agenthost_host::Host::open(cfg).unwrap();
    let (tx, in_rx) = mpsc::channel::<String>(256);
    let (out_tx, mut rx) = mpsc::channel::<String>(1024);
    rt.spawn(serve_connection(host.clone(), Principal::local(), in_rx, out_tx));

    let mut app = model::init();
    app.conn = Conn::Connected;
    app.admin = true;
    let next_id = std::cell::Cell::new(0u64);
    // One request: the notifications that arrive before its response go to the app, as
    // the bridge would hand them over.
    let rpc = |app: &mut model::App, rx: &mut mpsc::Receiver<String>, method: &str, params: serde_json::Value| -> Result<serde_json::Value, String> {
        next_id.set(next_id.get() + 1);
        let id = json!(next_id.get());
        tx.blocking_send(Message::Request(Request::new(id.clone(), method, params)).to_json()).unwrap();
        loop {
            let line = rt.block_on(async { tokio::time::timeout(Duration::from_secs(20), rx.recv()).await }).expect("the host went quiet").expect("the connection closed");
            match Message::parse(&line).unwrap() {
                Message::Response(r) if r.id == id => return match r.error {
                    Some(e) => Err(e.message),
                    None => Ok(r.result.unwrap_or_default()),
                },
                Message::Notification(n) => app.apply(bridge::FromHost::Note(n)),
                _ => {}
            }
        }
    };
    rpc(&mut app, &mut rx, m::HELLO, json!({"protocol_version": PROTOCOL_VERSION, "client_name": "desktop-test", "client_version": "0"})).unwrap();

    // Open the terminal as the app does, in the folder it would pick.
    let opened = rpc(&mut app, &mut rx, m::TERMINAL_OPEN, json!({"cwd": work, "cols": 80, "rows": 24})).unwrap();
    app.show_panel(model::PanelTab::Terminal);
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::TerminalOpened, result: Ok(opened) });
    let id = app.terms.selected.clone().expect("the new terminal is selected");
    assert_eq!(app.panel_shown(), Some(model::PanelTab::Terminal));
    rpc(&mut app, &mut rx, m::TERMINAL_ATTACH, json!({"terminal_id": id})).unwrap();

    // What the app queued for the host goes out in order; what the host sends comes in.
    let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);
    let mut acks = 0;
    let pump = |app: &mut model::App, rx: &mut mpsc::Receiver<String>, acks: &mut usize, until: &dyn Fn(&model::App) -> bool, what: &str| {
        let t0 = Instant::now();
        loop {
            for cmd in std::mem::take(&mut app.terms.sent) {
                let r = match cmd {
                    TermCmd::Input { id, data } => rpc(app, rx, m::TERMINAL_INPUT, json!({"terminal_id": id, "data": b64(&data)})),
                    TermCmd::Resize { id, cols, rows } => rpc(app, rx, m::TERMINAL_RESIZE, json!({"terminal_id": id, "cols": cols, "rows": rows})),
                    TermCmd::Ack { id, offset } => {
                        *acks += 1;
                        rpc(app, rx, m::TERMINAL_ACK, json!({"terminal_id": id, "offset": offset}))
                    }
                };
                r.unwrap_or_else(|e| panic!("the host refused what the app sent: {e}"));
            }
            if until(app) {
                return;
            }
            assert!(t0.elapsed() < Duration::from_secs(30), "timed out waiting for {what}; screen:\n{}", app.terms.current().map(|t| t.emu.screen_text()).unwrap_or_default());
            // Anything the host has to say, without waiting on a response.
            rpc(app, rx, m::TERMINAL_LIST, json!({})).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let screen = |a: &model::App| a.terms.current().map(|t| t.emu.screen_text()).unwrap_or_default();

    // The pane's size reaches the program, and typing reaches the shell.
    app.terminal_fit(100.0 * 8.0 + 16.0, 30.0 * 16.0 + 16.0, (8.0, 16.0));
    assert_eq!(app.terms.fit, Some((100, 30)));
    app.terminal_input(b"echo desk-$((40+2)) $(basename \"$PWD\") $(stty size | tr ' ' x)\n".to_vec());
    pump(&mut app, &mut rx, &mut acks, &|a| screen(a).contains("desk-42 work 30x100"), "the command's output");

    // A flood: two megabytes arrive whole because the app acknowledges as it draws.
    app.terminal_input(b"head -c 2000000 /dev/zero | tr '\\0' x; echo; echo flood-$((1+1))-over\n".to_vec());
    pump(&mut app, &mut rx, &mut acks, &|a| screen(a).contains("flood-2-over"), "the end of the flood");
    assert!(acks >= 20, "the app acknowledged along the way ({acks} times)");
    let s = screen(&app);
    assert!(s.lines().any(|l| l.len() == 100 && l.bytes().all(|b| b == b'x')), "the flood's last lines are on screen:\n{s}");

    // Leaving the shell: the host says it ended, and closing takes the tab and the panel.
    app.terminal_input(b"exit\n".to_vec());
    pump(&mut app, &mut rx, &mut acks, &|a| a.terms.tabs.first().is_some_and(|t| t.info.exited.is_some()), "the shell to end");
    assert_eq!(app.terms.tabs[0].info.exited.and_then(|e| e.code), Some(0));
    rpc(&mut app, &mut rx, m::TERMINAL_CLOSE, json!({"terminal_id": id})).unwrap();
    pump(&mut app, &mut rx, &mut acks, &|a| a.terms.tabs.is_empty(), "the tab to close");
    assert!(app.panel_shown().is_none(), "the last terminal closing hides the panel");
    assert!(host.terminals.list().is_empty());
}

/// Changes and Files belong to the open session: they show where it has them, fill from
/// the host's answers and reload when the agent's turn ends.
#[test]
fn changes_and_files_follow_the_open_session() {
    use crate::files::Load;
    use model::PanelTab;
    let mut app = demo("changes");
    assert_eq!(app.panel_shown(), Some(PanelTab::Changes));
    assert_eq!(crate::panel::tabs(&app), vec![PanelTab::Terminal, PanelTab::Changes, PanelTab::Files]);
    assert_eq!(app.changed_files(), Some(3));
    let f = app.browse.diff.files();
    assert_eq!(f.iter().map(|f| (f.path(), f.additions(), f.deletions())).collect::<Vec<_>>(), [("src/net/backoff.rs", 2, 1), ("src/net/mod.rs", 2, 1), ("tests/backoff.rs", 3, 0)]);
    // The tree carries the diff's counts, folders first.
    let node = |app: &model::App, id: &str| app.browse.tree.get(id).and_then(|n| n.data).unwrap();
    assert_eq!((node(&app, "src/net/backoff.rs").added, node(&app, "src/net/backoff.rs").removed), (2, 1));
    assert!(node(&app, "tests/backoff.rs").new);
    assert_eq!(node(&app, "README.md"), crate::files::Change::default());
    assert!(app.browse.tree.get("src/net").is_some_and(|n| n.dir));

    // Answers for another session are dropped.
    app.changes_loaded("ses_2", Ok(SessionChanges { branch: "x".into(), base: "0".into(), against: None, patch: String::new(), truncated: false }));
    assert_eq!(app.changed_files(), Some(3));

    // A file opens in the Files tab: tabs become four-column stops, lines are counted.
    app.open_file("src/net");
    assert!(app.browse.open.is_none(), "a folder isn't a file to open");
    app.open_file("src/net/backoff.rs");
    assert_eq!(app.panel_shown(), Some(PanelTab::Files));
    assert_eq!(app.browse.open.as_ref().map(|f| &f.load), Some(&Load::Loading));
    app.file_read("ses_1", "src/net/mod.rs", Ok(SessionFile { path: "src/net/mod.rs".into(), size: 1, text: Some("x".into()), truncated: false }));
    assert_eq!(app.browse.open.as_ref().map(|f| &f.load), Some(&Load::Loading), "an answer for another file is dropped");
    app.file_read("ses_1", "src/net/backoff.rs", Ok(SessionFile { path: "src/net/backoff.rs".into(), size: 140, text: Some(crate::demo::SAMPLE_FILE.into()), truncated: false }));
    let open = app.browse.open.as_ref().unwrap();
    assert_eq!((open.lines.len(), open.lang, open.binary), (8, Some("rust"), false));
    let (a, b) = open.lines[4];
    assert_eq!(&open.text[a as usize..b as usize], "    base: Duration,");
    assert!(!open.highlights.is_empty(), "Rust is coloured");
    // A file that isn't text says so instead.
    app.open_file("Cargo.lock");
    app.file_read("ses_1", "Cargo.lock", Ok(SessionFile { path: "Cargo.lock".into(), size: 2048, text: None, truncated: false }));
    assert!(app.browse.open.as_ref().unwrap().binary);
    app.close_file();

    // Filtering keeps the folders that lead to a match.
    app.browse.filter.replace_text("retry");
    app.apply_file_filter();
    assert!(app.browse.tree.is_filtering() && app.browse.tree.is_match("src/net/retry.rs") && !app.browse.tree.is_match("README.md"));
    app.browse.filter.replace_text("");
    app.apply_file_filter();
    assert!(!app.browse.tree.is_filtering());

    // Another session: its own state. One without a branch of its own has no Changes.
    app.screen = Screen::Session("ses_2".into());
    app.files_sync();
    assert_eq!((app.browse.session.as_deref(), app.changed_files()), (Some("ses_2"), None));
    if let Some(x) = app.sessions.iter_mut().find(|x| x.session_id == "ses_2") {
        x.worktree = None;
    }
    assert_eq!(crate::panel::tabs(&app), vec![PanelTab::Terminal, PanelTab::Files]);
    app.panel_tab = PanelTab::Changes;
    assert_eq!(app.panel_shown(), None);
    // Elsewhere, and on a host from before protocol 11, neither tab shows.
    app.screen = Screen::Home;
    assert_eq!(crate::panel::tabs(&app), vec![PanelTab::Terminal]);
    app.screen = Screen::Session("ses_1".into());
    if let Some(i) = app.info.as_mut() {
        i.protocol_version = 10;
    }
    assert!(!app.files_available() && !app.changes_available());

    assert_eq!(crate::files::expand_tabs("a\tb\n\t\tc"), "a   b\n        c");
}

/// The Changes and Files tabs lay out in both looks, and a click goes from the diff to a
/// file and back to the tree.
#[test]
fn changes_and_files_lay_out() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    let scenes: [(&str, &[&str], &[&str]); 4] = [
        ("changes", &["panel", "panel-tab-terminal", "panel-tab-changes", "panel-tab-files", "panel-grip", "changes-bar", "changes-summary", "changes-unified", "changes-split", "files-refresh", "changes-diff", "changes-open-0", "toggle-changes"], &["changes-note", "files-body", "panel-tab-side"]),
        ("changes-split", &["changes-diff", "changes-split"], &["changes-note"]),
        ("files", &["files-bar", "files-filter", "files-tree", "src/net/backoff.rs", "README.md"], &["changes-body", "file-view", "files-note"]),
        ("file-open", &["file-bar", "file-back", "file-path", "file-meta", "file-show-changes", "file-view", "file-rows", "L0", "L7"], &["files-tree", "file-note", "L8"]),
    ];
    for look in [Look::Mac, Look::Gnome] {
        for (scene, there, absent) in scenes {
            let ui = open(scene, look);
            for k in there {
                assert!(ui.find(k).is_some(), "{look:?} {scene}: {k} is not on screen");
            }
            for k in absent {
                assert!(ui.find(k).is_none(), "{look:?} {scene}: {k} should not be on screen");
            }
        }
    }
    let read = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = open("changes", look);
        let rect = |ui: &rune::Runtime<model::App>, key: &str| ui.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key} is not on screen")).rect;
        // The diff fills the panel under its bar.
        let (panel, bar, diff) = (rect(&ui, "panel"), rect(&ui, "changes-bar"), rect(&ui, "changes-diff"));
        assert_eq!((diff[0], diff[2]), (panel[0], panel[2]), "{look:?}: {diff:?} in {panel:?}");
        assert!(diff[1] >= bar[1] + bar[3] && (diff[1] + diff[3] - 800.0).abs() < 1.0 && diff[3] > 500.0, "{look:?}: {diff:?} under {bar:?}");
        click(&mut ui, "changes-split");
        assert!(read(&mut ui, &|s| s.browse.diff.mode() == rune_kit::diff::DiffMode::Split), "{look:?}");
        // From a file's header in the diff to the file itself.
        click(&mut ui, "changes-open-0");
        assert!(read(&mut ui, &|s| s.panel_shown() == Some(model::PanelTab::Files) && s.browse.open.as_ref().is_some_and(|f| f.path == "src/net/backoff.rs")), "{look:?}");
        assert!(ui.find("file-note").is_some(), "{look:?}: it says it is loading");
        ui.update(|s, _| s.file_read("ses_1", "src/net/backoff.rs", Ok(SessionFile { path: "src/net/backoff.rs".into(), size: 140, text: Some(crate::demo::SAMPLE_FILE.into()), truncated: false })));
        ui.frame(0.016);
        ui.frame(0.016);
        // Line numbers sit left of the code, one line under the other.
        let (l0, l1, view) = (rect(&ui, "L0"), rect(&ui, "L1"), rect(&ui, "file-view"));
        assert!(l0[0] >= view[0] && l1[1] > l0[1] && l1[1] - l0[1] < 30.0, "{look:?}: {l0:?} {l1:?} in {view:?}");
        // Back to where it changed, then to the tree.
        click(&mut ui, "file-show-changes");
        assert!(read(&mut ui, &|s| s.panel_shown() == Some(model::PanelTab::Changes)), "{look:?}");
        click(&mut ui, "panel-tab-files");
        click(&mut ui, "file-back");
        assert!(ui.find("files-tree").is_some() && ui.find("file-view").is_none(), "{look:?}");
        click(&mut ui, "toggle-changes");
        click(&mut ui, "toggle-changes");
        assert!(ui.find("panel").is_none(), "{look:?}: the toolbar's button hides the panel from Changes");
    }
}

/// The Browser tab lays out in both looks; the tab on screen is the one attached to, a click
/// lands on the page where you clicked, keys go to the page, and a tab of yours follows the
/// panel's size while an agent's keeps its own.
#[test]
fn the_browser_lays_out_and_sends_what_you_do() {
    use crate::bridge::BrowserCmd;
    use crate::look::Look;
    use rune::{Key, KeyEvent, Mods};
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    let scenes: [(&str, &[&str], &[&str]); 4] = [
        (
            "browser",
            &["panel", "panel-tab-browser", "panel-tab-terminal", "browser-tabs", "browser-tab-tab_agent", "browser-tab-tab_mine", "browser-close-tab_agent", "browser-new", "browser-back", "browser-forward", "browser-reload", "browser-url", "browser-view", "browser-ask", "browser-allow", "browser-deny", "browser-owner", "browser-clear", "toggle-browser"],
            &["browser-note", "browser-error", "browser-clear-confirm"],
        ),
        ("browser-empty", &["panel", "browser-note", "browser-url", "browser-new", "browser-clear"], &["browser-view", "browser-owner"]),
        ("browser-missing", &["panel", "browser-note"], &["browser-url", "browser-view"]),
        ("browser-remote", &[], &["panel-tab-browser", "toggle-browser", "browser-view"]),
    ];
    for look in [Look::Mac, Look::Gnome] {
        for (scene, there, absent) in scenes {
            let ui = open(scene, look);
            for k in there {
                assert!(ui.find(k).is_some(), "{look:?} {scene}: {k} is not on screen");
            }
            for k in absent {
                assert!(ui.find(k).is_none(), "{look:?} {scene}: {k} should not be on screen");
            }
        }
    }
    let read = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let inputs = |s: &model::App| -> Vec<BrowserInput> {
        s.web.sent.iter().filter_map(|c| match c {
            BrowserCmd::Input { tab, input } if tab == "tab_agent" => Some(input.clone()),
            _ => None,
        }).collect()
    };
    let none = Mods::default();

    for look in [Look::Mac, Look::Gnome] {
        let mut ui = open("browser", look);
        assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_agent") && s.web.view.is_some()), "{look:?}: the agent's tab is on screen");
        assert!(read(&mut ui, &|s| s.web.url.text == "https://staging.relay.example.com/settings"), "{look:?}: the address shows");
        // An agent's tab keeps its size: the picture is scaled to the view.
        assert!(read(&mut ui, &|s| !s.web.sent.iter().any(|c| matches!(c, BrowserCmd::Resize { .. }))), "{look:?}: no resize of the agent's tab");

        // A click in the middle of the view is a click in the middle of the page.
        ui.update(|s, _| s.web.sent.clear());
        click(&mut ui, "browser-view");
        assert!(ui.elements().iter().any(|e| e.key.as_deref() == Some("browser-view") && e.focused), "{look:?}: the click gave the page the keyboard");
        let clicks: Vec<(BrowserPointer, f32, f32)> = {
            let r = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
            let r2 = r.clone();
            ui.update_if(|s, _| {
                *r2.borrow_mut() = inputs(s).into_iter().filter_map(|i| match i {
                    BrowserInput::Mouse { phase, x, y, .. } if phase != BrowserPointer::Move => Some((phase, x, y)),
                    _ => None,
                }).collect();
                false
            });
            r.take()
        };
        assert_eq!(clicks.iter().map(|c| c.0).collect::<Vec<_>>(), vec![BrowserPointer::Down, BrowserPointer::Up], "{look:?}");
        let (_, x, y) = clicks[0];
        assert!((x - 640.0).abs() < 4.0 && (y - 400.0).abs() < 4.0, "{look:?}: the page's centre, not ({x}, {y})");

        // Keys go to the page, in order; ⌘/Ctrl+A selects in the page; ⌘/Ctrl+L goes to the address.
        ui.update(|s, _| s.web.sent.clear());
        for k in [Key::Text("h".into()), Key::Text("i".into()), Key::Enter, Key::Tab, Key::Backspace] {
            ui.key(KeyEvent { key: k, mods: none });
            ui.frame(0.016);
        }
        let cmd = if look == Look::Mac { Mods { cmd: true, logo: true, ..none } } else { Mods { cmd: true, ctrl: true, ..none } };
        ui.key(KeyEvent { key: Key::Command('a'), mods: cmd });
        ui.frame(0.016);
        let keys: Vec<(String, Option<String>, bool)> = {
            let r = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
            let r2 = r.clone();
            ui.update_if(|s, _| {
                *r2.borrow_mut() = inputs(s).into_iter().filter_map(|i| match i {
                    BrowserInput::Key { key, text, mods } => Some((key, text, mods.ctrl || mods.meta)),
                    _ => None,
                }).collect();
                false
            });
            r.take()
        };
        assert_eq!(
            keys,
            vec![
                ("h".to_string(), Some("h".to_string()), false),
                ("i".into(), Some("i".into()), false),
                ("Enter".into(), None, false),
                ("Tab".into(), None, false),
                ("Backspace".into(), None, false),
                ("a".into(), None, true),
            ],
            "{look:?}"
        );
        ui.key(KeyEvent { key: Key::Command('l'), mods: cmd });
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.web.url.focused), "{look:?}: the address field has the keyboard");

        // Your own tab: it follows the panel's size, and waits for its own picture.
        click(&mut ui, "browser-tab-tab_mine");
        assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_mine") && s.web.shown.page().is_none()), "{look:?}");
        assert!(read(&mut ui, &|s| {
            let (w, h) = s.web.view.unwrap();
            s.web.sent.iter().any(|c| matches!(c, BrowserCmd::Resize { tab, width, height } if tab == "tab_mine" && width.abs_diff(w.round() as u32) <= 1 && height.abs_diff(h.round() as u32) <= 1))
        }), "{look:?}: your tab is resized to the view");

        // The open session's agent opens another tab: the panel follows it.
        ui.update(|s, _| {
            let mut b = s.web.browsers["ws_relay"].clone();
            b.tabs.push(BrowserTab { tab_id: "tab_new".into(), url: "https://accounts.example.com/".into(), title: "Sign in".into(), loading: true, session_id: Some("ses_1".into()), width: 1280, height: 800, can_go_back: false, can_go_forward: false });
            s.browser_changed(b);
        });
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_new")), "{look:?}: the agent's new tab is on screen");
        assert!(ui.find("browser-stop").is_some(), "{look:?}: a loading page can be stopped");

        // Signing out asks first.
        click(&mut ui, "browser-clear");
        assert!(ui.find("browser-clear-confirm").is_some());

        // Hiding the panel stops the frames; the toolbar's button brings them back.
        click(&mut ui, "panel-hide");
        assert!(read(&mut ui, &|s| s.web.attached.is_none() && s.web.shown.tab().is_none()), "{look:?}: hidden, nothing attached");
        click(&mut ui, "toggle-browser");
        assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_new")), "{look:?}: shown again, attached again");
    }

    // A tab the host can't attach to is dropped, not tried on every poll.
    let mut ui = open("browser", Look::Gnome);
    ui.update(|s, _| {
        s.browser_attach_failed("tab_agent", "Page.startScreencast took longer than 20s");
        // Said, with the host's reason, not swallowed.
        let e = s.error.take().unwrap_or_default();
        assert!(e.contains("Page.startScreencast took longer than 20s") && e.contains("try again"), "{e}");
    });
    for _ in 0..5 {
        ui.frame(0.016);
    }
    assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_mine")), "the other tab is shown instead");
    ui.update(|s, _| {
        let mut b = s.web.browsers["ws_relay"].clone();
        b.tabs.push(BrowserTab { tab_id: "tab_agent".into(), url: "https://x.example/".into(), title: "X".into(), loading: false, session_id: None, width: 800, height: 600, can_go_back: false, can_go_forward: false });
        s.browser_changed(b);
    });
    ui.frame(0.016);
    ui.frame(0.016);
    assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_mine")), "and not picked again by itself");
    // Pressing its tab is asking for it: it is tried once more.
    click(&mut ui, "browser-tab-tab_agent");
    assert!(read(&mut ui, &|s| s.web.attached.as_deref() == Some("tab_agent")), "pressed, so tried again");
    // With every tab unshowable, the panel says so instead of inviting you to type an address.
    ui.update(|s, _| {
        s.browser_attach_failed("tab_agent", "no such tab");
        s.browser_attach_failed("tab_mine", "no such tab");
        s.error = None;
        let mut b = s.web.browsers["ws_relay"].clone();
        b.tabs = vec![BrowserTab { tab_id: "tab_mine".into(), url: "https://x.example/".into(), title: "X".into(), loading: false, session_id: None, width: 800, height: 600, can_go_back: false, can_go_forward: false }];
        s.browser_changed(b);
    });
    ui.frame(0.016);
    ui.frame(0.016);
    assert!(ui.elements().iter().any(|e| e.text.as_deref().is_some_and(|t| t.starts_with("This page could not be shown"))), "the panel says the page could not be shown");

    // The view maps points to the page: letterboxed, centred.
    let mut app = model::init();
    app.web.view = Some((600.0, 600.0));
    app.web.shown.set_tab(Some("t".into()));
    app.web.shown.show("t", 1, 4, 2, vec![0; 32], (1200, 600));
    assert_eq!(app.browser_point(300.0, 300.0), Some((600.0, 300.0, 0.5)));
    assert_eq!(app.browser_point(0.0, 150.0), Some((0.0, 0.0, 0.5)));
    assert_eq!(app.browser_point(300.0, 100.0), None, "above the picture is not the page");
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// Puts `$AGENTHOST_BROWSER` back as it was.
struct BrowserEnv(Option<std::ffi::OsString>);
impl Drop for BrowserEnv {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => std::env::set_var("AGENTHOST_BROWSER", v),
            None => std::env::remove_var("AGENTHOST_BROWSER"),
        }
    }
}

/// The browser's protocol against a real host, in process: the app reads what the host says
/// about its browser, and a paired phone gets none. Chromium itself starts only when asked
/// (`AGENTHOST_TEST_BROWSER=1`, as the host's `browser_core` is): then a tab opens. Otherwise
/// the host's browser is a program that exits at once, and the panel shows why nothing
/// started. A real Chromium is not for every run: inside an agent's sandbox it aborts as it
/// starts, and each abort is a core dump and a crash notice on the desktop.
#[test]
fn the_browser_against_a_real_host() {
    use agenthost_host::api::{serve_connection, Principal};
    use agenthost_proto::jsonrpc::{Message, Request};
    use tokio::sync::mpsc;

    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let real = std::env::var_os("AGENTHOST_TEST_BROWSER").is_some();
    let _env = BrowserEnv(std::env::var_os("AGENTHOST_BROWSER"));
    if !real {
        let stand_in = ["/usr/bin/false", "/bin/false"].into_iter().find(|p| std::path::Path::new(p).is_file()).expect("no `false` to stand in for the browser");
        std::env::set_var("AGENTHOST_BROWSER", stand_in);
    }
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    let dir = tempfile::Builder::new().prefix("ahd").tempdir_in(root).unwrap();
    let mut cfg = agenthost_host::HostConfig::in_dir(dir.path());
    cfg.home_dir = dir.path().join("home");
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let host = agenthost_host::Host::open(cfg).unwrap();
    let connect = |principal: Principal| {
        let (tx, in_rx) = mpsc::channel::<String>(256);
        let (out_tx, rx) = mpsc::channel::<String>(1024);
        rt.spawn(serve_connection(host.clone(), principal, in_rx, out_tx));
        (tx, rx)
    };
    let next_id = std::cell::Cell::new(0u64);
    let rpc = |app: &mut model::App, tx: &mpsc::Sender<String>, rx: &mut mpsc::Receiver<String>, method: &str, params: serde_json::Value| -> Result<serde_json::Value, agenthost_proto::RpcError> {
        next_id.set(next_id.get() + 1);
        let id = json!(next_id.get());
        tx.blocking_send(Message::Request(Request::new(id.clone(), method, params)).to_json()).unwrap();
        loop {
            let line = rt.block_on(async { tokio::time::timeout(Duration::from_secs(40), rx.recv()).await }).expect("the host went quiet").expect("the connection closed");
            match Message::parse(&line).unwrap() {
                Message::Response(r) if r.id == id => return match r.error {
                    Some(e) => Err(e),
                    None => Ok(r.result.unwrap_or_default()),
                },
                Message::Notification(n) => app.apply(bridge::FromHost::Note(n)),
                _ => {}
            }
        }
    };
    let hello = json!({"protocol_version": PROTOCOL_VERSION, "client_name": "desktop-test", "client_version": "0"});

    // A paired phone is refused.
    let mut phone_app = model::init();
    let (ptx, mut prx) = connect(Principal { capabilities: agenthost_proto::methods::Capability::phone(), device_id: Some("dev_1".into()), label: "phone".into() });
    rpc(&mut phone_app, &ptx, &mut prx, m::HELLO, hello.clone()).unwrap();
    let err = rpc(&mut phone_app, &ptx, &mut prx, m::BROWSER_LIST, json!({})).unwrap_err();
    assert_eq!(err.code, agenthost_proto::jsonrpc::codes::CAPABILITY_DENIED);

    // The app on the host's own computer.
    let mut app = model::init();
    app.conn = Conn::Connected;
    app.admin = true;
    let (tx, mut rx) = connect(Principal::local());
    rpc(&mut app, &tx, &mut rx, m::HELLO, hello).unwrap();
    let listed = rpc(&mut app, &tx, &mut rx, m::BROWSER_LIST, json!({})).map_err(|e| e.message);
    app.apply(bridge::FromHost::Reply { tag: bridge::Tag::Browsers, result: listed.map_err(|m| agenthost_proto::RpcError::internal(m)) });
    let support = app.web.support.clone().expect("the app knows whether the host has a browser");
    app.show_panel(model::PanelTab::Browser);
    if !support.available {
        assert!(support.detail.contains("Chromium"), "{support:?}");
        return;
    }
    let ws = app.browser_ws();
    assert_eq!(ws, QUICK_WORKSPACE_ID, "with nothing open, the browser is Quick tasks'");
    let opened = rpc(&mut app, &tx, &mut rx, m::BROWSER_OPEN, json!({"workspace_id": ws, "url": "about:blank", "width": 600, "height": 500}));
    match opened {
        Ok(tab) => {
            assert!(real, "the stand-in can't have started a browser");
            app.apply(bridge::FromHost::Reply { tag: bridge::Tag::BrowserOpened, result: Ok(tab) });
            app.browser_tick();
            assert!(app.web.attached.is_some(), "the new tab is attached to");
            let t = app.browser_tab().unwrap().clone();
            assert_eq!((t.width, t.height, t.session_id), (600, 500, None));
            rpc(&mut app, &tx, &mut rx, m::BROWSER_STOP, json!({"workspace_id": ws})).unwrap();
        }
        Err(e) => {
            // It did not start: the host said why, and so does the panel.
            assert!(!real, "Chromium was asked for and did not start: {e:?}");
            assert!(e.message.contains("did not start"), "{e:?}");
            rpc(&mut app, &tx, &mut rx, m::BROWSER_LIST, json!({})).unwrap();
            let b = app.web.browsers.get(&ws).expect("browser.changed told the app");
            assert!(b.error.as_deref().is_some_and(|m| m.contains("did not start")), "{b:?}");
        }
    }
}

/// Opening a session, from the sidebar or as a new one starts, puts the keyboard in its
/// composer: no click needed before typing.
#[test]
fn opening_a_session_focuses_its_composer() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let focused = |ui: &rune::Runtime<model::App>, key: &str| ui.elements().iter().any(|e| e.key.as_deref() == Some(key) && e.focused);
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let read = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "session").unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
            s.approvals.clear();
        });
        ui.frame(0.016);
        ui.frame(0.016);

        // From the sidebar, with the composer focused before, and not.
        click(&mut ui, "prompt");
        assert!(focused(&ui, "prompt"), "{look:?}: a click focuses the composer");
        click(&mut ui, "ses_3");
        assert!(read(&mut ui, &|s| s.screen == Screen::Session("ses_3".into())), "{look:?}");
        assert!(focused(&ui, "prompt"), "{look:?}: opening ses_3 focused its composer");
        click(&mut ui, "ses_2");
        assert!(focused(&ui, "prompt"), "{look:?}: opening ses_2 focused its composer");
        ui.key(rune::KeyEvent { key: rune::Key::Text("h".into()), mods: rune::Mods::default() });
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.prompt.text == "h"), "{look:?}: typing goes into the composer");
        ui.update(|s, _| s.prompt.replace_text(""));

        // A new session, once the host has made it (a moment after the sheet closed).
        click(&mut ui, "nav-new");
        assert!(ui.find("start-session").is_some(), "{look:?}: the sheet is open");
        click(&mut ui, "start-session");
        assert!(ui.find("start-session").is_none(), "{look:?}: Start closed the sheet");
        for _ in 0..30 {
            ui.frame(0.016);
        }
        // The reply comes through the bridge, so the app takes it in `poll`.
        let (fake, from_host, _to_host) = bridge::Bridge::fake();
        let new = std::rc::Rc::new(std::cell::RefCell::new(None));
        let new2 = new.clone();
        ui.update(move |s, _| {
            let mut x = s.sessions.iter().find(|x| x.session_id == "ses_3").unwrap().clone();
            x.session_id = "ses_new".into();
            x.state = SessionState::Starting;
            x.title = None;
            x.pinned_at = None;
            *new2.borrow_mut() = Some(x);
            s.bridge = Some(fake);
        });
        let new = new.borrow_mut().take().unwrap();
        from_host.send(bridge::FromHost::Reply { tag: bridge::Tag::SessionCreated, result: Ok(serde_json::to_value(&new).unwrap()) }).unwrap();
        for _ in 0..5 {
            ui.frame(0.016);
        }
        assert!(read(&mut ui, &|s| s.screen == Screen::Session("ses_new".into())), "{look:?}");
        assert!(focused(&ui, "prompt"), "{look:?}: the new session's composer has the keyboard");
        ui.key(rune::KeyEvent { key: rune::Key::Text("h".into()), mods: rune::Mods::default() });
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.prompt.text == "h"), "{look:?}: typing goes into the new session's composer");
    }
}

/// A press anywhere on the composer's box that no button takes (its padding, the gaps in the
/// row of controls) puts the keyboard in the message, as the button goes down.
#[test]
fn a_press_anywhere_on_the_composer_box_focuses_the_message() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let focused = |ui: &rune::Runtime<model::App>, key: &str| ui.elements().iter().any(|e| e.key.as_deref() == Some(key) && e.focused);
    // Left, top, right, bottom.
    let rect = |ui: &rune::Runtime<model::App>, key: &str| {
        let [x, y, w, h] = ui.elements().iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key} is not on screen")).rect;
        [x, y, x + w, y + h]
    };
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "session").unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
            s.approvals.clear();
        });
        ui.frame(0.016);
        ui.frame(0.016);
        let (b, p, send) = (rect(&ui, "composer-box"), rect(&ui, "prompt"), rect(&ui, "send"));
        assert!(p[0] - b[0] >= 8.0 && p[1] - b[1] >= 8.0, "{look:?}: the box has padding around the text: {b:?} {p:?}");
        let mid_y = (send[1] + send[3]) / 2.0;
        let spots = [
            ("the top left corner's padding", b[0] + 3.0, b[1] + 3.0),
            ("the padding left of the text", b[0] + 3.0, (p[1] + p[3]) / 2.0),
            ("the padding above the text", (b[0] + b[2]) / 2.0, b[1] + 2.0),
            ("the right edge beside the text", b[2] - 3.0, (p[1] + p[3]) / 2.0),
            ("the gap in the row of controls", (b[0] + b[2]) / 2.0, mid_y),
            ("the bottom edge", (b[0] + b[2]) / 2.0, b[3] - 2.0),
        ];
        for (what, x, y) in spots {
            ui.update(|s, tx| {
                s.prompt.focused = false;
                tx.blur();
            });
            ui.frame(0.016);
            ui.frame(0.016);
            assert!(!focused(&ui, "prompt"), "{look:?}: nothing has focus before the press on {what}");
            ui.pointer_down(x, y);
            ui.frame(0.016);
            assert!(focused(&ui, "prompt"), "{look:?}: a press on {what} focuses the message as it goes down");
            ui.pointer_up(x, y);
            ui.frame(0.016);
            ui.key(rune::KeyEvent { key: rune::Key::Text("h".into()), mods: rune::Mods::default() });
            ui.frame(0.016);
            let typed = std::rc::Rc::new(std::cell::Cell::new(false));
            let typed2 = typed.clone();
            ui.update(move |s, _| {
                typed2.set(s.prompt.text == "h");
                s.prompt.replace_text("");
            });
            assert!(typed.get(), "{look:?}: typing after a press on {what} goes into the message");
            // A second press there, with the message focused, leaves the keyboard where it is.
            ui.pointer_down(x, y);
            ui.frame(0.016);
            ui.pointer_up(x, y);
            ui.frame(0.016);
            assert!(focused(&ui, "prompt"), "{look:?}: a press on {what} keeps the message focused");
        }
        // The buttons inside keep their own presses: the box isn't one itself.
        let roles: Vec<_> = ui.elements().into_iter().filter(|e| matches!(e.key.as_deref(), Some("composer-box" | "send"))).map(|e| (e.key.unwrap(), e.role)).collect();
        assert!(roles.contains(&("composer-box".to_string(), "textbox")) && roles.contains(&("send".to_string(), "button")), "{look:?}: {roles:?}");
        let centre = |ui: &rune::Runtime<model::App>, key: &str| {
            let r = rect(ui, key);
            ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0)
        };
        let click = |ui: &mut rune::Runtime<model::App>, (x, y): (f32, f32)| {
            ui.pointer_down(x, y);
            ui.frame(0.016);
            ui.pointer_up(x, y);
            ui.frame(0.016);
            ui.frame(0.016);
        };
        let state = |ui: &mut rune::Runtime<model::App>, f: fn(&model::App) -> bool| {
            let r = std::rc::Rc::new(std::cell::Cell::new(false));
            let r2 = r.clone();
            ui.update_if(move |s, _| {
                r2.set(f(s));
                false
            });
            r.get()
        };
        for key in ["attach", "mic", "model-chip", "send"] {
            let (x, y) = centre(&ui, key);
            let at = ui.element_at(x, y);
            let path = at.as_ref().map(|p| p.path().to_string()).unwrap_or_default();
            assert!(path.contains(&format!("/{key}")), "{look:?}: a press on {key} lands on it, not the box: {path}");
        }
        // Attach opens the file picker.
        assert!(ui.file_dialog().is_none(), "{look:?}");
        let at = centre(&ui, "attach");
        click(&mut ui, at);
        assert!(ui.file_dialog().is_some(), "{look:?}: Attach opens the file picker");
        ui.resolve_file_dialog(None).unwrap();
        ui.frame(0.016);
        // The microphone answers: it records, or a toast says why it can't here.
        ui.update(|s, _| {
            s.toasts.clear();
            s.ask_voice_download = false;
        });
        assert!(state(&mut ui, |s| matches!(s.mic, model::Mic::Idle)), "{look:?}");
        let at = centre(&ui, "mic");
        click(&mut ui, at);
        assert!(state(&mut ui, |s| !s.toasts.is_empty() || s.ask_voice_download || !matches!(s.mic, model::Mic::Idle)), "{look:?}: the microphone button ran");
        ui.update(|s, _| {
            s.mic_cancel();
            s.toasts.clear();
            s.ask_voice_download = false;
        });
        ui.frame(0.016);
        // The model chip opens its menu.
        assert!(state(&mut ui, |s| s.menu.is_none()), "{look:?}");
        let at = centre(&ui, "model-chip");
        click(&mut ui, at);
        assert!(state(&mut ui, |s| s.menu == Some(model::Menu::Model)), "{look:?}: the model chip opens its menu");
    }
}

/// A session's pull request icon says where it stands: green open, purple merged, red
/// closed without merging, grey before the host has looked; the tooltip says it in words.
#[test]
fn pull_request_icons_follow_their_state() {
    use crate::look::{Look, Theme};
    for look in [Look::Mac, Look::Gnome] {
        let t = Theme::new(look);
        assert_eq!(crate::view::pr_color(&t, Some(PullRequestState::Open)), t.ok);
        assert_eq!(crate::view::pr_color(&t, Some(PullRequestState::Merged)), t.pr);
        assert_eq!(crate::view::pr_color(&t, Some(PullRequestState::Closed)), t.danger);
        assert_eq!(crate::view::pr_color(&t, None), t.text2);
    }
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let mut ui = crate::app().headless_runtime((1280.0, 1000.0), rune::Env::default());
    let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "sidebar").unwrap().1;
    ui.update(move |s, _| f(s));
    ui.frame(0.016);
    ui.frame(0.016);
    let help = |id: &str| ui.elements().into_iter().find(|e| e.key.as_deref() == Some(id)).and_then(|e| e.label).unwrap_or_default();
    assert!(help("ses_s5").contains("Pull request #25, merged"), "{}", help("ses_s5"));
    assert!(help("ses_s6").contains("Pull request #26, closed"), "{}", help("ses_s6"));
    assert!(help("ses_3").contains("Pull request #48483, open"), "{}", help("ses_3"));
}

/// Stopping a turn is the composer's stop button (and ⌘. / Ctrl+.), not the toolbar's nor
/// the session menu's.
#[test]
fn no_stop_button_in_the_toolbar_or_the_session_menu() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "session-menu").unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
            // ses_1 working, so a stop would be on offer.
            let x = s.sessions.iter_mut().find(|x| x.session_id == "ses_1").unwrap();
            x.state = SessionState::Running;
        });
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(ui.find("stop-turn").is_none(), "{look:?}: the toolbar has no stop button");
        assert!(ui.find("session-menu").is_some(), "{look:?}");
        let labels: Vec<String> = ui.elements().into_iter().filter_map(|e| e.label.or(e.text)).collect();
        assert!(labels.iter().any(|l| l == "Show Session Folder"), "{look:?}: the menu is open: {labels:?}");
        assert!(!labels.iter().any(|l| l.contains("Stop the Turn") && l != "Stop the Turn"), "{look:?}: {labels:?}");
        // The composer's send button is the one stop left.
        let send = ui.elements().into_iter().find(|e| e.key.as_deref() == Some("send")).unwrap();
        assert_eq!(send.label.as_deref(), Some("Stop the Turn"), "{look:?}");
        assert_eq!(labels.iter().filter(|l| l.contains("Stop the Turn")).count(), 1, "{look:?}: only the composer's: {labels:?}");
    }
}

/// An attached image shows a small picture of itself in its chip, so two pasted images can
/// be told apart; other files keep the file mark.
#[test]
fn attached_images_show_a_picture_in_the_composer() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
    let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "composer-paste").unwrap().1;
    // Decoded off the UI thread; waited for here, as the app's waker would redraw.
    let decoded = |s: &mut model::App| {
        for f in s.files.values().flatten() {
            if let Some(img) = &f.preview {
                let _ = img.wait();
            }
        }
    };
    ui.update(move |s, _| {
        f(s);
        decoded(s);
    });
    ui.frame(0.016);
    ui.frame(0.016);
    assert!(ui.find("file-preview-1").is_some() && ui.find("file-preview-2").is_some(), "both pasted images show a picture");
    let rect = |key: &str| ui.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect;
    let (pic, chip) = (rect("file-preview-1"), rect("file-1"));
    assert!(pic[2] <= 56.0 && pic[3] <= 32.0 && pic[2] >= 12.0 && pic[3] >= 12.0, "{pic:?}");
    assert!(pic[0] >= chip[0] && pic[1] >= chip[1] && pic[1] + pic[3] <= chip[1] + chip[3], "{pic:?} in {chip:?}");

    // Pasting makes one too; a picked text file doesn't.
    ui.update(|s, _| {
        s.info.as_mut().unwrap().protocol_version = 7;
        s.files.clear();
        s.attach_bytes(crate::brand::APP_ICON_PNG.to_vec(), "image/png");
        s.attach(&[std::path::PathBuf::from("Cargo.toml")]);
        decoded(s);
    });
    let ids: Vec<u64> = {
        let r = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let r2 = r.clone();
        ui.update_if(move |s, _| {
            *r2.borrow_mut() = s.files.get("ses_1").unwrap().iter().map(|f| f.local_id).collect();
            false
        });
        let v = r.borrow().clone();
        v
    };
    assert_eq!(ids.len(), 2);
    ui.frame(0.016);
    assert!(ui.find(&format!("file-preview-{}", ids[0])).is_some(), "the pasted image shows a picture");
    assert!(ui.find(&format!("file-{}", ids[1])).is_some());
    assert!(ui.find(&format!("file-preview-{}", ids[1])).is_none(), "a text file has no picture");
}

/// H1: a push approval says where the remote really points, because a
/// repository's own git settings can send an approved "push to origin"
/// somewhere else. An approval that carries no resolved URL (an older host,
/// or a tool where it doesn't apply) shows no such line.
#[test]
fn a_push_approval_shows_where_the_remote_really_points() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look| {
        let mut ui = crate::app().headless_runtime((1280.0, 1000.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
        });
        ui.frame(0.016);
        ui.frame(0.016);
        ui
    };
    for look in [Look::Mac, Look::Gnome] {
        let ui = open("push-approval", look);
        assert!(ui.find("approve").is_some(), "{look:?}: no approval on screen");
        assert!(
            ui.find("approval-url").is_some(),
            "{look:?}: the push approval must show the remote's resolved URL"
        );
        // Installing a tool has no remote, so there is nothing to resolve.
        let ui = open("install-approval", look);
        assert!(ui.find("approve").is_some(), "{look:?}: no approval on screen");
        assert!(ui.find("approval-url").is_none(), "{look:?}: nothing to show there");
    }
}

/// Settings fill the window: their sidebar in the sessions' place and the page in the rest,
/// a list beside its detail where there's room and over it where there isn't. Back, Escape
/// and the Settings key give the window back as it was.
#[test]
fn settings_take_the_window_and_give_it_back() {
    use crate::look::Look;
    use crate::model::Tab;
    use rune::{Key, KeyEvent, Mods};
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let open = |scene: &'static str, look: Look, size: (f32, f32)| {
        let mut ui = crate::app().headless_runtime(size, rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
            s.screen = Screen::Session("ses_1".into());
        });
        for _ in 0..4 {
            ui.frame(0.016);
        }
        ui
    };
    let rect = |ui: &rune::Runtime<model::App>, key: &str| ui.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key} is not on screen")).rect;
    let read = |ui: &mut rune::Runtime<model::App>, f: &dyn Fn(&model::App) -> bool| {
        let r = std::rc::Rc::new(std::cell::Cell::new(false));
        let r2 = r.clone();
        ui.update_if(|s, _| {
            r2.set(f(s));
            false
        });
        r.get()
    };
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let escape = |ui: &mut rune::Runtime<model::App>| {
        ui.key(KeyEvent { key: Key::Escape, mods: Mods::default() });
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let shown = ["settings-sidebar", "settings-back", "settings-pages", "tab-general", "tab-agents", "tab-workspaces", "tab-connectors", "tab-devices", "tab-archived", "host-line", "settings", "settings-toolbar", "settings-title", "settings-body"];
    let gone = ["sidebar", "session-list", "main", "toolbar", "panel", "settings-layer"];
    for look in [Look::Mac, Look::Gnome] {
        let side_w = crate::look::Theme::new(look).sidebar_w;
        let list_w = if look.mac() { 240.0 } else { 250.0 };
        let mut ui = open("settings-connectors", look, (1280.0, 800.0));
        for k in shown {
            assert!(ui.find(k).is_some(), "{look:?}: {k} is not on screen");
        }
        for k in gone {
            assert!(ui.find(k).is_none(), "{look:?}: {k} should not be on screen");
        }
        // The sidebar keeps the session sidebar's place and width; the page has the rest.
        let (side, pane) = (rect(&ui, "settings-sidebar"), rect(&ui, "settings"));
        assert_eq!(side, [0.0, 0.0, side_w, 800.0], "{look:?}");
        assert_eq!((pane[0], pane[0] + pane[2], pane[3]), (side_w, 1280.0, 800.0), "{look:?}: {pane:?}");
        // With room, the connectors are listed beside the chosen one's detail.
        let (list, name) = (rect(&ui, "source-list"), rect(&ui, "conn-name"));
        assert_eq!(list[2], list_w, "{look:?}: {list:?}");
        assert!(name[0] >= list[0] + list[2] && name[1] < list[1] + list[3], "{look:?}: the detail is beside the list: {name:?} {list:?}");
        // The rows are the pages.
        click(&mut ui, "tab-devices");
        assert!(read(&mut ui, &|s| s.settings == Some(Tab::Devices)), "{look:?}");
        assert!(ui.find("devices").is_some() && ui.find("connectors").is_none(), "{look:?}: the page follows the row");
        // Escape gives the window back as it was: the sessions, and the one that was open.
        escape(&mut ui);
        assert!(read(&mut ui, &|s| s.settings.is_none() && s.screen == Screen::Session("ses_1".into())), "{look:?}: Escape leaves Settings");
        for k in ["sidebar", "session-list", "main", "toolbar"] {
            assert!(ui.find(k).is_some(), "{look:?}: {k} is back");
        }
        assert!(ui.find("settings-sidebar").is_none() && ui.find("settings").is_none(), "{look:?}");
        // The Settings key opens them, and leaves them.
        ui.update(|s, tx| commands::run(s, Cmd::Settings, tx));
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.settings == Some(Tab::General)), "{look:?}");
        ui.update(|s, tx| commands::run(s, Cmd::Settings, tx));
        ui.frame(0.016);
        assert!(read(&mut ui, &|s| s.settings.is_none()), "{look:?}: the key leaves Settings too");
        // And so does Back.
        ui.update(|s, tx| commands::run(s, Cmd::Settings, tx));
        ui.frame(0.016);
        ui.frame(0.016);
        click(&mut ui, "settings-back");
        assert!(read(&mut ui, &|s| s.settings.is_none() && s.screen == Screen::Session("ses_1".into())), "{look:?}: Back leaves Settings");

        // Pairing is a sheet over the whole window. Escape puts it away first, then Settings.
        let mut ui = open("pairing", look, (1280.0, 800.0));
        let (layer, sheet) = (rect(&ui, "pair-layer"), rect(&ui, "pairing"));
        assert_eq!(layer, [0.0, 0.0, 1280.0, 800.0], "{look:?}");
        assert!(((sheet[0] + sheet[2] / 2.0) - 640.0).abs() < 1.0, "{look:?}: the sheet is centred in the window: {sheet:?}");
        assert!(ui.find("settings-sidebar").is_some(), "{look:?}");
        escape(&mut ui);
        assert!(read(&mut ui, &|s| s.pairing.is_none() && s.settings == Some(Tab::Devices)), "{look:?}: Escape closes the sheet, not Settings");
        assert!(ui.find("pair-layer").is_none(), "{look:?}");
        escape(&mut ui);
        assert!(read(&mut ui, &|s| s.settings.is_none()), "{look:?}");

        // The smallest window: the sidebar gives way so the page keeps its width, and the
        // list sits over the detail.
        for (scene, row, detail) in [("settings-connectors", "conn-con_gh", "conn-name"), ("settings-workspaces", "ws-ws_relay", "workspaces")] {
            let ui = open(scene, look, (760.0, 520.0));
            let (side, pane) = (rect(&ui, "settings-sidebar"), rect(&ui, "settings"));
            assert_eq!((side[2], pane[0], pane[2]), (220.0, 220.0, 540.0), "{look:?} {scene}");
            let (list, item) = (rect(&ui, "source-list"), rect(&ui, row));
            assert!(list[2] > 400.0 && item[3] > 20.0 && item[1] >= list[1] && item[1] + item[3] <= list[1] + list[3], "{look:?} {scene}: the list is as wide as the page and holds its rows: {list:?} {item:?}");
            if detail == "conn-name" {
                let name = rect(&ui, detail);
                assert!(name[1] >= list[1] + list[3], "{look:?} {scene}: the detail is under the list: {name:?} {list:?}");
            }
        }
    }
}

/// A word too long for its line (an encoded command, a hash, a URL) breaks inside the word
/// wherever a session shows text: nothing is wider than the column it is in, in either look.
#[test]
fn a_word_too_long_for_its_line_breaks_inside_the_column() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 6000.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "session-long-words").unwrap().1;
        ui.update(move |s, _| {
            f(s);
            s.look = look;
        });
        ui.frame(0.016);
        ui.frame(0.016);
        let els = ui.elements();
        let of = |key: &str| els.iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{look:?}: {key} is not on screen"));
        // What scrolls sideways on purpose (a code block, a wide table) may be wider inside.
        let sideways: Vec<&str> = els.iter().filter(|e| e.scroll_axes[0]).map(|e| e.path.as_str()).collect();
        let (mut seen, mut wrong) = (0, vec![]);
        for (within, name) in [("transcript-rows", "the transcript"), ("composer", "the composer")] {
            let b = of(within);
            let (left, right) = (b.rect[0] - 0.5, b.rect[0] + b.rect[2] + 0.5);
            for e in els.iter().filter(|e| e.path.starts_with(&format!("{}/", b.path)) && !sideways.iter().any(|p| e.path.starts_with(&format!("{p}/")))) {
                let at = format!("{:?} at {} in {name}", e.text.as_deref().map(|t| t.chars().take(40).collect::<String>()), e.path);
                if e.rect[0] < left || e.rect[0] + e.rect[2] > right {
                    wrong.push(format!("{at} spans {}..{}, outside {left}..{right}", e.rect[0], e.rect[0] + e.rect[2]));
                }
                // Its words stay inside its own box too (unless it ends in an ellipsis on purpose).
                if let (Some(size), false) = (e.text_size, e.truncate) {
                    let room = e.rect[2] - e.inset[0] - e.inset[2];
                    if size[0] > room + 0.5 {
                        wrong.push(format!("{at}: its text is {} wide in a box with room for {room}", size[0]));
                    }
                    seen += e.text.as_deref().is_some_and(|t| t.contains(&crate::demo::LONG_WORD[..40])) as usize;
                }
            }
        }
        assert!(wrong.is_empty(), "{look:?}: wider than its column:\n{}", wrong.join("\n"));
        // The prompt, its unsent twin, the thought, the command's output, the read's path, the
        // reply's paragraph and list, the approval's lines and the message being written.
        assert!(seen >= 9, "{look:?}: only {seen} texts with the long word were checked");
    }
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// However many extra hosts a workspace has, Add Host… stays in its row: the hosts wrap onto
/// lines of their own under it, and a long name ends in an ellipsis.
#[test]
fn extra_hosts_wrap_and_keep_add_host_in_reach() {
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    for look in [Look::Mac, Look::Gnome] {
        // A wide window, and one narrow enough that the list and the detail stack.
        for width in [1280.0, 640.0] {
            let mut ui = crate::app().headless_runtime((width, 1600.0), rune::Env::default());
            let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "settings-workspaces-hosts").unwrap().1;
            ui.update(move |s, _| {
                f(s);
                s.look = look;
                s.admin = true;
            });
            ui.frame(0.016);
            ui.frame(0.016);
            let inside = |ui: &rune::Runtime<model::App>, what: &str| {
                let els = ui.elements();
                let of = |key: &str| els.iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{look:?} {width}: {key} is not on screen ({what})"));
                let b = of("ws_relay-extra-hosts");
                // The row is as wide as the rows above it, and everything in it is inside it.
                let above = of("ws_relay-Github");
                let up = els.iter().find(|e| above.path.strip_prefix(&e.path).is_some_and(|rest| rest.starts_with('/') && !rest[1..].contains('/'))).unwrap();
                assert!((b.rect[0] - up.rect[0]).abs() < 0.5 && (b.rect[2] - up.rect[2]).abs() < 0.5, "{look:?} {width} ({what}): the row spans {:?}, the one above {:?}", b.rect, up.rect);
                let (left, right) = (b.rect[0] - 0.5, b.rect[0] + b.rect[2] + 0.5);
                for e in els.iter().filter(|e| e.path.starts_with(&format!("{}/", b.path))) {
                    assert!(e.rect[0] >= left && e.rect[0] + e.rect[2] <= right, "{look:?} {width} ({what}): {} ({:?}) spans {}..{}, outside {left}..{right}", e.path, e.text, e.rect[0], e.rect[0] + e.rect[2]);
                }
                assert_eq!(els.iter().filter(|e| e.path.starts_with(&format!("{}/", of("ws_relay-hosts").path)) && e.role == "button").count(), 9, "{look:?} {width} ({what}): every host can be removed");
            };
            inside(&ui, "at rest");
            // Add Host… is there to press, and its field opens in the same place.
            let (x, y) = ui.find("ws_relay-add-host").unwrap_or_else(|| panic!("{look:?} {width}: Add Host… is not on screen"));
            ui.pointer_down(x, y);
            ui.pointer_up(x, y);
            ui.frame(0.016);
            ui.frame(0.016);
            assert!(ui.find("ws_relay-host-field").is_some(), "{look:?} {width}: the field did not open");
            inside(&ui, "adding");
        }
    }
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// The right panel's leading edge is a line you can see: lighter than the panes on both sides
/// of it (a tab's body can be the colour of the main pane) and drawn over the tab's body, with
/// the grip that resizes the panel on it.
#[test]
fn the_right_panels_edge_is_a_line_you_can_see() {
    use crate::look::{Look, Theme};
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    for look in [Look::Mac, Look::Gnome] {
        for scene in ["terminal", "browser"] {
            let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
            let f = crate::demo::SCENES.iter().find(|(n, _)| *n == scene).unwrap().1;
            ui.update(move |s, _| {
                f(s);
                s.look = look;
            });
            ui.frame(0.016);
            ui.frame(0.016);
            let els = ui.elements();
            let of = |key: &str| els.iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{look:?} {scene}: {key} is not on screen"));
            let (panel, grip, main, edge) = (of("panel"), of("panel-grip"), of("main"), of("panel-edge"));
            // Opaque, so it looks the same whatever is under it, and lighter than the content.
            let line = edge.border_color.unwrap_or_else(|| panic!("{look:?} {scene}: the edge draws nothing"));
            let content = Theme::new(look).content;
            assert!(main.fill == content, "{look:?} {scene}: the main pane is not the content colour");
            assert!(line.a == 1.0 && line.luminance() > content.luminance() * 2.0, "{look:?} {scene}: the edge ({}) is lost against the content ({})", line.hex(), content.hex());
            // It runs down the panel's leading edge, over the tab's body (which has a fill of
            // its own, and comes first), and the grip is centred on it.
            assert!(edge.rect[0] == panel.rect[0] && edge.rect[1] == panel.rect[1] && edge.rect[3] == panel.rect[3], "{look:?} {scene}: edge {:?}, panel {:?}", edge.rect, panel.rect);
            let at = |key: &str| els.iter().position(|e| e.key.as_deref() == Some(key)).unwrap();
            let body = if scene == "terminal" { "term-body" } else { "browser-body" };
            assert!(of(body).rect[0] == panel.rect[0] && at("panel-edge") > at(body), "{look:?} {scene}: the body is drawn over the edge");
            assert!((grip.rect[0] + grip.rect[2] / 2.0 - edge.rect[0]).abs() <= 0.5 && grip.rect[3] == panel.rect[3], "{look:?} {scene}: grip {:?}, edge {:?}", grip.rect, edge.rect);
        }
    }
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// A blocked host reads as allowed once the workspace's extra hosts cover it, by name or by
/// a wildcard (`*.crates.io`), as the host's proxy would decide; otherwise it offers to allow it.
#[test]
fn a_blocked_host_is_allowed_once_a_wildcard_covers_it() {
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    // The scene's session was refused index.crates.io:443.
    for (hosts, allowed) in [
        (vec![], false),
        (vec!["index.crates.io"], true),
        (vec!["*.crates.io"], true),
        (vec!["*.CRATES.io:443"], true),
        (vec!["*.crates.io:8443"], false),
        (vec!["crates.io", "*.index.crates.io", "*.s.io"], false),
    ] {
        let mut ui = crate::app().headless_runtime((1280.0, 1600.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "session").unwrap().1;
        let list: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
        ui.update(move |s, _| {
            f(s);
            let ws = s.session("ses_1").unwrap().workspace_id.clone();
            let w = s.workspaces.iter_mut().find(|w| w.workspace_id == ws).unwrap();
            w.network = NetworkProfile { presets: vec![], custom_hosts: list.clone() };
        });
        ui.frame(0.016);
        ui.frame(0.016);
        let els = ui.elements();
        let has = |t: &str| els.iter().any(|e| e.text.as_deref().is_some_and(|x| x.starts_with(t)));
        assert!(has("Blocked index.crates.io"), "{hosts:?}: the blocked host is not shown");
        assert_eq!(has("Allowed"), allowed, "{hosts:?}");
        assert_eq!(has("Allow for"), !allowed, "{hosts:?}");
    }
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}

/// A site the browser was refused is asked about in the Browser tab, newest first: Allow adds
/// it to the workspace's network rules and reloads the page, Don't Allow keeps quiet about it
/// until you ask again, and what the rules cover (a wildcard, the open web) is never asked.
#[test]
fn the_browser_asks_before_a_site_joins_the_workspaces_rules() {
    use crate::browser::SiteAnswer;
    use crate::look::Look;
    let _turn = HOST_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AGENTHOST_DESKTOP_OFFLINE", "1");
    let click = |ui: &mut rune::Runtime<model::App>, key: &str| {
        let (x, y) = ui.find(key).unwrap_or_else(|| panic!("{key} is not on screen"));
        ui.pointer_down(x, y);
        ui.pointer_up(x, y);
        ui.frame(0.016);
        ui.frame(0.016);
    };
    let says = |ui: &rune::Runtime<model::App>, key: &str, what: &str| {
        let els = ui.elements();
        let Some(bar) = els.iter().find(|e| e.key.as_deref() == Some(key)) else { return false };
        els.iter().any(|e| e.path.starts_with(&format!("{}/", bar.path)) && e.text.as_deref().is_some_and(|t| t.contains(what)))
    };
    let blocked = |ui: &mut rune::Runtime<model::App>, hosts: &[&str]| {
        let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
        ui.update(move |s, _| {
            let mut b = s.web.browsers["ws_relay"].clone();
            b.blocked = hosts.clone();
            s.browser_changed(b);
        });
        ui.frame(0.016);
        ui.frame(0.016);
    };
    for look in [Look::Mac, Look::Gnome] {
        let mut ui = crate::app().headless_runtime((1280.0, 800.0), rune::Env::default());
        let f = crate::demo::SCENES.iter().find(|(n, _)| *n == "browser").unwrap().1;
        ui.update(move |s, _| {
            s.look = look;
            f(s);
        });
        ui.frame(0.016);
        ui.frame(0.016);
        // The scene's browser was refused fonts.gstatic.com: asked about under the address bar,
        // inside the panel.
        for k in ["browser-ask", "browser-allow", "browser-deny"] {
            assert!(ui.find(k).is_some(), "{look:?}: {k} is not on screen");
        }
        assert!(says(&ui, "browser-ask", "fonts.gstatic.com") && ui.find("browser-declined").is_none(), "{look:?}");
        let els = ui.elements();
        let of = |key: &str| els.iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect;
        let (ask, panel, bar, view) = (of("browser-ask"), of("panel"), of("browser-bar"), of("browser-view"));
        assert!(ask[0] >= panel[0] && ask[0] + ask[2] <= panel[0] + panel[2] + 0.5 && ask[1] >= bar[1] + bar[3] - 0.5 && ask[1] + ask[3] <= view[1] + 0.5, "{look:?}: ask {ask:?}, bar {bar:?}, view {view:?}");
        for k in ["browser-allow", "browser-deny"] {
            let b = of(k);
            assert!(b[0] >= ask[0] && b[0] + b[2] <= ask[0] + ask[2] + 0.5, "{look:?}: {k} {b:?} is outside the bar {ask:?}");
        }

        // Don't Allow: nothing is sent, and the site waits quietly until Ask Again.
        click(&mut ui, "browser-deny");
        assert!(ui.find("browser-ask").is_none() && says(&ui, "browser-declined", "fonts.gstatic.com"), "{look:?}");
        ui.update(|s, _| assert!(s.web.calls.is_empty() && s.web.answers["ws_relay"]["fonts.gstatic.com"] == SiteAnswer::Declined));
        click(&mut ui, "browser-ask-again");
        assert!(says(&ui, "browser-ask", "fonts.gstatic.com") && ui.find("browser-declined").is_none(), "{look:?}");

        // Another site is refused: the newest is asked about first, and the bar says one more waits.
        blocked(&mut ui, &["cdn.example.com:8443", "fonts.gstatic.com"]);
        assert!(says(&ui, "browser-ask", "cdn.example.com:8443") && says(&ui, "browser-ask", "1 more"), "{look:?}");

        // Allow: the host adds it to the workspace's rules, and the next site is asked about.
        click(&mut ui, "browser-allow");
        ui.update(|s, _| {
            assert_eq!(s.web.calls, vec![(m::WORKSPACE_UPDATE, json!({"workspace_id": "ws_relay", "add_custom_host": "cdn.example.com:8443"}))]);
            assert!(s.web.answers["ws_relay"]["cdn.example.com:8443"] == SiteAnswer::Allowing);
        });
        assert!(says(&ui, "browser-ask", "fonts.gstatic.com") && !says(&ui, "browser-ask", "more"), "{look:?}");
        // The host's answer is the workspace with its new rule: the page on show is reloaded.
        ui.update(|s, _| {
            let mut w = s.workspace("ws_relay").unwrap().clone();
            w.network.custom_hosts.push("cdn.example.com:8443".into());
            s.apply(bridge::FromHost::Reply { tag: bridge::Tag::BrowserSiteAllowed { workspace_id: "ws_relay".into(), host: "cdn.example.com:8443".into() }, result: Ok(serde_json::to_value(&w).unwrap()) });
            assert!(s.workspace("ws_relay").unwrap().network.names("cdn.example.com", 8443));
            assert_eq!(s.web.calls.last().unwrap(), &(m::BROWSER_NAVIGATE, json!({"tab_id": "tab_agent", "action": "reload"})));
            assert!(!s.web.answers.get("ws_relay").is_some_and(|a| a.contains_key("cdn.example.com:8443")));
        });
        ui.frame(0.016);
        assert!(says(&ui, "browser-ask", "fonts.gstatic.com"), "{look:?}");

        // The host refuses: the reason is shown and the site is asked about again.
        click(&mut ui, "browser-allow");
        assert!(ui.find("browser-ask").is_none(), "{look:?}: nothing else waits while it is being allowed");
        ui.update(|s, _| {
            let refused = agenthost_proto::jsonrpc::RpcError { code: -32602, message: "no such workspace".into(), data: None };
            s.apply(bridge::FromHost::Reply { tag: bridge::Tag::BrowserSiteAllowed { workspace_id: "ws_relay".into(), host: "fonts.gstatic.com".into() }, result: Err(refused) });
            assert_eq!(s.error.take().as_deref(), Some("Could not allow fonts.gstatic.com: no such workspace"));
        });
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(says(&ui, "browser-ask", "fonts.gstatic.com"), "{look:?}");

        // What the rules cover is never asked about: a wildcard, then the open web.
        let rules = |ui: &mut rune::Runtime<model::App>, presets: Vec<NetworkPreset>, hosts: &[&str]| {
            let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
            ui.update(move |s, _| s.workspaces.iter_mut().find(|w| w.workspace_id == "ws_relay").unwrap().network = NetworkProfile { presets: presets.clone(), custom_hosts: hosts.clone() });
            ui.frame(0.016);
            ui.frame(0.016);
        };
        rules(&mut ui, vec![], &["*.gstatic.com", "*.example.com:8443"]);
        assert!(ui.find("browser-ask").is_none() && ui.find("browser-declined").is_none(), "{look:?}: a wildcard covers them");
        rules(&mut ui, vec![], &["*.gstatic.com", "*.example.com"]);
        assert!(ui.find("browser-ask").is_none(), "{look:?}: any port");
        rules(&mut ui, vec![], &["gstatic.com"]);
        assert!(says(&ui, "browser-ask", "cdn.example.com:8443"), "{look:?}: the name alone covers neither");
        rules(&mut ui, vec![NetworkPreset::Web], &[]);
        assert!(ui.find("browser-ask").is_none(), "{look:?}: the open web covers them");

        // With the panel closed, a newly refused site says so in a toast whose button opens the
        // Browser tab; with the tab on screen the bar is enough.
        rules(&mut ui, vec![], &[]);
        ui.update(|s, _| {
            let before = s.toasts.len();
            let mut b = s.web.browsers["ws_relay"].clone();
            b.blocked.insert(0, "seen.example.org".into());
            s.browser_changed(b.clone());
            assert_eq!(s.toasts.len(), before, "the bar is on screen");
            s.panel_open = false;
            b.blocked.insert(0, "new.example.org".into());
            s.browser_changed(b.clone());
            let t = s.toasts.last().unwrap().clone();
            assert!(t.text.contains("new.example.org") && !t.text.contains("seen.example.org") && t.action == Some(model::ToastAction::ShowBrowser), "{t:?}");
            // Told once, not on every change.
            s.browser_changed(b);
            assert_eq!(s.toasts.len(), before + 1);
        });
        ui.update(|s, tx| {
            let id = s.toasts.last().unwrap().id;
            s.toast_action(id, tx);
            assert!(s.panel_shown() == Some(model::PanelTab::Browser));
        });
        ui.frame(0.016);
        ui.frame(0.016);
        assert!(says(&ui, "browser-ask", "new.example.org"), "{look:?}");
    }
    std::env::remove_var("AGENTHOST_DESKTOP_OFFLINE");
}
