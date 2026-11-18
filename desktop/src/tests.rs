//! The desktop's model and bridge against a real host (in process, fake agent).

use crate::bridge;
use crate::commands::{self, Cmd};
use crate::model::{self, Conn, Screen};
use agenthost_proto::*;
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
    app.bridge = Some(bridge::start(Arc::new(|| {})));
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
    pump(&mut app, |a| {
        a.transcripts.get(&sid).map(|t| t.events.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: hello desktop")).unwrap_or(false)
    });
    // Seqs are contiguous: nothing missed, nothing doubled.
    let seqs: Vec<i64> = app.transcripts[&sid].events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    assert!(app.prompt.text.is_empty());
    assert!(app.error.is_none(), "{:?}", app.error);
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
    app.bridge = Some(bridge::start(Arc::new(|| {})));
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
    app.bridge = Some(bridge::start(Arc::new(|| {})));
    pump(&mut app, |a| a.conn == Conn::Connected && a.info.is_some() && !a.workspaces.is_empty());
    app
}

/// ⌘N's sheet: pick a workspace and a login, type a first message, Start. The session opens
/// and the message goes as its first prompt.
#[test]
fn the_new_session_sheet_starts_a_session_with_its_first_message() {
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
    app.first_prompt.replace_text("hello from the sheet");
    app.start_from_sheet();
    assert!(!app.new_open, "the sheet closes on Start");
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

    // Add the server as Settings › Connectors does: URL and a bearer token.
    app.conn_url.replace_text(&server.url);
    app.conn_token.replace_text("s3cret-desktop-token");
    app.add_connector();
    pump(&mut app, |a| a.connectors.iter().any(|c| c.status == "ok" && !c.tools.is_empty()));
    let conn = app.connectors[0].clone();
    assert_eq!(app.sel_conn.as_deref(), Some(conn.connector_id.as_str()));

    app.open_new_session(true);
    app.new_agent = Some("fake".into());
    app.first_prompt.replace_text(&format!(r#"mcp {}__create_note {{"text":"buy milk"}}"#, connector_slug(&conn.name)));
    app.start_from_sheet();
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
    assert_eq!(names, ["Quick tasks", "relay", "notes"], "Quick tasks first; empty workspaces left out");
    // Ended sessions get no number.
    assert_eq!(app.numbered(), ["ses_q", "ses_1", "ses_2", "ses_3"]);
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
