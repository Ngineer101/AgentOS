//! Screen sharing: a phone watches (and, if allowed, controls) the host's
//! screen through [screen-relay](https://github.com/vercel-labs/screen-relay).
//!
//! The host starts screen-relay on demand, on loopback only, and pipes its
//! `/stream` WebSocket into a device tunnel without looking inside: the phone
//! decodes the H.264 frames and sends the ACKs that drive screen-relay's rate
//! control, so the bitrate follows the real path through the relay.
//!
//! Nothing happens unless the desktop turned it on ([`ScreenPolicy`]). A phone
//! calls `screen.start` on its normal connection and gets a one-time ticket,
//! then opens a second tunnel whose first message is `screen.attach`. After
//! the last viewer leaves, the engine is stopped.

use crate::bus::BusMsg;
use crate::ctx::Ctx;
use crate::util::{now_ms, random_hex};
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::screen as wire;
use agenthost_proto::*;
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMsg;

const POLICY_KEY: &str = "screen.policy";
/// screen-relay session name, so `down` never touches a session of yours.
const SESSION_NAME: &str = "agenthost";
const TICKET_TTL_MS: i64 = 60_000;
/// How long a started engine waits for its first viewer, and lingers after
/// the last one (a phone switching networks reconnects within this).
const IDLE_STOP: Duration = Duration::from_secs(60);
/// The virtual desktop for [`ScreenMode::Browser`]: roomy enough for sign-in
/// pages, small enough to read on a phone.
const BROWSER_RESOLUTION: &str = "1280x800";

/// Stream settings for a phone over the relay: 30 fps and a lower bitrate
/// ceiling than screen-relay's LAN defaults (60 fps, up to 40 Mbps).
const STREAM_ENV: [(&str, &str); 4] = [
    ("SCREEN_RELAY_FPS", "30"),
    ("SCREEN_RELAY_BITRATE_MIN", "500k"),
    ("SCREEN_RELAY_BITRATE_START", "3M"),
    ("SCREEN_RELAY_BITRATE_MAX", "8M"),
];

struct Engine {
    mode: ScreenMode,
    control: bool,
    started_at: i64,
    started_by: String,
    /// `ws://127.0.0.1:PORT`
    base: String,
    /// Started by us (stop it when done) rather than an engine you run.
    owned: bool,
}

struct Ticket {
    device_id: Option<String>,
    control: bool,
    expires_at: i64,
}

#[derive(Default)]
struct State {
    engine: Option<Engine>,
    tickets: HashMap<String, Ticket>,
    /// screen-relay allows one stream at a time: a new viewer replaces the old.
    viewer: Option<(u64, oneshot::Sender<()>)>,
    next_viewer: u64,
    /// Bumped on every change; a pending idle stop checks it is still current.
    epoch: u64,
    error: Option<String>,
}

pub struct Screen {
    ctx: Arc<Ctx>,
    state: Mutex<State>,
    /// Serialises start and stop, which run screen-relay and take seconds.
    lifecycle: tokio::sync::Mutex<()>,
}

/// What `screen.attach` hands the pipe.
pub struct Attached {
    url: String,
    control: bool,
    id: u64,
    kill: oneshot::Receiver<()>,
}

/// The session environment of the desktop the host runs under. A login
/// service often lacks `WAYLAND_DISPLAY` and friends, so look for the
/// compositor's socket too.
#[derive(Debug, Clone, Default)]
struct Session {
    wayland: Option<String>,
    x11: Option<String>,
    xauthority: Option<String>,
    runtime_dir: Option<PathBuf>,
    desktop: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Session {
    fn detect() -> Session {
        let runtime_dir = env("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))))
            .filter(|d| d.is_dir());
        let wayland = env("WAYLAND_DISPLAY").or_else(|| {
            let dir = runtime_dir.as_ref()?;
            let mut socks: Vec<String> = std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
                .collect();
            socks.sort();
            socks.into_iter().next()
        });
        Session {
            wayland,
            x11: env("DISPLAY"),
            xauthority: env("XAUTHORITY"),
            runtime_dir,
            desktop: env("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        }
    }

    /// GNOME and KDE offer capture and input only through xdg-desktop-portal,
    /// which screen-relay has no backend for yet.
    fn portal_only(&self) -> bool {
        let d = self.desktop.to_ascii_lowercase();
        self.wayland.is_some() && (d.contains("gnome") || d.contains("kde"))
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// A browser for [`ScreenMode::Browser`], and the arguments it needs.
fn browser(profile: &std::path::Path) -> Option<Vec<String>> {
    let p = profile.display().to_string();
    for name in ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave"] {
        if let Some(bin) = which(name) {
            return Some(vec![
                bin.display().to_string(),
                format!("--user-data-dir={p}"),
                "--no-first-run".into(),
                "--start-maximized".into(),
            ]);
        }
    }
    which("firefox").map(|bin| {
        vec![bin.display().to_string(), "--no-remote".into(), "--profile".into(), p]
    })
}

/// screen-relay's `--desktop` takes one command line; quote each word.
fn command_line(words: &[String]) -> String {
    words
        .iter()
        .map(|w| format!("'{}'", w.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Screen {
    pub fn new(ctx: Arc<Ctx>) -> Arc<Screen> {
        Arc::new(Screen {
            ctx,
            state: Mutex::new(State::default()),
            lifecycle: tokio::sync::Mutex::new(()),
        })
    }

    fn bin(&self) -> Option<PathBuf> {
        self.ctx
            .cfg
            .screen_relay_bin
            .clone()
            .or_else(|| which("screen-relay"))
    }

    fn browser_profile(&self) -> PathBuf {
        self.ctx.cfg.data_dir.join("screen-browser")
    }

    pub fn policy(&self) -> ScreenPolicy {
        self.ctx
            .store
            .setting_get(POLICY_KEY)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub async fn set_policy(&self, p: ScreenPolicy) -> Result<ScreenStatus, RpcError> {
        self.ctx
            .store
            .setting_set(POLICY_KEY, &p)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        self.ctx.audit.log("screen.policy", json!(p));
        let stop = !p.enabled
            || (!p.allow_control
                && self.state.lock().unwrap().engine.as_ref().is_some_and(|e| e.control));
        if stop {
            self.stop("policy changed").await;
        }
        self.changed();
        Ok(self.status())
    }

    pub fn modes(&self) -> Vec<ScreenModeInfo> {
        if let Some(url) = &self.ctx.cfg.screen_engine_url {
            return vec![
                ScreenModeInfo { mode: ScreenMode::Desktop, available: true, detail: format!("screen-relay at {url}") },
                ScreenModeInfo { mode: ScreenMode::Browser, available: false, detail: "Not with an engine you started yourself".into() },
            ];
        }
        let installed = self.bin().is_some();
        let missing = "screen-relay is not installed: npm install -g screen-relay && screen-relay install";
        if cfg!(target_os = "macos") {
            return vec![
                ScreenModeInfo {
                    mode: ScreenMode::Desktop,
                    available: installed,
                    detail: if installed { "Main display".into() } else { missing.into() },
                },
                ScreenModeInfo { mode: ScreenMode::Browser, available: false, detail: "Linux only".into() },
            ];
        }
        let s = Session::detect();
        let desktop = if !installed {
            (false, missing.to_string())
        } else if s.portal_only() {
            (false, format!("{} on Wayland is not supported yet; use Browser", s.desktop))
        } else if let Some(w) = &s.wayland {
            let name = if s.desktop.is_empty() { "Wayland".to_string() } else { s.desktop.clone() };
            (true, format!("{name} ({w})"))
        } else if let Some(d) = &s.x11 {
            (true, format!("X11 {d}"))
        } else {
            (false, "No graphical session found".into())
        };
        let browser = match (installed, browser(&self.browser_profile())) {
            (false, _) => (false, missing.to_string()),
            (true, None) => (false, "No Chromium, Chrome, Brave or Firefox on PATH".into()),
            (true, Some(cmd)) => {
                let name = PathBuf::from(&cmd[0]).file_name().unwrap_or_default().to_string_lossy().to_string();
                (true, format!("{name} in a separate {BROWSER_RESOLUTION} desktop"))
            }
        };
        vec![
            ScreenModeInfo { mode: ScreenMode::Desktop, available: desktop.0, detail: desktop.1 },
            ScreenModeInfo { mode: ScreenMode::Browser, available: browser.0, detail: browser.1 },
        ]
    }

    pub fn status(&self) -> ScreenStatus {
        let st = self.state.lock().unwrap();
        ScreenStatus {
            policy: self.policy(),
            installed: self.ctx.cfg.screen_engine_url.is_some() || self.bin().is_some(),
            modes: self.modes(),
            running: st.engine.as_ref().map(|e| ScreenRunning {
                mode: e.mode,
                control: e.control,
                started_at: e.started_at,
                started_by: e.started_by.clone(),
                viewers: st.viewer.is_some() as u32,
            }),
            error: st.error.clone(),
        }
    }

    fn changed(&self) {
        self.ctx.bus.publish(BusMsg::ScreenChanged(self.status()));
    }

    /// Start (or reuse) the engine and hand out a ticket for `screen.attach`.
    pub async fn start(
        self: &Arc<Self>,
        p: ScreenStartParams,
        device_id: Option<String>,
        who: &str,
    ) -> Result<ScreenStartResult, RpcError> {
        let policy = self.policy();
        if !policy.enabled {
            return Err(RpcError::new(
                codes::CAPABILITY_DENIED,
                "Screen sharing is off. Turn it on in the desktop app's Settings.",
            ));
        }
        if p.control && !policy.allow_control {
            return Err(RpcError::new(
                codes::CAPABILITY_DENIED,
                "This host allows watching only. Allow control in the desktop app's Settings.",
            ));
        }
        let mode = self.modes().into_iter().find(|m| m.mode == p.mode);
        match mode {
            Some(m) if m.available => {}
            Some(m) => return Err(RpcError::new(codes::UNAVAILABLE, m.detail)),
            None => return Err(RpcError::invalid_params("unknown mode")),
        }
        {
            let _g = self.lifecycle.lock().await;
            let restart = self
                .state
                .lock()
                .unwrap()
                .engine
                .as_ref()
                .map(|e| e.mode != p.mode || e.control != p.control);
            if restart == Some(true) {
                self.stop_locked("restarting for another mode").await;
            }
            if restart != Some(false) {
                match self.launch(p.mode).await {
                    Ok((base, owned)) => {
                        let mut st = self.state.lock().unwrap();
                        st.engine = Some(Engine {
                            mode: p.mode,
                            control: p.control,
                            started_at: now_ms(),
                            started_by: who.to_string(),
                            base,
                            owned,
                        });
                        st.error = None;
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        self.state.lock().unwrap().error = Some(msg.clone());
                        self.changed();
                        return Err(RpcError::new(codes::UNAVAILABLE, msg));
                    }
                }
            }
        }
        let ticket = random_hex(32);
        {
            let mut st = self.state.lock().unwrap();
            let now = now_ms();
            st.tickets.retain(|_, t| t.expires_at > now);
            st.tickets.insert(
                hex::encode(tunnel::sha256(ticket.as_bytes())),
                Ticket { device_id: device_id.clone(), control: p.control, expires_at: now + TICKET_TTL_MS },
            );
        }
        self.ctx.audit.log(
            "screen.start",
            json!({"mode": p.mode, "control": p.control, "device_id": device_id, "label": who}),
        );
        self.changed();
        self.schedule_idle_stop();
        Ok(ScreenStartResult { ticket, expires_in_ms: TICKET_TTL_MS, mode: p.mode, control: p.control })
    }

    /// Run `screen-relay up` for `mode`; returns the engine's base URL.
    async fn launch(&self, mode: ScreenMode) -> Result<(String, bool)> {
        if let Some(url) = &self.ctx.cfg.screen_engine_url {
            let base = url.trim_end_matches('/').replacen("http", "ws", 1);
            return Ok((base, false));
        }
        let bin = self.bin().ok_or_else(|| anyhow!("screen-relay is not installed"))?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(["up", "-d", "--json", "--name", SESSION_NAME, "--port", &port.to_string()]);
        cmd.envs(STREAM_ENV);
        cmd.stdin(std::process::Stdio::null());
        match mode {
            ScreenMode::Desktop if cfg!(target_os = "macos") => {}
            ScreenMode::Desktop => {
                let s = Session::detect();
                if let Some(dir) = &s.runtime_dir {
                    cmd.env("XDG_RUNTIME_DIR", dir);
                }
                if let Some(w) = &s.wayland {
                    cmd.args(["--backend", "wayland", "--display", w]);
                } else if let Some(d) = &s.x11 {
                    cmd.args(["--backend", "x11", "--display", d]);
                    if let Some(a) = &s.xauthority {
                        cmd.env("XAUTHORITY", a);
                    }
                } else {
                    bail!("no graphical session found");
                }
            }
            ScreenMode::Browser => {
                let profile = self.browser_profile();
                std::fs::create_dir_all(&profile)?;
                let words = browser(&profile).ok_or_else(|| anyhow!("no browser found"))?;
                cmd.args(["--backend", "x11", "--desktop", &command_line(&words)]);
                cmd.env("SCREEN_RELAY_RESOLUTION", BROWSER_RESOLUTION);
            }
        }
        // A session left behind by a host that crashed holds the name.
        let _ = self.run_down(&bin).await;
        let out = tokio::time::timeout(Duration::from_secs(90), cmd.output())
            .await
            .map_err(|_| anyhow!("screen-relay did not start within 90 s"))?
            .with_context(|| format!("run {}", bin.display()))?;
        if !out.status.success() {
            bail!("screen-relay could not start: {}", screen_relay_error(&out.stderr, &out.stdout));
        }
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        let url = v["session"]["url"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}"));
        Ok((url.trim_end_matches('/').replacen("http", "ws", 1), true))
    }

    async fn run_down(&self, bin: &std::path::Path) -> Result<()> {
        let out = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(bin)
                .args(["down", "--json", "--name", SESSION_NAME])
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await??;
        if !out.status.success() {
            bail!("{}", screen_relay_error(&out.stderr, &out.stdout));
        }
        Ok(())
    }

    /// Stop the engine and close the viewer.
    pub async fn stop(&self, reason: &str) {
        let _g = self.lifecycle.lock().await;
        self.stop_locked(reason).await;
    }

    async fn stop_locked(&self, reason: &str) {
        let (engine, viewer) = {
            let mut st = self.state.lock().unwrap();
            st.tickets.clear();
            st.epoch += 1;
            (st.engine.take(), st.viewer.take())
        };
        if let Some((_, kill)) = viewer {
            let _ = kill.send(());
        }
        let Some(engine) = engine else { return };
        if engine.owned {
            if let Some(bin) = self.bin() {
                if let Err(e) = self.run_down(&bin).await {
                    tracing::warn!("screen-relay down: {e:#}");
                }
            }
        }
        self.ctx.audit.log("screen.stop", json!({"reason": reason}));
        self.changed();
    }

    /// Stop after [`IDLE_STOP`] unless a viewer is attached by then.
    fn schedule_idle_stop(self: &Arc<Self>) {
        let epoch = {
            let mut st = self.state.lock().unwrap();
            st.epoch += 1;
            st.epoch
        };
        let me = self.clone();
        let wait = self.ctx.cfg.screen_idle_stop.unwrap_or(IDLE_STOP);
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let idle = {
                let st = me.state.lock().unwrap();
                st.epoch == epoch && st.viewer.is_none() && st.engine.is_some()
            };
            if idle {
                me.stop("no viewer").await;
            }
        });
    }

    /// Redeem a ticket for the tunnel it arrived on.
    pub fn attach(&self, p: &ScreenAttachParams, device_id: Option<&str>) -> Result<Attached, RpcError> {
        let mut st = self.state.lock().unwrap();
        let t = st.tickets.remove(&hex::encode(tunnel::sha256(p.ticket.as_bytes())));
        let Some(t) = t.filter(|t| t.expires_at > now_ms() && t.device_id.as_deref() == device_id) else {
            return Err(RpcError::new(codes::CAPABILITY_DENIED, "unknown or expired screen ticket"));
        };
        let policy = self.policy();
        if !policy.enabled || (t.control && !policy.allow_control) {
            return Err(RpcError::new(codes::CAPABILITY_DENIED, "screen sharing is off"));
        }
        let Some(engine) = st.engine.as_ref() else {
            return Err(RpcError::new(codes::INVALID_STATE, "screen sharing stopped"));
        };
        let control = t.control && engine.control;
        let url = format!("{}/stream{}", engine.base, if control { "" } else { "?view=1" });
        st.next_viewer += 1;
        let id = st.next_viewer;
        let (kill_tx, kill) = oneshot::channel();
        if let Some((_, old)) = st.viewer.replace((id, kill_tx)) {
            let _ = old.send(());
        }
        st.epoch += 1;
        drop(st);
        self.ctx.audit.log("screen.attach", json!({"device_id": device_id, "control": control}));
        self.changed();
        Ok(Attached { url, control, id, kill })
    }

    /// Pipe screen-relay messages between the engine and a device until
    /// either side closes, another viewer takes over, or sharing stops.
    /// `from_device` and `to_device` carry whole, decrypted messages.
    pub async fn pipe(
        self: &Arc<Self>,
        a: Attached,
        mut from_device: mpsc::Receiver<Vec<u8>>,
        to_device: mpsc::Sender<Vec<u8>>,
    ) -> Result<()> {
        let id = a.id;
        let r = self.pipe_inner(a, &mut from_device, &to_device).await;
        let last = {
            let mut st = self.state.lock().unwrap();
            if st.viewer.as_ref().is_some_and(|(v, _)| *v == id) {
                st.viewer = None;
                true
            } else {
                false
            }
        };
        if last {
            self.changed();
            self.schedule_idle_stop();
        }
        r
    }

    async fn pipe_inner(
        &self,
        mut a: Attached,
        from_device: &mut mpsc::Receiver<Vec<u8>>,
        to_device: &mpsc::Sender<Vec<u8>>,
    ) -> Result<()> {
        let mut req = a.url.as_str().into_client_request()?;
        // A token goes in the subprotocol list, never the URL (screen-relay's rule).
        let protocols = match &self.ctx.cfg.screen_engine_token_file {
            Some(f) => {
                let token = std::fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?;
                format!("{}, bearer.{}", wire::SUBPROTOCOL, token.trim())
            }
            None => wire::SUBPROTOCOL.to_string(),
        };
        req.headers_mut().insert("Sec-WebSocket-Protocol", protocols.parse()?);
        let (ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(req))
            .await
            .map_err(|_| anyhow!("screen-relay did not answer"))?
            .context("connect to screen-relay")?;
        let (mut sink, mut stream) = ws.split();
        loop {
            tokio::select! {
                _ = &mut a.kill => return Ok(()),
                msg = stream.next() => match msg {
                    Some(Ok(WsMsg::Binary(b))) => {
                        if to_device.send(b.to_vec()).await.is_err() { return Ok(()); }
                    }
                    Some(Ok(WsMsg::Close(f))) => {
                        bail!("screen-relay closed the stream{}", f.map(|f| format!(": {}", f.reason)).unwrap_or_default())
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => bail!("screen-relay closed the stream"),
                },
                msg = from_device.recv() => {
                    let Some(m) = msg else { return Ok(()) };
                    // ACKs always (they drive the rate control); input only
                    // when this viewer may control; nothing else.
                    let allowed = match wire::header(&m) {
                        Ok(h) if h.kind == wire::T_ACK => true,
                        Ok(h) if h.kind == wire::T_INPUT => a.control,
                        _ => false,
                    };
                    if allowed {
                        sink.send(WsMsg::Binary(m.into())).await?;
                    }
                }
            }
        }
    }
}

/// The message in screen-relay's JSON error output, or its raw text.
fn screen_relay_error(stderr: &[u8], stdout: &[u8]) -> String {
    for b in [stderr, stdout] {
        if let Ok(v) = serde_json::from_slice::<Value>(b) {
            let e = if v["error"].is_object() { &v["error"] } else { &v };
            if let Some(m) = e["message"].as_str() {
                return match e["next"].as_str().or(e["fix"].as_str()) {
                    Some(n) => format!("{m} ({n})"),
                    None => m.to_string(),
                };
            }
        }
    }
    let text = String::from_utf8_lossy(if stderr.is_empty() { stdout } else { stderr });
    text.trim().lines().last().unwrap_or("no output").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_command_is_quoted() {
        let words = vec!["/usr/bin/chromium".to_string(), "--user-data-dir=/home/a b/it's".into()];
        assert_eq!(command_line(&words), r"'/usr/bin/chromium' '--user-data-dir=/home/a b/it'\''s'");
    }

    #[test]
    fn errors_come_from_json_when_there_is_some() {
        let e = br#"{"error":{"reason":"not_installed","message":"The engine is not built","next":"screen-relay install"}}"#;
        assert_eq!(screen_relay_error(e, b""), "The engine is not built (screen-relay install)");
        assert_eq!(screen_relay_error(b"line one\nboom\n", b""), "boom");
    }

    #[test]
    fn gnome_and_kde_on_wayland_need_a_portal() {
        let s = |w: Option<&str>, d: &str| Session { wayland: w.map(Into::into), desktop: d.into(), ..Default::default() };
        assert!(s(Some("wayland-0"), "GNOME").portal_only());
        assert!(s(Some("wayland-0"), "KDE").portal_only());
        assert!(!s(Some("wayland-1"), "Hyprland").portal_only());
        assert!(!s(None, "GNOME").portal_only());
    }
}
