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
    assert_eq!(names, ["Quick tasks", "relay", "notes"], "Quick tasks first; empty workspaces left out");
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
    assert!(app.side_open && app.side_visible());
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
    assert!(!app.side_visible() && !app.side_open);
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
fn the_header_title_falls_back_to_the_workspace() {
    let app = demo("header-titled");
    let x = app.session("ses_1").unwrap();
    assert_eq!(app.session_title(x), "Fix the flaky resume test");
    assert_eq!(app.session_context(x), "Claude Code · Work · relay");
    let app = demo("header-quick-untitled");
    let x = app.session("ses_q").unwrap();
    assert_eq!(app.session_title(x), "Quick task");
    assert_eq!(app.session_context(x), "Claude Code · Personal", "the title already says where");
    let app = demo("header-untitled");
    let x = app.session("ses_2").unwrap();
    assert_eq!(app.session_title(x), "relay");
    assert!(!app.session_title(x).contains("Codex"), "never the login");
    let mut blank = x.clone();
    blank.title = Some("   ".into());
    assert_eq!(app.session_title(&blank), "relay");
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
    assert_eq!(app.session_title(app.session(&sid).unwrap()), "Quick task");
    commands::run(&mut app, Cmd::RenameSession, &mut tx);
    app.title_edit.replace_text("  Tidy the build  ");
    app.save_session_rename();
    pump(&mut app, |a| a.session_rename.is_none() && a.session(&sid).is_some_and(|s| s.title.as_deref() == Some("Tidy the build")));
    commands::run(&mut app, Cmd::RenameSession, &mut tx);
    app.title_edit.replace_text("");
    app.save_session_rename();
    pump(&mut app, |a| a.session_rename.is_none() && a.session(&sid).is_some_and(|s| s.title.is_none()));
    assert_eq!(app.session_title(app.session(&sid).unwrap()), "Quick task");
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
    app.side_open = true;
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
    pump(&mut app, |a| a.builtins.len() == 3);
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
    assert!(!offered("session"), "a connector's tool has its own policies");
    // Pushes already run unasked in the base demo: nothing to offer.
    let mut app = demo("push-approval");
    app.builtins[1].policy = ToolPolicy::Auto;
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
