//! The desktop's model and bridge against a real host (in process, fake agent).

use crate::bridge;
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
    frames(&mut ui, &|u| u.find("nav-workspaces").is_some(), "first frame");
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
