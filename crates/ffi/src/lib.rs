//! UniFFI bindings for the AgentOS iOS app.
//!
//! The phone does exactly what `agenthost-cli`'s [`remote`] module does
//! (pairing over `Noise_XXpsk3`, then `Noise_IK` connections through the relay,
//! then the JSON-RPC client protocol). This crate only adapts that code to a
//! foreign-language API, so the handshake, framing and protocol exist once,
//! in Rust.
//!
//! The API is deliberately small:
//!
//! * [`pair`] turns a scanned QR payload into [`DeviceCredentials`], telling a
//!   [`PairingListener`] the 6-digit confirmation code as soon as it is known.
//! * [`RemoteClient::connect`] opens a tunnel with stored credentials;
//!   [`RemoteClient::call`] sends one JSON-RPC request (params and result as
//!   JSON text); a [`ConnectionListener`] receives notifications and one final
//!   `on_closed`; [`RemoteClient::disconnect`] closes the tunnel.
//!
//! All work runs on a Tokio runtime owned by this crate, so callers may call
//! from any thread. Listener callbacks arrive on that runtime's threads, never
//! on the caller's main thread: hop to the main actor before touching UI.
//!
//! Key material is handed to the caller as base64url strings (exactly the
//! fields of [`remote::DeviceState`]). This crate never stores anything; the
//! app keeps credentials in the iOS Keychain.

use agenthost_cli::remote::{self, Closed, DeviceState};
use agenthost_cli::Client;
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::methods;
use agenthost_proto::tunnel::PairingQr;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

uniffi::setup_scaffolding!();

// ---------------------------------------------------------------------------
// Close codes and error codes, re-exported so Swift does not hard-code them.
// ---------------------------------------------------------------------------

/// Relay close code: the device was removed on the desktop.
pub const CLOSE_DEVICE_REVOKED: u16 = 4401;
/// Relay close code: unknown device key, or a used or expired pairing code.
pub const CLOSE_NOT_ALLOWED: u16 = 4403;
/// Relay close code: the host is not connected to the relay.
pub const CLOSE_HOST_OFFLINE: u16 = 4404;

/// Relay close codes and JSON-RPC error codes the app reacts to.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ProtocolConstants {
    /// Client protocol version this library speaks.
    pub protocol_version: u32,
    pub close_device_revoked: u16,
    pub close_not_allowed: u16,
    pub close_host_offline: u16,
    /// JSON-RPC: the device's capabilities do not allow the method.
    pub error_capability_denied: i64,
    /// JSON-RPC: this app's protocol version is too old ("update required").
    pub error_update_required: i64,
    pub error_not_found: i64,
    pub error_invalid_state: i64,
    pub error_unavailable: i64,
}

/// Constants shared with the host (close codes, error codes, protocol version).
#[uniffi::export]
pub fn protocol_constants() -> ProtocolConstants {
    use agenthost_proto::jsonrpc::codes;
    ProtocolConstants {
        protocol_version: agenthost_proto::PROTOCOL_VERSION,
        close_device_revoked: CLOSE_DEVICE_REVOKED,
        close_not_allowed: CLOSE_NOT_ALLOWED,
        close_host_offline: CLOSE_HOST_OFFLINE,
        error_capability_denied: codes::CAPABILITY_DENIED,
        error_update_required: codes::UPDATE_REQUIRED,
        error_not_found: codes::NOT_FOUND,
        error_invalid_state: codes::INVALID_STATE,
        error_unavailable: codes::UNAVAILABLE,
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure the API reports.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentHostError {
    /// The host answered a request with a JSON-RPC error. `code` is one of the
    /// codes in [`ProtocolConstants`] (for example capability denied or update
    /// required) or a standard JSON-RPC code. `data_json` is the error's
    /// `data` member as JSON text, when present.
    #[error("{detail} (code {code})")]
    Rpc {
        code: i64,
        detail: String,
        data_json: Option<String>,
    },
    /// The relay or host closed the connection with a WebSocket close code
    /// (4401 revoked, 4403 not allowed, 4404 host offline, …).
    #[error("closed with code {code}: {reason}")]
    Closed { code: u16, reason: String },
    /// Pairing did not complete (refused on the desktop, timed out, …).
    #[error("pairing failed: {detail}")]
    Pairing { detail: String },
    /// An argument was malformed (bad QR payload, bad credentials, bad JSON).
    #[error("invalid input: {detail}")]
    InvalidInput { detail: String },
    /// Network or tunnel failure without a close code.
    #[error("connection failed: {detail}")]
    Transport { detail: String },
    /// The client was disconnected; connect again.
    #[error("not connected")]
    NotConnected,
}

impl AgentHostError {
    fn rpc(e: RpcError) -> Self {
        AgentHostError::Rpc {
            code: e.code,
            detail: e.message,
            data_json: e.data.map(|d| d.to_string()),
        }
    }

    /// Classify an error from `agenthost-cli`, keeping close codes and
    /// JSON-RPC errors typed.
    fn from_anyhow(e: anyhow::Error, otherwise: fn(String) -> AgentHostError) -> Self {
        if let Some(c) = e.downcast_ref::<Closed>() {
            return AgentHostError::Closed {
                code: c.code,
                reason: c.reason.clone(),
            };
        }
        if let Some(r) = e.downcast_ref::<RpcError>() {
            return AgentHostError::rpc(r.clone());
        }
        otherwise(format!("{e:#}"))
    }
}

fn transport(detail: String) -> AgentHostError {
    AgentHostError::Transport { detail }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("agenthost-ffi")
            .enable_all()
            .build()
            .expect("start the AgentOS tokio runtime")
    })
}

/// Aborts the spawned task when the foreign caller drops the future
/// (for example when a Swift `Task` is cancelled).
struct AbortOnDrop<T>(JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Run `f` on our runtime and await it from whatever executor polls us.
async fn on_runtime<F, T>(f: F) -> Result<T, AgentHostError>
where
    F: Future<Output = Result<T, AgentHostError>> + Send + 'static,
    T: Send + 'static,
{
    let mut h = AbortOnDrop(runtime().spawn(f));
    match (&mut h.0).await {
        Ok(r) => r,
        Err(e) if e.is_cancelled() => Err(transport("cancelled".into())),
        Err(e) => Err(transport(format!("internal error: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

/// What a paired device must keep, and nothing else. Same fields as the
/// CLI's `DeviceState`; binary values are base64url without padding.
/// `noise_private` and `relay_private` are secret: store them in the Keychain.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeviceCredentials {
    pub relay_url: String,
    pub relay_id: String,
    /// The host's X25519 static key (public), checked during every handshake.
    pub host_key: String,
    pub host_name: String,
    pub device_id: String,
    pub host_id: String,
    /// The device's X25519 static private key. Secret.
    pub noise_private: String,
    /// The device's Ed25519 relay key seed. Secret.
    pub relay_private: String,
}

impl From<DeviceState> for DeviceCredentials {
    fn from(s: DeviceState) -> Self {
        DeviceCredentials {
            relay_url: s.relay_url,
            relay_id: s.relay_id,
            host_key: s.host_key,
            host_name: s.host_name,
            device_id: s.device_id,
            host_id: s.host_id,
            noise_private: s.noise_private,
            relay_private: s.relay_private,
        }
    }
}

impl From<DeviceCredentials> for DeviceState {
    fn from(c: DeviceCredentials) -> Self {
        DeviceState {
            relay_url: c.relay_url,
            relay_id: c.relay_id,
            host_key: c.host_key,
            host_name: c.host_name,
            device_id: c.device_id,
            host_id: c.host_id,
            noise_private: c.noise_private,
            relay_private: c.relay_private,
        }
    }
}

/// The public parts of a pairing QR code, for showing before pairing starts.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PairingQrInfo {
    pub host_name: String,
    pub relay_url: String,
    pub relay_id: String,
}

/// Check that a scanned string is an AgentOS pairing code without using it.
#[uniffi::export]
pub fn parse_pairing_qr(qr: String) -> Result<PairingQrInfo, AgentHostError> {
    let q = PairingQr::decode(&qr).map_err(|e| AgentHostError::InvalidInput {
        detail: e.to_string(),
    })?;
    Ok(PairingQrInfo {
        host_name: q.host_name,
        relay_url: q.relay_url,
        relay_id: q.relay_id,
    })
}

/// Receives the confirmation code during [`pair`].
#[uniffi::export(foreign)]
pub trait PairingListener: Send + Sync {
    /// The handshake finished; show this code (`"123 456"`) and ask the user
    /// to confirm the same code on the desktop. Called at most once.
    fn on_short_code(&self, code: String);
}

/// Pair with the host named in a scanned QR payload.
///
/// Connects to the relay, runs the `XXpsk3` handshake (checking the host key
/// against the QR code), reports the short code to `listener`, then waits up
/// to 150 s for the user to confirm on the desktop. Returns the credentials to
/// store; nothing is persisted by this call. A QR code works once and for two
/// minutes: a reused or expired one fails with `Closed { code: 4403 }`.
#[uniffi::export]
pub async fn pair(
    qr: String,
    device_name: String,
    listener: Arc<dyn PairingListener>,
) -> Result<DeviceCredentials, AgentHostError> {
    PairingQr::decode(&qr).map_err(|e| AgentHostError::InvalidInput {
        detail: e.to_string(),
    })?;
    on_runtime(async move {
        let state = remote::pair(&qr, &device_name, |code| {
            listener.on_short_code(code.to_string())
        })
        .await
        .map_err(|e| AgentHostError::from_anyhow(e, |detail| AgentHostError::Pairing { detail }))?;
        Ok(DeviceCredentials::from(state))
    })
    .await
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Receives everything the host pushes over one connection.
#[uniffi::export(foreign)]
pub trait ConnectionListener: Send + Sync {
    /// A JSON-RPC notification: `event`, `session.delta`, `session.changed`,
    /// `device.revoked`, … with its params as JSON text. Delivered in order.
    fn on_notification(&self, method: String, params_json: String);
    /// The tunnel closed. Called once, after the last notification, unless the
    /// app called [`RemoteClient::disconnect`]. `code` is the relay's close
    /// code when known (4401 = this device was removed, also reported when the
    /// host sent `device.revoked`; 4404 = host offline), or 0.
    fn on_closed(&self, reason: String, code: u16);
}

/// What `hello` returned.
#[derive(Debug, Clone, uniffi::Record)]
pub struct HelloInfo {
    pub host_id: String,
    pub host_version: String,
    pub protocol_version: u32,
    /// `read`, `sessions`, `approvals` (a phone never gets `admin`).
    pub capabilities: Vec<String>,
    pub device_id: Option<String>,
}

/// One live, greeted connection to the host through the relay.
///
/// Reconnecting means calling [`RemoteClient::connect`] again; after a
/// reconnect, re-subscribe with `events.subscribe` and the last seen `seq`.
impl RemoteClient {
    fn connected(&self) -> Result<Arc<Client>, AgentHostError> {
        self.client
            .lock()
            .unwrap()
            .clone()
            .ok_or(AgentHostError::NotConnected)
    }
}

async fn read_limited(path: &str) -> Result<Vec<u8>, AgentHostError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| AgentHostError::InvalidInput { detail: format!("read {path}: {e}") })?;
    if meta.len() > agenthost_proto::MAX_ATTACHMENT_BYTES {
        return Err(AgentHostError::InvalidInput {
            detail: format!(
                "the file is too large: files can be up to {} MB",
                agenthost_proto::MAX_ATTACHMENT_BYTES / (1024 * 1024)
            ),
        });
    }
    tokio::fs::read(path)
        .await
        .map_err(|e| AgentHostError::InvalidInput { detail: format!("read {path}: {e}") })
}

#[derive(uniffi::Object)]
pub struct RemoteClient {
    client: Mutex<Option<Arc<Client>>>,
    hello: HelloInfo,
    /// Set by `disconnect()` so `on_closed` is not reported for it.
    closed_by_app: Arc<AtomicBool>,
    /// Cleared by the pump when the tunnel closes, before `on_closed`.
    tunnel_open: Arc<AtomicBool>,
}

#[uniffi::export]
impl RemoteClient {
    /// Connect with stored credentials: relay authentication, `Noise_IK`
    /// handshake, then `hello`. Fails with `Closed { code: 4404 }` when the
    /// host is offline, `Closed { code: 4403 }` when this device is not (or no
    /// longer) allowed, and `Rpc { code: -32002 }` when the app is too old.
    #[uniffi::constructor]
    pub async fn connect(
        credentials: DeviceCredentials,
        listener: Arc<dyn ConnectionListener>,
    ) -> Result<Arc<RemoteClient>, AgentHostError> {
        on_runtime(async move {
            let state = DeviceState::from(credentials);
            let (client, mut notes, mut closed) = remote::connect(&state)
                .await
                .map_err(|e| AgentHostError::from_anyhow(e, transport))?;
            let hello = HelloInfo {
                host_id: client.hello.host_id.clone(),
                host_version: client.hello.host_version.clone(),
                protocol_version: client.hello.protocol_version,
                capabilities: client
                    .hello
                    .capabilities
                    .iter()
                    .map(|c| {
                        serde_json::to_value(c)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    })
                    .collect(),
                device_id: client.hello.device_id.clone(),
            };
            let closed_by_app = Arc::new(AtomicBool::new(false));
            let by_app = closed_by_app.clone();
            let tunnel_open = Arc::new(AtomicBool::new(true));
            let open = tunnel_open.clone();
            tokio::spawn(async move {
                let mut revoked = false;
                while let Some(n) = notes.recv().await {
                    if n.method == methods::N_DEVICE_REVOKED {
                        revoked = true;
                    }
                    listener.on_notification(n.method, n.params.to_string());
                }
                // The notification stream ends when the tunnel closes; the
                // tunnel task records why just before that.
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), closed.wait_for(|r| r.is_some()))
                        .await;
                // Before `on_closed`, so the listener sees `is_connected()` false.
                open.store(false, Ordering::SeqCst);
                if by_app.load(Ordering::SeqCst) {
                    return;
                }
                let (reason, code) = match closed.borrow().clone() {
                    Some(c) => (c.to_string(), c.code),
                    None => ("connection closed".to_string(), 0),
                };
                let code = if revoked {
                    CLOSE_DEVICE_REVOKED
                } else {
                    code
                };
                listener.on_closed(reason, code);
            });
            Ok(Arc::new(RemoteClient {
                client: Mutex::new(Some(Arc::new(client))),
                hello,
                closed_by_app,
                tunnel_open,
            }))
        })
        .await
    }

    /// The host's answer to `hello` on this connection.
    pub fn hello(&self) -> HelloInfo {
        self.hello.clone()
    }

    /// Whether `disconnect()` has not been called and the tunnel is believed open.
    pub fn is_connected(&self) -> bool {
        self.client.lock().unwrap().is_some() && self.tunnel_open.load(Ordering::SeqCst)
    }

    /// Send one request. `params_json` is a JSON object (or `""` for none);
    /// the result is returned as JSON text. Host errors come back as
    /// `Rpc { code, detail, data_json }`. Times out after 60 s.
    pub async fn call(
        &self,
        method: String,
        params_json: String,
    ) -> Result<String, AgentHostError> {
        let params: serde_json::Value = if params_json.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&params_json).map_err(|e| AgentHostError::InvalidInput {
                detail: format!("params are not JSON: {e}"),
            })?
        };
        let client = self
            .client
            .lock()
            .unwrap()
            .clone()
            .ok_or(AgentHostError::NotConnected)?;
        on_runtime(async move {
            match tokio::time::timeout(Duration::from_secs(60), client.request(&method, params))
                .await
            {
                Ok(Ok(v)) => Ok(v.to_string()),
                Ok(Err(e)) => Err(AgentHostError::rpc(e)),
                Err(_) => Err(transport(format!("{method} timed out"))),
            }
        })
        .await
    }

    /// Send a file to the host for the next prompt (protocol 2 hosts). `name`
    /// defaults to the file's own name and `mime` is guessed from the name
    /// when absent. Returns the `Attachment` as JSON text; pass its
    /// `attachment_id` in `session.prompt`'s `attachments`. Files can be up to
    /// 25 MB. The file is read here, so a temporary copy may be deleted after.
    pub async fn upload_file(
        &self,
        path: String,
        name: Option<String>,
        mime: Option<String>,
    ) -> Result<String, AgentHostError> {
        let client = self.connected()?;
        on_runtime(async move {
            let data = read_limited(&path).await?;
            let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".into())
            });
            let a = tokio::time::timeout(
                Duration::from_secs(600),
                client.upload(&name, mime.as_deref(), &data, |_, _| {}),
            )
            .await
            .map_err(|_| transport("upload timed out".into()))?
            .map_err(|e| AgentHostError::from_anyhow(e, transport))?;
            Ok(serde_json::to_string(&a).unwrap())
        })
        .await
    }

    /// Transcribe a WAV recording with the host's speech model and return the
    /// text. Fails with `Rpc { code: -32006 (unavailable) }` and data
    /// `{"needs_download": true}` when the model is not installed: ask the
    /// user, then call `voice.download`.
    pub async fn transcribe_wav_file(
        &self,
        path: String,
        language: Option<String>,
    ) -> Result<String, AgentHostError> {
        let client = self.connected()?;
        on_runtime(async move {
            let data = read_limited(&path).await?;
            tokio::time::timeout(
                Duration::from_secs(300),
                client.transcribe_wav(&data, language.as_deref()),
            )
            .await
            .map_err(|_| transport("transcription timed out".into()))?
            .map_err(|e| AgentHostError::from_anyhow(e, transport))
        })
        .await
    }

    /// Close the tunnel. `on_closed` is not called for an app-initiated
    /// disconnect. Safe to call more than once.
    pub fn disconnect(&self) {
        self.closed_by_app.store(true, Ordering::SeqCst);
        // Dropping the last `Client` ends the outbound queue, which ends the
        // tunnel task and closes the WebSocket.
        let _ = self.client.lock().unwrap().take();
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ---------------------------------------------------------------------------
// Screen sharing
// ---------------------------------------------------------------------------

/// One H.264 access unit from screen-relay: Annex B NAL units, with SPS and
/// PPS on keyframes. Decode in order; ACK each with [`ScreenSession::ack`]
/// once it was drawn (or dropped for a newer one).
#[derive(Debug, Clone, uniffi::Record)]
pub struct ScreenFrame {
    pub generation: u32,
    pub sequence: u32,
    pub keyframe: bool,
    /// Desktop size in pixels.
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

/// Receives one screen stream.
#[uniffi::export(foreign)]
pub trait ScreenListener: Send + Sync {
    /// A frame, in order. Called on a background thread.
    fn on_frame(&self, frame: ScreenFrame);
    /// The stream ended (host stopped sharing, another viewer took over, the
    /// network dropped). Called once, unless the app called `close()`.
    fn on_closed(&self, reason: String);
}

/// A screen stream on a tunnel of its own. Get a ticket with `screen.start`
/// on the normal connection first. Input methods take positions as fractions
/// of the desktop (0…1) and do nothing unless the ticket allows control.
#[derive(uniffi::Object)]
pub struct ScreenSession {
    tx: Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    generation: Arc<std::sync::atomic::AtomicU32>,
    closed_by_app: Arc<AtomicBool>,
}

fn coord(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * agenthost_proto::screen::COORD_MAX as f64).round() as u16
}

impl ScreenSession {
    fn input(&self, events: &[agenthost_proto::screen::Input]) {
        let generation = self.generation.load(Ordering::SeqCst);
        if generation == 0 {
            return; // No frame yet: the engine would reject the generation.
        }
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            for e in events {
                // Never block the UI thread: drop input if the link is that far behind.
                let _ = tx.try_send(e.message(generation));
            }
        }
    }
}

#[uniffi::export]
impl ScreenSession {
    #[uniffi::constructor]
    pub async fn open(
        credentials: DeviceCredentials,
        ticket: String,
        listener: Arc<dyn ScreenListener>,
    ) -> Result<Arc<ScreenSession>, AgentHostError> {
        on_runtime(async move {
            let state = DeviceState::from(credentials);
            let (mut incoming, sender, mut closed) = remote::open_screen(&state, &ticket)
                .await
                .map_err(|e| AgentHostError::from_anyhow(e, transport))?
                .split();
            let generation = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let closed_by_app = Arc::new(AtomicBool::new(false));
            let session = Arc::new(ScreenSession {
                tx: Mutex::new(Some(sender)),
                generation: generation.clone(),
                closed_by_app: closed_by_app.clone(),
            });
            tokio::spawn(async move {
                use agenthost_proto::screen::{decode, ServerMessage};
                while let Some(msg) = incoming.recv().await {
                    match decode(&msg) {
                        Ok(ServerMessage::Frame(f)) => {
                            generation.store(f.generation, Ordering::SeqCst);
                            listener.on_frame(ScreenFrame {
                                generation: f.generation,
                                sequence: f.sequence,
                                keyframe: f.keyframe,
                                width: f.width,
                                height: f.height,
                                data: f.data,
                            });
                        }
                        Ok(_) => {}
                        Err(e) => {
                            if !closed_by_app.load(Ordering::SeqCst) {
                                listener.on_closed(format!("bad message from the host: {e}"));
                            }
                            return;
                        }
                    }
                }
                let _ = tokio::time::timeout(Duration::from_secs(2), closed.wait_for(|r| r.is_some())).await;
                if !closed_by_app.load(Ordering::SeqCst) {
                    let reason = closed.borrow().clone().unwrap_or_else(|| "the stream ended".into());
                    listener.on_closed(reason);
                }
            });
            Ok(session)
        })
        .await
    }

    /// Acknowledge a frame. `wait_us`: how long it waited between arriving
    /// and being drawn, so a busy phone isn't mistaken for a slow network.
    pub fn ack(&self, generation: u32, sequence: u32, wait_us: u32) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.try_send(agenthost_proto::screen::ack(generation, sequence, wait_us));
        }
    }

    pub fn pointer_move(&self, x: f64, y: f64) {
        use agenthost_proto::screen::Input;
        self.input(&[Input::PointerMove { x: coord(x), y: coord(y) }]);
    }

    /// Press and release a button (1 left, 2 middle, 3 right) at a point.
    pub fn click(&self, x: f64, y: f64, button: u16) {
        use agenthost_proto::screen::Input;
        let (x, y) = (coord(x), coord(y));
        self.input(&[
            Input::PointerMove { x, y },
            Input::Button { button, down: true, x, y },
            Input::Button { button, down: false, x, y },
        ]);
    }

    /// Hold or release a button, for drags.
    pub fn button(&self, x: f64, y: f64, button: u16, down: bool) {
        use agenthost_proto::screen::Input;
        self.input(&[Input::Button { button, down, x: coord(x), y: coord(y) }]);
    }

    /// Scroll by notches at a point (positive `dy` scrolls down).
    pub fn scroll(&self, x: f64, y: f64, dx: i32, dy: i32) {
        use agenthost_proto::screen::Input;
        let (x, y) = (coord(x), coord(y));
        let mut events = vec![];
        if dy != 0 {
            events.push(Input::Wheel { horizontal: false, delta: dy * 120, x, y });
        }
        if dx != 0 {
            events.push(Input::Wheel { horizontal: true, delta: dx * 120, x, y });
        }
        self.input(&events);
    }

    /// Type text as characters (layout independent).
    pub fn type_text(&self, text: String) {
        use agenthost_proto::screen::Input;
        let events: Vec<Input> = text.chars().map(Input::Text).collect();
        self.input(&events);
    }

    /// Press and release one key by HID usage (Enter 40, Escape 41,
    /// Backspace 42, Tab 43, arrows 79–82), with modifiers held around it
    /// (HID 224–231: left Ctrl, Shift, Alt, GUI/⌘, then the right ones).
    pub fn key(&self, hid: u16, modifiers: Vec<u16>) {
        use agenthost_proto::screen::Input;
        let mut events: Vec<Input> = modifiers.iter().map(|&m| Input::Key { hid: m, down: true }).collect();
        events.push(Input::Key { hid, down: true });
        events.push(Input::Key { hid, down: false });
        events.extend(modifiers.iter().rev().map(|&m| Input::Key { hid: m, down: false }));
        self.input(&events);
    }

    /// Close the stream. `on_closed` is not called for it.
    pub fn close(&self) {
        self.closed_by_app.store(true, Ordering::SeqCst);
        let _ = self.tx.lock().unwrap().take();
    }
}

impl Drop for ScreenSession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_carries_its_code() {
        let c = Closed { code: 4401, reason: "bye".into() };
        assert_eq!(c.code, 4401);
        assert!(c.to_string().contains("4401"));
        assert_eq!(Closed::other("tunnel integrity failure: x").code, 0);
        assert_eq!(Closed::other("tunnel integrity failure: x").to_string(), "tunnel integrity failure: x");
    }

    #[test]
    fn bad_qr_is_invalid_input() {
        assert!(matches!(
            parse_pairing_qr("https://example.com".into()),
            Err(AgentHostError::InvalidInput { .. })
        ));
    }
}
