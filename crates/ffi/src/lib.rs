//! UniFFI bindings for the agenthost iOS app.
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
            .expect("start the agenthost tokio runtime")
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

/// Check that a scanned string is an agenthost pairing code without using it.
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
#[derive(uniffi::Object)]
pub struct RemoteClient {
    client: Mutex<Option<Arc<Client>>>,
    hello: HelloInfo,
    /// Set by `disconnect()` so `on_closed` is not reported for it.
    closed_by_app: Arc<AtomicBool>,
    pump: Mutex<Option<JoinHandle<()>>>,
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
            let pump = tokio::spawn(async move {
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
                let reason = closed
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "connection closed".to_string());
                if by_app.load(Ordering::SeqCst) {
                    return;
                }
                let code = if revoked {
                    CLOSE_DEVICE_REVOKED
                } else {
                    close_code_from_reason(&reason)
                };
                listener.on_closed(reason, code);
            });
            Ok(Arc::new(RemoteClient {
                client: Mutex::new(Some(Arc::new(client))),
                hello,
                closed_by_app,
                pump: Mutex::new(Some(pump)),
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
        let open = self.client.lock().unwrap().is_some();
        let pumping = self
            .pump
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| !p.is_finished())
            .unwrap_or(false);
        open && pumping
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

/// Pull the close code out of the CLI's closed-reason text
/// (`"… (code 4401: …)"`), or 0 when there is none.
fn close_code_from_reason(reason: &str) -> u16 {
    reason
        .split("(code ")
        .nth(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_codes_are_parsed() {
        let r = Closed {
            code: 4401,
            reason: "bye".into(),
        }
        .to_string();
        assert_eq!(close_code_from_reason(&r), 4401);
        let r = Closed {
            code: 4404,
            reason: String::new(),
        }
        .to_string();
        assert_eq!(close_code_from_reason(&r), 4404);
        assert_eq!(close_code_from_reason("tunnel integrity failure: x"), 0);
    }

    #[test]
    fn bad_qr_is_invalid_input() {
        assert!(matches!(
            parse_pairing_qr("https://example.com".into()),
            Err(AgentHostError::InvalidInput { .. })
        ));
    }
}
