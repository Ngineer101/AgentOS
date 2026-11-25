//! The browser: a headless Chromium per workspace, which agents drive with
//! the gateway's `agenthost__browser_*` tools and you watch and use in the
//! desktop's right panel (see `agenthost_proto::browser`).
//!
//! - Each workspace has its own profile under `<data>/browser/<workspace>`,
//!   so a sign-in there lasts and every session of that workspace shares it.
//! - The host talks to it over `--remote-debugging-pipe`: no port is open.
//! - Its traffic goes through an egress proxy of its own that allows what
//!   the workspace's network settings allow, and nothing on this computer.
//! - It only opens web addresses, never downloads, and runs with Chromium's
//!   own sandbox (it is the host's child, outside the agents' sandbox).
//! - It starts when a tab is wanted and stops after [`IDLE_STOP`] with
//!   nobody watching and no agent using it. The profile stays.

mod agent;
mod cdp;
pub mod keys;
pub mod snapshot;

pub use agent::AgentOut;

use crate::bus::BusMsg;
use crate::ctx::Ctx;
use crate::proxy::{EgressProxy, ProxyHooks};
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::*;
use anyhow::{anyhow, Context, Result};
use cdp::{Cdp, CdpEvent, Pipes};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch, Notify};

/// A browser nobody watched or used for this long is stopped.
pub const IDLE_STOP: Duration = Duration::from_secs(10 * 60);
/// Lines of the browser's stderr kept to explain a failed start.
const STDERR_KEPT: usize = 30;
/// Messages kept per tab for `browser_console`.
const LOG_KEPT: usize = 200;
/// The largest picture side sent to a viewer.
const MAX_FRAME_SIDE: u32 = 2560;

/// Linux browsers that speak the DevTools protocol, by command name.
const LINUX_BROWSERS: &[&str] = &["chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave-browser", "brave", "microsoft-edge-stable"];
/// The same on macOS, by app bundle.
const MAC_BROWSERS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// The browser this host runs: `$AGENTHOST_BROWSER`, else the first
/// Chromium-based browser installed.
pub fn find_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGENTHOST_BROWSER").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for app in MAC_BROWSERS {
            let p = PathBuf::from(app);
            if p.is_file() {
                return Some(p);
            }
            if let Some(h) = &home {
                let mine = h.join(app.trim_start_matches('/'));
                if mine.is_file() {
                    return Some(mine);
                }
            }
        }
        return None;
    }
    LINUX_BROWSERS.iter().find_map(|b| which(b))
}

/// How Chromium is started for a workspace: headless, driven over the pipe,
/// through `proxy_port`, and quiet (no sync, updates or pings of its own).
pub fn browser_args(profile: &Path, proxy_port: u16) -> Vec<String> {
    let (w, h) = BROWSER_DEFAULT_SIZE;
    let mut a: Vec<String> = [
        "--headless=new",
        "--remote-debugging-pipe",
        "--no-first-run",
        "--no-default-browser-check",
        // Pages can't tell an agent drives it (sign-in pages refuse automated browsers).
        "--disable-blink-features=AutomationControlled",
        // Everything, localhost included, goes through the workspace's proxy.
        "--proxy-bypass-list=<-loopback>",
        // WebRTC would otherwise send UDP around the proxy.
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--disable-quic",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--disable-domain-reliability",
        "--disable-default-apps",
        "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication,InterestFeedContentSuggestions",
        "--no-pings",
        "--metrics-recording-only",
        "--mute-audio",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(format!("--user-data-dir={}", profile.display()));
    a.push(format!("--proxy-server=http://127.0.0.1:{proxy_port}"));
    a.push(format!("--window-size={w},{h}"));
    // Sign-ins are kept by the profile alone, not your keyring (which a
    // service may not reach, and which would ask on macOS).
    a.push(if cfg!(target_os = "macos") { "--use-mock-keychain".into() } else { "--password-store=basic".into() });
    a.push("about:blank".into());
    a
}

fn clamp_size(w: Option<u32>, h: Option<u32>) -> (u32, u32) {
    let (dw, dh) = BROWSER_DEFAULT_SIZE;
    (w.unwrap_or(dw).clamp(BROWSER_MIN_SIZE, BROWSER_MAX_SIZE), h.unwrap_or(dh).clamp(BROWSER_MIN_SIZE, BROWSER_MAX_SIZE))
}

// ---------------------------------------------------------------------------
// Viewers: the frames one connection gets of one tab
// ---------------------------------------------------------------------------

struct Frame {
    data: Arc<str>,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct ViewerInner {
    /// The newest frame not yet sent; a newer one replaces it.
    latest: Mutex<Option<Arc<Frame>>>,
    sent: AtomicU64,
    acked: AtomicU64,
    wake: Notify,
    gone: AtomicBool,
}

impl ViewerInner {
    fn offer(&self, f: Arc<Frame>) {
        *self.latest.lock().unwrap() = Some(f);
        self.wake.notify_one();
    }
}

/// One connection watching one tab. Dropping it stops the frames.
pub struct Attachment {
    pub tab_id: String,
    pub workspace_id: String,
    inner: Arc<ViewerInner>,
    browsers: Weak<Browsers>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Attachment {
    /// Start sending frames with `send`, which says whether the client is
    /// still there.
    pub fn start<F, Fut>(&mut self, send: F)
    where
        F: Fn(BrowserFrame) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = bool> + Send,
    {
        let inner = self.inner.clone();
        let tab_id = self.tab_id.clone();
        self.task = Some(tokio::spawn(async move {
            loop {
                if inner.gone.load(Ordering::Relaxed) {
                    return;
                }
                let ahead = inner.sent.load(Ordering::Relaxed).saturating_sub(inner.acked.load(Ordering::Relaxed));
                let next = if ahead < BROWSER_FRAMES_AHEAD { inner.latest.lock().unwrap().take() } else { None };
                match next {
                    Some(f) => {
                        let frame_id = inner.sent.fetch_add(1, Ordering::Relaxed) + 1;
                        let frame = BrowserFrame { tab_id: tab_id.clone(), frame_id, data: f.data.to_string(), page_width: f.width, page_height: f.height };
                        if !send(frame).await {
                            return;
                        }
                    }
                    None => inner.wake.notified().await,
                }
            }
        }));
    }

    pub fn ack(&self, frame_id: u64) {
        self.inner.acked.fetch_max(frame_id.min(self.inner.sent.load(Ordering::Relaxed)), Ordering::Relaxed);
        self.inner.wake.notify_one();
        if let Some(b) = self.browsers.upgrade() {
            b.touch_tab(&self.tab_id);
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.inner.gone.store(true, Ordering::Relaxed);
        self.inner.wake.notify_one();
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// One workspace's browser
// ---------------------------------------------------------------------------

struct Tab {
    id: String,
    /// The flat CDP session attached to it.
    cdp: String,
    url: String,
    title: String,
    loading: bool,
    session_id: Option<String>,
    width: u32,
    height: u32,
    back: bool,
    forward: bool,
    used: Instant,
    viewers: Vec<Arc<ViewerInner>>,
    last_frame: Option<Arc<Frame>>,
    casting: bool,
    /// The button held down, so moves while it is drag.
    pressed: Option<BrowserButton>,
    log: VecDeque<(u64, String)>,
    log_seq: u64,
    /// What the agent has read of `log`.
    log_read: u64,
    /// Counts finished loads, so an agent can wait for the next.
    loads: watch::Sender<u64>,
}

impl Tab {
    fn describe(&self) -> BrowserTab {
        BrowserTab {
            tab_id: self.id.clone(),
            url: self.url.clone(),
            title: self.title.clone(),
            loading: self.loading,
            session_id: self.session_id.clone(),
            width: self.width,
            height: self.height,
            can_go_back: self.back,
            can_go_forward: self.forward,
        }
    }

    fn note(&mut self, line: String) {
        self.log_seq += 1;
        self.log.push_back((self.log_seq, line));
        while self.log.len() > LOG_KEPT {
            self.log.pop_front();
        }
    }
}

#[derive(Default)]
struct State {
    tabs: Vec<Tab>,
    blocked: VecDeque<String>,
}

pub struct Instance {
    workspace_id: String,
    cdp: Cdp,
    child: Mutex<Option<tokio::process::Child>>,
    _proxy: EgressProxy,
    st: Mutex<State>,
    user_agent: String,
    used: Mutex<Instant>,
    alive: AtomicBool,
    /// Set when we asked it to quit, so its exit is no surprise.
    stopping: AtomicBool,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Instance {
    fn touch(&self) {
        *self.used.lock().unwrap() = Instant::now();
    }

    fn info(&self) -> BrowserInfo {
        let st = self.st.lock().unwrap();
        BrowserInfo {
            workspace_id: self.workspace_id.clone(),
            running: self.alive.load(Ordering::Relaxed),
            tabs: st.tabs.iter().map(Tab::describe).collect(),
            blocked: st.blocked.iter().cloned().collect(),
            error: None,
        }
    }

    fn tab_session(&self, tab_id: &str) -> Option<String> {
        self.st.lock().unwrap().tabs.iter().find(|t| t.id == tab_id).map(|t| t.cdp.clone())
    }

    fn describe_tab(&self, tab_id: &str) -> Option<BrowserTab> {
        self.st.lock().unwrap().tabs.iter().find(|t| t.id == tab_id).map(Tab::describe)
    }

    async fn call(&self, tab_id: &str, method: &str, params: Value) -> Result<Value> {
        let s = self.tab_session(tab_id).ok_or_else(|| anyhow!("the tab is closed"))?;
        self.cdp.call(method, params, Some(&s)).await
    }

    fn stderr_tail(&self) -> String {
        let e = self.stderr.lock().unwrap();
        e.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default()
    }
}

/// The browser's network: what its workspace allows, nothing else.
struct BrowserNet {
    ctx: Arc<Ctx>,
    workspace_id: String,
    browsers: Weak<Browsers>,
}

impl ProxyHooks for BrowserNet {
    fn allowed_hosts(&self) -> Vec<String> {
        match self.ctx.store.workspace_get(&self.workspace_id) {
            Ok(Some(ws)) => crate::sandbox::profile_hosts(&ws.network),
            _ => vec![],
        }
    }
    fn on_denied(&self, host: &str, port: u16, reason: &str) {
        if let Some(b) = self.browsers.upgrade() {
            b.blocked(&self.workspace_id, host, port, reason);
        }
    }
    fn gateway_socket(&self) -> Option<PathBuf> {
        None
    }
}

// ---------------------------------------------------------------------------
// All of them
// ---------------------------------------------------------------------------

pub struct Browsers {
    ctx: Arc<Ctx>,
    me: Weak<Browsers>,
    running: Mutex<HashMap<String, Arc<Instance>>>,
    /// One start at a time, so two callers don't launch on one profile.
    starting: tokio::sync::Mutex<()>,
    errors: Mutex<HashMap<String, String>>,
    /// The tab each agent session works in.
    session_tabs: Mutex<HashMap<String, String>>,
}

fn rpc(e: anyhow::Error) -> RpcError {
    RpcError::new(codes::UNAVAILABLE, format!("{e:#}"))
}

impl Browsers {
    pub fn new(ctx: Arc<Ctx>) -> Arc<Browsers> {
        Arc::new_cyclic(|me| Browsers {
            ctx,
            me: me.clone(),
            running: Mutex::default(),
            starting: tokio::sync::Mutex::new(()),
            errors: Mutex::default(),
            session_tabs: Mutex::default(),
        })
    }

    pub fn support(&self) -> BrowserSupport {
        match find_browser() {
            Some(p) => BrowserSupport { available: true, detail: p.display().to_string() },
            None => BrowserSupport {
                available: false,
                detail: if cfg!(target_os = "macos") {
                    "No Chromium-based browser found. Install Google Chrome or Chromium.".into()
                } else {
                    "No Chromium-based browser found. Install chromium (or Google Chrome).".into()
                },
            },
        }
    }

    pub fn profile_dir(&self, workspace_id: &str) -> PathBuf {
        self.ctx.cfg.data_dir.join("browser").join(workspace_id)
    }

    fn instance(&self, workspace_id: &str) -> Option<Arc<Instance>> {
        self.running.lock().unwrap().get(workspace_id).filter(|i| i.alive.load(Ordering::Relaxed)).cloned()
    }

    fn by_tab(&self, tab_id: &str) -> Result<Arc<Instance>, RpcError> {
        let running = self.running.lock().unwrap();
        running
            .values()
            .find(|i| i.st.lock().unwrap().tabs.iter().any(|t| t.id == tab_id))
            .cloned()
            .ok_or_else(|| RpcError::not_found("no such tab"))
    }

    fn info_for(&self, workspace_id: &str) -> BrowserInfo {
        let mut info = match self.instance(workspace_id) {
            Some(i) => i.info(),
            None => BrowserInfo { workspace_id: workspace_id.into(), running: false, tabs: vec![], blocked: vec![], error: None },
        };
        info.error = self.errors.lock().unwrap().get(workspace_id).cloned();
        info
    }

    fn publish(&self, workspace_id: &str) {
        self.ctx.bus.publish(BusMsg::BrowserChanged(BrowserChanged { browser: self.info_for(workspace_id) }));
    }

    pub fn list(&self) -> BrowserList {
        let mut ids: Vec<String> = self.running.lock().unwrap().keys().cloned().collect();
        for id in self.errors.lock().unwrap().keys() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids.sort();
        BrowserList { support: self.support(), browsers: ids.iter().map(|id| self.info_for(id)).collect() }
    }

    fn touch_tab(&self, tab_id: &str) {
        if let Ok(i) = self.by_tab(tab_id) {
            i.touch();
        }
    }

    fn blocked(&self, workspace_id: &str, host: &str, port: u16, reason: &str) {
        let Some(inst) = self.instance(workspace_id) else { return };
        let shown = if port == 443 || port == 80 { host.to_string() } else { format!("{host}:{port}") };
        let changed = {
            let mut st = inst.st.lock().unwrap();
            for t in st.tabs.iter_mut() {
                t.note(format!("[network] {shown} was refused: {reason}"));
            }
            if st.blocked.front() == Some(&shown) {
                false
            } else {
                st.blocked.retain(|h| *h != shown);
                st.blocked.push_front(shown);
                st.blocked.truncate(BROWSER_BLOCKED_KEPT);
                true
            }
        };
        if changed {
            self.publish(workspace_id);
        }
    }

    /// The workspace's browser, started if need be.
    async fn ensure(&self, workspace_id: &str) -> Result<Arc<Instance>, RpcError> {
        if let Some(i) = self.instance(workspace_id) {
            return Ok(i);
        }
        let _g = self.starting.lock().await;
        if let Some(i) = self.instance(workspace_id) {
            return Ok(i);
        }
        if self.ctx.store.workspace_get(workspace_id).map_err(|e| RpcError::internal(e.to_string()))?.is_none() {
            return Err(RpcError::not_found("no such workspace"));
        }
        match self.launch(workspace_id).await {
            Ok(i) => {
                self.errors.lock().unwrap().remove(workspace_id);
                self.running.lock().unwrap().insert(workspace_id.to_string(), i.clone());
                self.ctx.audit.log("browser.start", json!({"workspace_id": workspace_id}));
                self.publish(workspace_id);
                Ok(i)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!(workspace_id, "browser did not start: {msg}");
                self.errors.lock().unwrap().insert(workspace_id.to_string(), format!("The browser did not start: {msg}"));
                self.publish(workspace_id);
                Err(RpcError::new(codes::UNAVAILABLE, format!("the browser did not start: {msg}")))
            }
        }
    }

    async fn launch(&self, workspace_id: &str) -> Result<Arc<Instance>> {
        let bin = find_browser().ok_or_else(|| anyhow!("{}", self.support().detail))?;
        let profile = self.profile_dir(workspace_id);
        crate::config::create_private_dir(&profile).context("browser profile folder")?;
        let proxy = EgressProxy::start(Arc::new(BrowserNet { ctx: self.ctx.clone(), workspace_id: workspace_id.into(), browsers: self.me.clone() })).await?;
        let pipes = Pipes::new()?;
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(browser_args(&profile, proxy.http_port))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            // No display: it draws nothing on screen.
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY");
        let (ci, co) = (pipes.child_in.as_raw_fd(), pipes.child_out.as_raw_fd());
        // SAFETY: only async-signal-safe calls (dup2, prctl) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                Pipes::install_in_child(ci, co)?;
                // It goes when the host does, even if the host is killed.
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = cmd.spawn().with_context(|| format!("could not run {}", bin.display()))?;
        let stderr: Arc<Mutex<VecDeque<String>>> = Arc::default();
        if let Some(err) = child.stderr.take() {
            let keep = stderr.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut k = keep.lock().unwrap();
                    k.push_back(l);
                    while k.len() > STDERR_KEPT {
                        k.pop_front();
                    }
                }
            });
        }
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed_rx) = oneshot::channel();
        let cdp = pipes.connect(ev_tx, closed_tx);
        let version = match cdp.call_within("Browser.getVersion", json!({}), None, Duration::from_secs(20)).await {
            Ok(v) => v,
            Err(e) => {
                let _ = child.start_kill();
                tokio::time::sleep(Duration::from_millis(200)).await;
                let tail = stderr.lock().unwrap().iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
                return Err(anyhow!("{e:#}{}", if tail.is_empty() { String::new() } else { format!(" ({tail})") }));
            }
        };
        // Headless Chromium says so in its user agent; sites then refuse to sign you in.
        let user_agent = version["userAgent"].as_str().unwrap_or_default().replace("HeadlessChrome", "Chrome");
        let inst = Arc::new(Instance {
            workspace_id: workspace_id.to_string(),
            cdp,
            child: Mutex::new(Some(child)),
            _proxy: proxy,
            st: Mutex::default(),
            user_agent,
            used: Mutex::new(Instant::now()),
            alive: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            stderr,
        });
        inst.cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await?;
        // Nothing lands outside the sandbox's view on its own.
        inst.cdp.call("Browser.setDownloadBehavior", json!({"behavior": "deny"}), None).await?;
        tokio::spawn(events(self.me.clone(), Arc::downgrade(&inst), ev_rx));
        let (me, weak) = (self.me.clone(), Arc::downgrade(&inst));
        tokio::spawn(async move {
            let _ = closed_rx.await;
            if let (Some(b), Some(i)) = (me.upgrade(), weak.upgrade()) {
                b.exited(&i);
            }
        });
        let (me, weak) = (self.me.clone(), Arc::downgrade(&inst));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let (Some(b), Some(i)) = (me.upgrade(), weak.upgrade()) else { return };
                if !i.alive.load(Ordering::Relaxed) {
                    return;
                }
                let watched = i.st.lock().unwrap().tabs.iter().any(|t| t.viewers.iter().any(|v| !v.gone.load(Ordering::Relaxed)));
                if !watched && i.used.lock().unwrap().elapsed() > IDLE_STOP {
                    tracing::info!(workspace_id = %i.workspace_id, "browser idle, stopping");
                    b.quit(&i).await;
                    return;
                }
            }
        });
        Ok(inst)
    }

    /// The browser's pipe closed: it quit, or crashed.
    fn exited(&self, inst: &Arc<Instance>) {
        inst.alive.store(false, Ordering::Relaxed);
        let ws = inst.workspace_id.clone();
        {
            let mut running = self.running.lock().unwrap();
            if running.get(&ws).is_some_and(|i| Arc::ptr_eq(i, inst)) {
                running.remove(&ws);
            }
        }
        if !inst.stopping.load(Ordering::Relaxed) {
            let tail = inst.stderr_tail();
            tracing::warn!(workspace_id = %ws, "browser quit unexpectedly: {tail}");
            self.errors.lock().unwrap().insert(ws.clone(), format!("The browser quit unexpectedly.{}", if tail.is_empty() { String::new() } else { format!(" {tail}") }));
        }
        self.publish(&ws);
    }

    /// Ask the browser to quit (it saves its cookies), then make sure.
    async fn quit(&self, inst: &Arc<Instance>) {
        inst.stopping.store(true, Ordering::Relaxed);
        let _ = inst.cdp.call_within("Browser.close", json!({}), None, Duration::from_secs(5)).await;
        let child = inst.child.lock().unwrap().take();
        if let Some(mut c) = child {
            if tokio::time::timeout(Duration::from_secs(5), c.wait()).await.is_err() {
                let _ = c.start_kill();
                let _ = c.wait().await;
            }
        }
        self.exited(inst);
    }

    // ---- What clients ask for ----

    pub async fn open(&self, p: BrowserOpenParams) -> Result<BrowserTab, RpcError> {
        let url = p.url.as_deref().map(browser_url).transpose().map_err(RpcError::invalid_params)?;
        let inst = self.ensure(&p.workspace_id).await?;
        let (w, h) = clamp_size(p.width, p.height);
        let tab = self.new_tab(&inst, None, w, h).await.map_err(rpc)?;
        if let Some(u) = url {
            inst.call(&tab, "Page.navigate", json!({"url": u})).await.map_err(rpc)?;
        }
        self.ctx.audit.log("browser.open_tab", json!({"workspace_id": p.workspace_id, "tab_id": tab}));
        inst.describe_tab(&tab).ok_or_else(|| RpcError::not_found("the tab closed"))
    }

    /// Open a tab and set it up: events on, its size, an ordinary user agent.
    async fn new_tab(&self, inst: &Arc<Instance>, session_id: Option<&str>, w: u32, h: u32) -> Result<String> {
        self.make_room(inst).await;
        // The page's size is set below (a size here is only for new windows).
        let r = inst.cdp.call("Target.createTarget", json!({"url": "about:blank"}), None).await?;
        let id = r["targetId"].as_str().ok_or_else(|| anyhow!("no target id"))?.to_string();
        self.adopt(inst, &id, session_id.map(str::to_string), w, h).await?;
        Ok(id)
    }

    async fn adopt(&self, inst: &Arc<Instance>, target_id: &str, session_id: Option<String>, w: u32, h: u32) -> Result<()> {
        let r = inst.cdp.call("Target.attachToTarget", json!({"targetId": target_id, "flatten": true}), None).await?;
        let s = r["sessionId"].as_str().ok_or_else(|| anyhow!("no session id"))?.to_string();
        {
            let mut st = inst.st.lock().unwrap();
            if st.tabs.iter().any(|t| t.id == target_id) {
                return Ok(());
            }
            st.tabs.push(Tab {
                id: target_id.to_string(),
                cdp: s.clone(),
                url: "about:blank".into(),
                title: String::new(),
                loading: false,
                session_id,
                width: w,
                height: h,
                back: false,
                forward: false,
                used: Instant::now(),
                viewers: vec![],
                last_frame: None,
                casting: false,
                pressed: None,
                log: VecDeque::new(),
                log_seq: 0,
                log_read: 0,
                loads: watch::channel(0).0,
            });
        }
        let c = |m: &'static str, p: Value| {
            let s = s.clone();
            async move { inst.cdp.call(m, p, Some(&s)).await }
        };
        c("Page.enable", json!({})).await?;
        c("Runtime.enable", json!({})).await?;
        c("Log.enable", json!({})).await?;
        c("Emulation.setDeviceMetricsOverride", json!({"width": w, "height": h, "deviceScaleFactor": 1, "mobile": false})).await?;
        if !inst.user_agent.is_empty() {
            c("Emulation.setUserAgentOverride", json!({"userAgent": inst.user_agent})).await?;
        }
        inst.touch();
        self.publish(&inst.workspace_id);
        Ok(())
    }

    /// At most [`MAX_BROWSER_TABS`]: the agent tab used longest ago, with
    /// nobody watching, makes way.
    async fn make_room(&self, inst: &Arc<Instance>) {
        let victim = {
            let st = inst.st.lock().unwrap();
            if st.tabs.len() < MAX_BROWSER_TABS {
                return;
            }
            st.tabs.iter().filter(|t| t.session_id.is_some() && t.viewers.iter().all(|v| v.gone.load(Ordering::Relaxed))).min_by_key(|t| t.used).map(|t| t.id.clone())
        };
        if let Some(id) = victim {
            let _ = inst.cdp.call("Target.closeTarget", json!({"targetId": id}), None).await;
            self.forget_tab(inst, &id);
        }
    }

    fn forget_tab(&self, inst: &Arc<Instance>, tab_id: &str) {
        let removed = {
            let mut st = inst.st.lock().unwrap();
            let before = st.tabs.len();
            st.tabs.retain(|t| t.id != tab_id);
            st.tabs.len() != before
        };
        if removed {
            self.session_tabs.lock().unwrap().retain(|_, t| t != tab_id);
            self.publish(&inst.workspace_id);
        }
    }

    pub async fn navigate(&self, p: BrowserNavigateParams) -> Result<BrowserTab, RpcError> {
        let inst = self.by_tab(&p.tab_id)?;
        inst.touch();
        match (p.url.as_deref(), p.action) {
            (Some(u), None) => {
                let url = browser_url(u).map_err(RpcError::invalid_params)?;
                inst.call(&p.tab_id, "Page.navigate", json!({"url": url})).await.map_err(rpc)?;
            }
            (None, Some(a)) => go(&inst, &p.tab_id, a).await.map_err(rpc)?,
            _ => return Err(RpcError::invalid_params("give either url or action")),
        }
        inst.describe_tab(&p.tab_id).ok_or_else(|| RpcError::not_found("the tab closed"))
    }

    pub async fn resize(&self, p: BrowserResizeParams) -> Result<BrowserTab, RpcError> {
        let inst = self.by_tab(&p.tab_id)?;
        let (w, h) = clamp_size(Some(p.width), Some(p.height));
        inst.call(&p.tab_id, "Emulation.setDeviceMetricsOverride", json!({"width": w, "height": h, "deviceScaleFactor": 1, "mobile": false})).await.map_err(rpc)?;
        let restart = {
            let mut st = inst.st.lock().unwrap();
            let Some(t) = st.tabs.iter_mut().find(|t| t.id == p.tab_id) else { return Err(RpcError::not_found("no such tab")) };
            t.width = w;
            t.height = h;
            t.casting
        };
        if restart {
            // The picture's size cap follows the page's.
            let _ = inst.call(&p.tab_id, "Page.startScreencast", screencast_params(w, h)).await;
        }
        self.publish(&inst.workspace_id);
        inst.describe_tab(&p.tab_id).ok_or_else(|| RpcError::not_found("the tab closed"))
    }

    pub async fn attach(&self, tab_id: &str) -> Result<Attachment, RpcError> {
        let inst = self.by_tab(tab_id)?;
        inst.touch();
        let inner = Arc::new(ViewerInner::default());
        let (start, size) = {
            let mut st = inst.st.lock().unwrap();
            let Some(t) = st.tabs.iter_mut().find(|t| t.id == tab_id) else { return Err(RpcError::not_found("no such tab")) };
            t.viewers.retain(|v| !v.gone.load(Ordering::Relaxed));
            t.viewers.push(inner.clone());
            if let Some(f) = &t.last_frame {
                inner.offer(f.clone());
            }
            let start = !t.casting;
            t.casting = true;
            (start, (t.width, t.height))
        };
        if start {
            inst.call(tab_id, "Page.startScreencast", screencast_params(size.0, size.1)).await.map_err(rpc)?;
        }
        Ok(Attachment { tab_id: tab_id.to_string(), workspace_id: inst.workspace_id.clone(), inner, browsers: self.me.clone(), task: None })
    }

    pub async fn input(&self, p: BrowserInputParams) -> Result<(), RpcError> {
        let inst = self.by_tab(&p.tab_id)?;
        inst.touch();
        let s = inst.tab_session(&p.tab_id).ok_or_else(|| RpcError::not_found("no such tab"))?;
        let events: Vec<(&str, Value)> = match p.input {
            BrowserInput::Mouse { phase, x, y, button, clicks, mods } => {
                let held = {
                    let mut st = inst.st.lock().unwrap();
                    let Some(t) = st.tabs.iter_mut().find(|t| t.id == p.tab_id) else { return Err(RpcError::not_found("no such tab")) };
                    match phase {
                        BrowserPointer::Down => t.pressed = Some(button.unwrap_or(BrowserButton::Left)),
                        BrowserPointer::Up => {}
                        BrowserPointer::Move => {}
                    }
                    let held = t.pressed;
                    if phase == BrowserPointer::Up {
                        t.pressed = None;
                    }
                    held
                };
                let (kind, b) = match phase {
                    BrowserPointer::Down => ("mousePressed", button.or(held)),
                    BrowserPointer::Up => ("mouseReleased", button.or(held)),
                    BrowserPointer::Move => ("mouseMoved", held),
                };
                let buttons = if phase == BrowserPointer::Up { 0 } else { held.map(button_bit).unwrap_or(0) };
                vec![("Input.dispatchMouseEvent", json!({
                    "type": kind, "x": x, "y": y, "button": b.map(button_name).unwrap_or("none"), "buttons": buttons,
                    "clickCount": if phase == BrowserPointer::Move { 0 } else { clicks.unwrap_or(1) }, "modifiers": keys::mod_bits(&mods)
                }))]
            }
            BrowserInput::Wheel { x, y, dx, dy, mods } => vec![("Input.dispatchMouseEvent", json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "modifiers": keys::mod_bits(&mods)}))],
            BrowserInput::Key { key, text, mods } => keys::press(&key, text.as_deref(), &mods).into_iter().map(|e| ("Input.dispatchKeyEvent", e)).collect(),
            BrowserInput::Text { text } => vec![("Input.insertText", json!({"text": text}))],
        };
        for (m, params) in events {
            inst.cdp.call(m, params, Some(&s)).await.map_err(rpc)?;
        }
        Ok(())
    }

    pub async fn selection(&self, tab_id: &str) -> Result<BrowserSelection, RpcError> {
        let inst = self.by_tab(tab_id)?;
        let r = inst.call(tab_id, "Runtime.evaluate", json!({"expression": "String(getSelection())", "returnByValue": true})).await.map_err(rpc)?;
        Ok(BrowserSelection { text: r["result"]["value"].as_str().unwrap_or_default().to_string() })
    }

    pub async fn close_tab(&self, tab_id: &str) -> Result<(), RpcError> {
        let inst = self.by_tab(tab_id)?;
        let _ = inst.cdp.call("Target.closeTarget", json!({"targetId": tab_id}), None).await;
        self.forget_tab(&inst, tab_id);
        Ok(())
    }

    pub async fn stop(&self, workspace_id: &str) {
        self.errors.lock().unwrap().remove(workspace_id);
        match self.instance(workspace_id) {
            Some(i) => self.quit(&i).await,
            None => self.publish(workspace_id),
        }
    }

    /// Stop the browser and delete its profile: signed out of everything.
    pub async fn clear(&self, workspace_id: &str) -> Result<(), RpcError> {
        let _g = self.starting.lock().await;
        if let Some(i) = self.instance(workspace_id) {
            self.quit(&i).await;
        }
        let dir = self.profile_dir(workspace_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| RpcError::internal(format!("could not delete the browser's data: {e}")))?;
        }
        self.ctx.audit.log("browser.clear", json!({"workspace_id": workspace_id}));
        self.publish(workspace_id);
        Ok(())
    }

    /// Call the workspace's browser directly (tests only; nothing outside
    /// this process can reach it).
    #[doc(hidden)]
    pub async fn raw_call(&self, workspace_id: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        let inst = self.ensure(workspace_id).await?;
        inst.cdp.call(method, params, None).await.map_err(rpc)
    }

    /// The host is shutting down: let every browser save its cookies.
    pub async fn close_all(&self) {
        let all: Vec<Arc<Instance>> = self.running.lock().unwrap().values().cloned().collect();
        futures::future::join_all(all.iter().map(|i| self.quit(i))).await;
    }
}

fn screencast_params(w: u32, h: u32) -> Value {
    json!({"format": "jpeg", "quality": 70, "maxWidth": w.min(MAX_FRAME_SIDE), "maxHeight": h.min(MAX_FRAME_SIDE), "everyNthFrame": 1})
}

fn button_name(b: BrowserButton) -> &'static str {
    match b {
        BrowserButton::Left => "left",
        BrowserButton::Middle => "middle",
        BrowserButton::Right => "right",
    }
}

fn button_bit(b: BrowserButton) -> u32 {
    match b {
        BrowserButton::Left => 1,
        BrowserButton::Right => 2,
        BrowserButton::Middle => 4,
    }
}

/// Back, forward, reload or stop.
async fn go(inst: &Instance, tab_id: &str, a: BrowserAction) -> Result<()> {
    match a {
        BrowserAction::Reload => inst.call(tab_id, "Page.reload", json!({})).await.map(drop),
        BrowserAction::Stop => inst.call(tab_id, "Page.stopLoading", json!({})).await.map(drop),
        BrowserAction::Back | BrowserAction::Forward => {
            let h = inst.call(tab_id, "Page.getNavigationHistory", json!({})).await?;
            let i = h["currentIndex"].as_i64().unwrap_or(0) + if a == BrowserAction::Back { -1 } else { 1 };
            let Some(entry) = h["entries"].as_array().and_then(|e| e.get(usize::try_from(i).ok()?)) else {
                return Err(anyhow!("nothing to go {}", if a == BrowserAction::Back { "back to" } else { "forward to" }));
            };
            inst.call(tab_id, "Page.navigateToHistoryEntry", json!({"entryId": entry["id"]})).await.map(drop)
        }
    }
}

/// Whether there is somewhere to go back or forward to.
async fn refresh_history(b: Weak<Browsers>, inst: Weak<Instance>, tab_id: String) {
    let Some(i) = inst.upgrade() else { return };
    let Ok(h) = i.call(&tab_id, "Page.getNavigationHistory", json!({})).await else { return };
    let cur = h["currentIndex"].as_i64().unwrap_or(0);
    let n = h["entries"].as_array().map(|e| e.len() as i64).unwrap_or(0);
    let changed = {
        let mut st = i.st.lock().unwrap();
        let Some(t) = st.tabs.iter_mut().find(|t| t.id == tab_id) else { return };
        let (back, fwd) = (cur > 0, cur + 1 < n);
        let changed = (t.back, t.forward) != (back, fwd);
        (t.back, t.forward) = (back, fwd);
        changed
    };
    if changed {
        if let Some(b) = b.upgrade() {
            b.publish(&i.workspace_id);
        }
    }
}

fn console_text(args: &Value) -> String {
    args.as_array()
        .map(|a| {
            a.iter()
                .map(|x| match &x["value"] {
                    Value::String(s) => s.clone(),
                    Value::Null => x["description"].as_str().or(x["type"].as_str()).unwrap_or("").to_string(),
                    v => v.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Everything the browser tells us, in order.
async fn events(b: Weak<Browsers>, inst: Weak<Instance>, mut rx: mpsc::UnboundedReceiver<CdpEvent>) {
    while let Some(ev) = rx.recv().await {
        let (Some(br), Some(i)) = (b.upgrade(), inst.upgrade()) else { return };
        let p = &ev.params;
        let mut publish = false;
        match (ev.session.as_deref(), ev.method.as_str()) {
            (None, "Target.targetCreated") => {
                // A page a tab opened (a sign-in popup) belongs with it.
                let t = &p["targetInfo"];
                let opener = t["openerId"].as_str().unwrap_or("");
                let owner = if t["type"] == "page" && !opener.is_empty() {
                    i.st.lock().unwrap().tabs.iter().find(|x| x.id == opener).map(|x| x.session_id.clone())
                } else {
                    None
                };
                if let (Some(owner), Some(id)) = (owner, t["targetId"].as_str()) {
                    let (br2, i2, id) = (br.clone(), i.clone(), id.to_string());
                    tokio::spawn(async move {
                        let (w, h) = BROWSER_DEFAULT_SIZE;
                        if let Err(e) = br2.adopt(&i2, &id, owner, w, h).await {
                            tracing::debug!("browser: popup not adopted: {e:#}");
                        }
                    });
                }
            }
            (None, "Target.targetInfoChanged") => {
                let t = &p["targetInfo"];
                let id = t["targetId"].as_str().unwrap_or("");
                let mut st = i.st.lock().unwrap();
                if let Some(tab) = st.tabs.iter_mut().find(|x| x.id == id) {
                    let (url, title) = (t["url"].as_str().unwrap_or("").to_string(), t["title"].as_str().unwrap_or("").to_string());
                    if tab.url != url || tab.title != title {
                        tab.url = url;
                        tab.title = title;
                        publish = true;
                    }
                }
            }
            (None, "Target.targetDestroyed") | (None, "Target.detachedFromTarget") => {
                if let Some(id) = p["targetId"].as_str() {
                    br.forget_tab(&i, id);
                }
            }
            (Some(s), m) => {
                let mut st = i.st.lock().unwrap();
                let Some(tab) = st.tabs.iter_mut().find(|t| t.cdp == s) else { continue };
                match m {
                    "Page.screencastFrame" => {
                        i.cdp.send("Page.screencastFrameAck", json!({"sessionId": p["sessionId"]}), Some(s));
                        let Some(data) = p["data"].as_str() else { continue };
                        let md = &p["metadata"];
                        let f = Arc::new(Frame {
                            data: Arc::from(data),
                            width: md["deviceWidth"].as_f64().map(|v| v.round() as u32).unwrap_or(tab.width),
                            height: md["deviceHeight"].as_f64().map(|v| v.round() as u32).unwrap_or(tab.height),
                        });
                        tab.viewers.retain(|v| !v.gone.load(Ordering::Relaxed));
                        for v in &tab.viewers {
                            v.offer(f.clone());
                        }
                        tab.last_frame = Some(f);
                        if tab.viewers.is_empty() && tab.casting {
                            tab.casting = false;
                            i.cdp.send("Page.stopScreencast", json!({}), Some(s));
                        }
                    }
                    "Page.frameStartedLoading" if p["frameId"] == tab.id.as_str() => {
                        tab.loading = true;
                        publish = true;
                    }
                    "Page.frameStoppedLoading" if p["frameId"] == tab.id.as_str() => {
                        tab.loading = false;
                        tab.loads.send_modify(|n| *n += 1);
                        publish = true;
                        tokio::spawn(refresh_history(b.clone(), inst.clone(), tab.id.clone()));
                    }
                    "Page.navigatedWithinDocument" => {
                        tokio::spawn(refresh_history(b.clone(), inst.clone(), tab.id.clone()));
                    }
                    "Page.javascriptDialogOpening" => {
                        // Nobody sees a dialog in a headless browser: say yes, and say so.
                        let kind = p["type"].as_str().unwrap_or("dialog");
                        tab.note(format!("[dialog] a {kind} said {:?} and was accepted", p["message"].as_str().unwrap_or("")));
                        let mut answer = json!({"accept": true});
                        if kind == "prompt" {
                            answer["promptText"] = p["defaultPrompt"].clone();
                        }
                        i.cdp.send("Page.handleJavaScriptDialog", answer, Some(s));
                    }
                    "Runtime.consoleAPICalled" => {
                        let kind = p["type"].as_str().unwrap_or("log");
                        tab.note(format!("[console.{kind}] {}", snapshot_short(&console_text(&p["args"]))));
                    }
                    "Runtime.exceptionThrown" => {
                        let d = &p["exceptionDetails"];
                        let text = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("an exception");
                        tab.note(format!("[uncaught] {}", snapshot_short(text)));
                    }
                    "Log.entryAdded" => {
                        let e = &p["entry"];
                        if matches!(e["level"].as_str(), Some("error" | "warning")) {
                            let url = e["url"].as_str().map(|u| format!(" ({u})")).unwrap_or_default();
                            tab.note(format!("[{}] {}{url}", e["level"].as_str().unwrap_or(""), snapshot_short(e["text"].as_str().unwrap_or(""))));
                        }
                    }
                    "Inspector.targetCrashed" => {
                        tab.note("[crash] the page crashed; reload it".into());
                        tab.loading = false;
                        publish = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if publish {
            br.publish(&i.workspace_id);
        }
    }
}

fn snapshot_short(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 2000 {
        format!("{}…", s.chars().take(2000).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browser_goes_through_the_proxy_and_keeps_its_profile() {
        let a = browser_args(Path::new("/data/browser/ws_1"), 4242);
        assert!(a.contains(&"--remote-debugging-pipe".to_string()));
        assert!(a.contains(&"--proxy-server=http://127.0.0.1:4242".to_string()));
        // Loopback too: Chromium skips the proxy for localhost unless told.
        assert!(a.contains(&"--proxy-bypass-list=<-loopback>".to_string()));
        assert!(a.contains(&"--user-data-dir=/data/browser/ws_1".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--remote-debugging-port")));
        assert!(!a.iter().any(|x| x == "--no-sandbox"));
    }

    #[test]
    fn sizes_are_kept_sane() {
        assert_eq!(clamp_size(None, None), BROWSER_DEFAULT_SIZE);
        assert_eq!(clamp_size(Some(10), Some(100_000)), (BROWSER_MIN_SIZE, BROWSER_MAX_SIZE));
    }

    #[tokio::test]
    async fn a_slow_viewer_gets_the_newest_frame_not_a_queue() {
        let inner = Arc::new(ViewerInner::default());
        let mut a = Attachment { tab_id: "t".into(), workspace_id: "w".into(), inner: inner.clone(), browsers: Weak::new(), task: None };
        let (tx, mut rx) = mpsc::unbounded_channel::<BrowserFrame>();
        a.start(move |f| {
            let tx = tx.clone();
            async move { tx.send(f).is_ok() }
        });
        let frame = |n: &str| Arc::new(Frame { data: Arc::from(n), width: 10, height: 10 });
        // Two go out unacknowledged; then it waits, keeping only the newest.
        inner.offer(frame("1"));
        let f1 = rx.recv().await.unwrap();
        inner.offer(frame("2"));
        let f2 = rx.recv().await.unwrap();
        for n in ["3", "4", "5"] {
            inner.offer(frame(n));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err(), "no third frame before an ack");
        assert_eq!((f1.frame_id, f1.data.as_str(), f2.frame_id), (1, "1", 2));
        a.ack(1);
        let f3 = rx.recv().await.unwrap();
        assert_eq!((f3.frame_id, f3.data.as_str()), (3, "5"));
        drop(a);
        inner.offer(frame("6"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "nothing after detaching");
    }
}
