//! The browser against a real Chromium, in process (no socket, no gateway).
//!
//! Chromium can't start inside an agent session's sandbox (its own sandbox
//! needs user namespaces, and it needs Unix sockets), so this runs only when
//! asked: `AGENTHOST_TEST_BROWSER=1 cargo test -p agenthost-host --test browser_core`.
//! `AGENTHOST_TEST_BROWSER_NET=1` also opens https://example.com through the
//! workspace's proxy.

mod common;

use agenthost_host::Host;
use agenthost_proto::*;
use base64::Engine as _;
use serde_json::json;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn wanted() -> bool {
    if std::env::var_os("AGENTHOST_TEST_BROWSER").is_none() {
        eprintln!("skipped: set AGENTHOST_TEST_BROWSER=1 to run against a real Chromium");
        return false;
    }
    assert!(agenthost_host::browser::find_browser().is_some(), "no Chromium-based browser installed");
    true
}

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

fn open(root: &std::path::Path) -> (Arc<Host>, String) {
    let cfg = common::test_config(root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let host = Host::open(cfg).unwrap();
    let ws = Workspace {
        workspace_id: "ws_browser".into(),
        name: "work".into(),
        path: work.display().to_string(),
        kind: WorkspaceKind::Folder,
        network: NetworkProfile { presets: vec![NetworkPreset::Web], custom_hosts: vec![] },
        connectors: vec![],
        created_at: 0,
        default_agent: None,
        default_profile_id: None,
    };
    host.ctx.store.workspace_insert(&ws).unwrap();
    (host, ws.workspace_id)
}

/// The ref of the first line of a snapshot that contains `what`.
fn ref_of(snapshot: &str, what: &str) -> String {
    let line = snapshot.lines().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what} in:\n{snapshot}"));
    let at = line.find("[ref=").unwrap_or_else(|| panic!("no ref on {line}"));
    line[at + 5..].trim_end_matches(']').split(']').next().unwrap().to_string()
}

const FORM: &str = r#"document.body.innerHTML = `
  <h1>Test form</h1>
  <label>Name <input id="name"></label>
  <label><input type="checkbox" id="agree"> Agree</label>
  <select id="size" aria-label="Size"><option value="s">Small</option><option value="l">Large</option></select>
  <button onclick="document.title = 'Hello ' + document.getElementById('name').value + ' ' + document.getElementById('size').value; console.error('clicked')">Go</button>`;
  'ok'"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_fills_a_form_and_the_panel_sees_it() {
    if !wanted() {
        return;
    }
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let b = &host.browsers;
    let mut bus = host.ctx.bus.subscribe();

    // The first call starts the workspace's browser and gives the session a tab.
    let out = b.agent_navigate("ses_a", &ws, Some("about:blank"), None).await.expect("navigate");
    assert!(out.text.contains("URL: about:blank"), "{}", out.text);
    let list = b.list();
    assert!(list.support.available);
    let info = list.browsers.iter().find(|x| x.workspace_id == ws).expect("the workspace's browser");
    assert!(info.running);
    assert_eq!(info.tabs.len(), 1);
    assert_eq!(info.tabs[0].session_id.as_deref(), Some("ses_a"));
    let tab_id = info.tabs[0].tab_id.clone();
    let mut saw_change = false;
    while let Ok(m) = bus.try_recv() {
        saw_change |= matches!(m, agenthost_host::bus::BusMsg::BrowserChanged(_));
    }
    assert!(saw_change, "browser.changed went out");

    // The page as an outline, with refs to act on.
    assert_eq!(b.agent_evaluate("ses_a", &ws, FORM).await.unwrap().text, "ok");
    let snap = b.agent_snapshot("ses_a", &ws).await.unwrap().text;
    assert!(snap.contains("heading \"Test form\""), "{snap}");
    assert!(snap.contains("checkbox \"Agree\" [checked=false]"), "{snap}");
    let name = ref_of(&snap, "textbox \"Name\"");
    let size = ref_of(&snap, "combobox \"Size\"");
    let go = ref_of(&snap, "button \"Go\"");
    let agree = ref_of(&snap, "checkbox \"Agree\"");

    let after = b.agent_type("ses_a", &ws, &name, "Ada", false, false).await.unwrap().text;
    assert!(after.contains("textbox \"Name\": Ada"), "{after}");
    // Typing again replaces; append adds.
    b.agent_type("ses_a", &ws, &name, "Grace", false, false).await.unwrap();
    b.agent_type("ses_a", &ws, &name, " Hopper", false, true).await.unwrap();
    b.agent_select("ses_a", &ws, &size, &["Large".to_string()]).await.unwrap();
    let after = b.agent_click("ses_a", &ws, &agree, false, BrowserButton::Left).await.unwrap().text;
    // Clicked, it's checked (and focused, which its line says too).
    let line = after.lines().find(|l| l.contains("checkbox \"Agree\"")).unwrap_or_default();
    assert!(line.contains("[checked]") && !line.contains("checked=false"), "{after}");
    let after = b.agent_click("ses_a", &ws, &go, false, BrowserButton::Left).await.unwrap().text;
    if !after.contains("Title: Hello Grace Hopper l") {
        let title = b.agent_evaluate("ses_a", &ws, "document.title").await.map(|o| o.text);
        let at = b.agent_evaluate("ses_a", &ws, "(() => { const r = [...document.querySelectorAll('button')][0].getBoundingClientRect(); const e = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2); return JSON.stringify({rect: r, at: e && e.outerHTML.slice(0, 80), active: document.activeElement && document.activeElement.tagName, scroll: [scrollX, scrollY], inner: [innerWidth, innerHeight]}); })()").await.map(|o| o.text);
        let console = b.agent_console("ses_a", &ws).await.map(|o| o.text);
        panic!("the click on Go didn't show: document.title = {title:?}; {at:?}; console {console:?}\n{after}");
    }
    assert!(after.contains("new console error"), "{after}");
    let console = b.agent_console("ses_a", &ws).await.unwrap().text;
    assert!(console.contains("[console.error] clicked"), "{console}");
    assert_eq!(b.agent_console("ses_a", &ws).await.unwrap().text, "Nothing new in the console.");

    // A stale ref says to take a new snapshot.
    let err = b.agent_click("ses_a", &ws, "e999999", false, BrowserButton::Left).await.unwrap_err();
    assert!(format!("{err:#}").contains("new snapshot"), "{err:#}");

    // Keys: Enter in a field, a shortcut, and a bad key.
    b.agent_press("ses_a", &ws, "Control+a").await.unwrap();
    assert!(b.agent_press("ses_a", &ws, "Hyper+q").await.is_err());

    // A screenshot is a JPEG.
    let shot = b.agent_screenshot("ses_a", &ws, false).await.unwrap();
    let jpeg = base64::engine::general_purpose::STANDARD.decode(shot.image.unwrap()).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);

    // The panel: frames of the tab, held back until acknowledged.
    let mut a = b.attach(&tab_id).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BrowserFrame>();
    a.start(move |f| {
        let tx = tx.clone();
        async move { tx.send(f).is_ok() }
    });
    let f = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("a first frame").unwrap();
    let pic = base64::engine::general_purpose::STANDARD.decode(&f.data).unwrap();
    assert_eq!(&pic[..2], &[0xFF, 0xD8]);
    assert_eq!((f.page_width, f.page_height), BROWSER_DEFAULT_SIZE);
    a.ack(f.frame_id);

    // You type in the panel: a click in the field, then text and keys.
    let rect = b.agent_evaluate("ses_a", &ws, "JSON.stringify(document.getElementById('name').getBoundingClientRect())").await.unwrap().text;
    let r: serde_json::Value = serde_json::from_str(&rect).unwrap();
    let (x, y) = ((r["x"].as_f64().unwrap() + 5.0) as f32, (r["y"].as_f64().unwrap() + 5.0) as f32);
    for phase in [BrowserPointer::Down, BrowserPointer::Up] {
        b.input(BrowserInputParams { tab_id: tab_id.clone(), input: BrowserInput::Mouse { phase, x, y, button: Some(BrowserButton::Left), clicks: Some(3), mods: BrowserMods::default() } }).await.unwrap();
    }
    b.input(BrowserInputParams { tab_id: tab_id.clone(), input: BrowserInput::Text { text: "Lin".into() } }).await.unwrap();
    b.input(BrowserInputParams { tab_id: tab_id.clone(), input: BrowserInput::Key { key: "Backspace".into(), text: None, mods: BrowserMods::default() } }).await.unwrap();
    b.input(BrowserInputParams { tab_id: tab_id.clone(), input: BrowserInput::Key { key: "a".into(), text: Some("a".into()), mods: BrowserMods::default() } }).await.unwrap();
    let v = b.agent_evaluate("ses_a", &ws, "document.getElementById('name').value").await.unwrap().text;
    assert_eq!(v, "Lia", "a triple click selected the field's text, which the typing replaced");
    // The page changed, so another frame comes.
    let f2 = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("a frame after typing").unwrap();
    assert!(f2.frame_id > f.frame_id);
    drop(a);

    // The panel's own tab, resized to the panel, and back/forward.
    let mine = b.open(BrowserOpenParams { workspace_id: ws.clone(), url: Some("about:blank".into()), width: Some(500), height: Some(700) }).await.unwrap();
    assert_eq!((mine.width, mine.height, mine.session_id.clone()), (500, 700, None));
    let mine2 = b.resize(BrowserResizeParams { tab_id: mine.tab_id.clone(), width: 640, height: 480 }).await.unwrap();
    assert_eq!((mine2.width, mine2.height), (640, 480));
    assert!(b.navigate(BrowserNavigateParams { tab_id: mine.tab_id.clone(), url: Some("file:///etc/passwd".into()), action: None }).await.is_err());
    b.close_tab(&mine.tab_id).await.unwrap();
    assert!(!b.list().browsers[0].tabs.iter().any(|t| t.tab_id == mine.tab_id));

    host.browsers.close_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_browser_cannot_reach_this_computer() {
    if !wanted() {
        return;
    }
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    // A server on loopback that counts who reaches it.
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for s in server.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            let mut s = s;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\nsecret");
        }
    });
    for url in [format!("http://localhost:{port}/"), format!("http://127.0.0.1:{port}/")] {
        let err = host.browsers.agent_navigate("ses_b", &ws, Some(&url), None).await.err();
        let snap = host.browsers.agent_snapshot("ses_b", &ws).await.unwrap().text;
        assert!(err.is_some() || !snap.contains("secret"), "{url} opened: {snap}");
        if let Some(e) = err {
            assert!(format!("{e:#}").contains("localhost") || format!("{e:#}").contains("did not open"), "{e:#}");
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the local server");
    // Not listed for the panel to ask about: no network rule could let this computer in.
    let info = host.browsers.list().browsers.into_iter().find(|x| x.workspace_id == ws).unwrap();
    assert!(info.blocked.is_empty(), "{info:?}");
    host.browsers.close_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_ins_outlast_the_browser_and_clearing_forgets_them() {
    if !wanted() {
        return;
    }
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let b = &host.browsers;
    let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 86_400;
    b.raw_call(&ws, "Storage.setCookies", json!({"cookies": [{"name": "sid", "value": "alice", "domain": "example.com", "path": "/", "expires": expires, "secure": true}]})).await.unwrap();
    // Quit as the host does at shutdown, then start again on the same profile.
    b.stop(&ws).await;
    assert!(!b.list().browsers.iter().any(|x| x.workspace_id == ws && x.running));
    let got = b.raw_call(&ws, "Storage.getCookies", json!({})).await.unwrap();
    assert!(got["cookies"].as_array().unwrap().iter().any(|c| c["name"] == "sid" && c["value"] == "alice"), "{got}");
    // Another workspace's browser has its own sign-ins.
    let other_dir = dir.path().join("other");
    std::fs::create_dir_all(&other_dir).unwrap();
    let other = Workspace { workspace_id: "ws_other".into(), name: "other".into(), path: other_dir.display().to_string(), ..host.ctx.store.workspace_get(&ws).unwrap().unwrap() };
    host.ctx.store.workspace_insert(&other).unwrap();
    let got = b.raw_call("ws_other", "Storage.getCookies", json!({})).await.unwrap();
    assert!(!got["cookies"].as_array().unwrap().iter().any(|c| c["name"] == "sid"), "{got}");
    // Clearing deletes the profile.
    b.clear(&ws).await.unwrap();
    assert!(!b.profile_dir(&ws).exists());
    let got = b.raw_call(&ws, "Storage.getCookies", json!({})).await.unwrap();
    assert!(!got["cookies"].as_array().unwrap().iter().any(|c| c["name"] == "sid"), "{got}");
    host.browsers.close_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_page_opens_through_the_proxy() {
    if !wanted() || std::env::var_os("AGENTHOST_TEST_BROWSER_NET").is_none() {
        return;
    }
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let out = host.browsers.agent_navigate("ses_c", &ws, Some("example.com"), None).await.expect("example.com");
    assert!(out.text.contains("Example Domain"), "{}", out.text);
    let ua = host.browsers.agent_evaluate("ses_c", &ws, "navigator.userAgent + ' ' + navigator.webdriver").await.unwrap().text;
    assert!(!ua.contains("Headless"), "{ua}");
    assert!(ua.ends_with("false"), "navigator.webdriver: {ua}");
    // A workspace without the open web can't.
    let mut closed = host.ctx.store.workspace_get(&ws).unwrap().unwrap();
    closed.network = NetworkProfile::default();
    closed.workspace_id = "ws_closed".into();
    let closed_dir = dir.path().join("closed");
    std::fs::create_dir_all(&closed_dir).unwrap();
    closed.path = closed_dir.display().to_string();
    host.ctx.store.workspace_insert(&closed).unwrap();
    let err = host.browsers.agent_navigate("ses_d", "ws_closed", Some("example.com"), None).await.unwrap_err();
    assert!(format!("{err:#}").contains("network settings"), "{err:#}");
    host.browsers.close_all().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_of_the_agents_tab_is_an_mp4() {
    if !wanted() {
        return;
    }
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let b = &host.browsers;
    b.agent_navigate("ses_r", &ws, Some("about:blank"), None).await.unwrap();
    assert!(b.agent_record_stop("ses_r").await.is_err(), "nothing to stop yet");
    let out = b.agent_record_start("ses_r", &ws).await.expect("record");
    assert!(out.text.contains("browser_record_stop"), "{}", out.text);
    assert!(b.agent_record_start("ses_r", &ws).await.is_err(), "one recording at a time");

    // A page that changes: a counter, ten times, with nobody watching.
    let page = r#"document.body.innerHTML = '<h1 id="n" style="font-size:120px">0</h1>';
      let i = 0; const t = setInterval(() => { document.getElementById('n').textContent = ++i; if (i == 10) clearInterval(t); }, 150); 'ok'"#;
    assert_eq!(b.agent_evaluate("ses_r", &ws, page).await.unwrap().text, "ok");
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    // Someone watches for a moment, then leaves: the recording carries on.
    let tab_id = b.list().browsers.iter().find(|x| x.workspace_id == ws).unwrap().tabs[0].tab_id.clone();
    let viewer = b.attach(&tab_id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(viewer);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let started = std::time::Instant::now();
    let f = b.agent_record_stop("ses_r").await.expect("stop");
    assert!(started.elapsed() < Duration::from_secs(5), "stopping is quick");
    assert_eq!((f.width, f.height), BROWSER_DEFAULT_SIZE);
    assert!(!f.cut);
    assert!((1_000..4_000).contains(&f.duration_ms), "{} ms", f.duration_ms);
    assert!(f.poster.as_ref().is_some_and(|p| p.starts_with(&[0xff, 0xd8])), "a JPEG poster");
    assert!(f.mp4.len() > 1_000 && f.mp4.len() < 2_000_000, "{} bytes", f.mp4.len());

    if std::process::Command::new("ffprobe").arg("-version").output().is_ok() {
        let path = dir.path().join("r.mp4");
        std::fs::write(&path, &f.mp4).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,profile,width,height,nb_read_frames", "-of", "default=nw=1"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        eprintln!("ffprobe:\n{text}");
        assert!(out.status.success() && out.stderr.is_empty(), "{text}");
        assert!(text.contains("codec_name=h264") && text.contains("width=1280") && text.contains("height=800"), "{text}");
        let frames: u32 = text.lines().find_map(|l| l.strip_prefix("nb_read_frames=")).unwrap().parse().unwrap();
        // The counter changed ten times; at most 10 pictures a second.
        assert!((5..=40).contains(&frames), "{frames} frames\n{text}");
    }
    // Stopped, the tab's screencast stopped too (nobody watches).
    assert!(b.agent_record_stop("ses_r").await.is_err());
    host.browsers.close_all().await;
}
