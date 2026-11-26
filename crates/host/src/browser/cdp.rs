//! The Chrome DevTools Protocol over `--remote-debugging-pipe`: the browser
//! reads commands on its fd 3 and writes replies and events on its fd 4,
//! each message JSON ended by a NUL byte. Nothing listens on a port, so no
//! other program on this computer (and nothing in a sandbox) can drive it.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a command may take before it counts as failed. Navigation
/// waits elsewhere; this only bounds one reply.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A message bigger than this from the browser ends the connection
/// (a full-page screenshot of a long page is the largest we ask for).
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

type Reply = Result<Value, String>;

/// One event from the browser: its method, params and the target session
/// it came from (none for the browser itself).
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session: Option<String>,
}

pub struct Cdp {
    out: std::sync::mpsc::Sender<Vec<u8>>,
    next: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
}

/// The two pipes: the browser's ends go to the child as fd 3 and 4.
pub struct Pipes {
    /// The browser reads commands from this (its fd 3).
    pub child_in: OwnedFd,
    /// The browser writes to this (its fd 4).
    pub child_out: OwnedFd,
    ours_write: OwnedFd,
    ours_read: OwnedFd,
}

fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: fds is a valid two-element array for pipe to fill.
    #[cfg(target_os = "linux")]
    let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(target_os = "linux"))]
    let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if r != 0 {
        bail!("pipe: {}", std::io::Error::last_os_error());
    }
    // SAFETY: pipe just returned these two fresh descriptors, owned by nobody else.
    let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // Out of the way of 0..=4, which the child's ends are moved onto, and
    // closed on exec everywhere else.
    Ok((high(a)?, high(b)?))
}

fn high(fd: OwnedFd) -> Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC on a valid descriptor returns a new one or -1.
    let n = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if n < 0 {
        bail!("fcntl: {}", std::io::Error::last_os_error());
    }
    // SAFETY: n is a fresh descriptor from fcntl.
    Ok(unsafe { OwnedFd::from_raw_fd(n) })
}

impl Pipes {
    pub fn new() -> Result<Pipes> {
        let (child_in, ours_write) = pipe()?;
        let (ours_read, child_out) = pipe()?;
        Ok(Pipes { child_in, child_out, ours_write, ours_read })
    }

    /// For the child, between fork and exec: put its ends on fd 3 and 4.
    /// Both are at 10 or above, so neither move overwrites the other.
    pub fn install_in_child(child_in: i32, child_out: i32) -> std::io::Result<()> {
        // SAFETY: dup2 is async-signal-safe; it clears close-on-exec on the copy.
        unsafe {
            if libc::dup2(child_in, 3) < 0 || libc::dup2(child_out, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Start talking: once the child has its copies, ours become the
    /// connection, and every event goes to `events`.
    pub fn connect(self, events: mpsc::UnboundedSender<CdpEvent>, closed: oneshot::Sender<()>) -> Cdp {
        let Pipes { child_in, child_out, ours_write, ours_read } = self;
        // The child has its own copies now.
        drop(child_in);
        drop(child_out);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>> = Arc::default();
        let (out, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut w = std::fs::File::from(ours_write);
        std::thread::Builder::new()
            .name("browser-cdp-write".into())
            .spawn(move || {
                while let Ok(mut msg) = out_rx.recv() {
                    msg.push(0);
                    if w.write_all(&msg).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn cdp writer");
        let p = pending.clone();
        let r = std::fs::File::from(ours_read);
        std::thread::Builder::new()
            .name("browser-cdp-read".into())
            .spawn(move || {
                let mut r = BufReader::with_capacity(1 << 20, r);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match r.read_until(0, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if buf.last() == Some(&0) {
                        buf.pop();
                    }
                    if buf.len() > MAX_MESSAGE {
                        tracing::warn!(len = buf.len(), "browser: message too large, disconnecting");
                        break;
                    }
                    let Ok(msg) = serde_json::from_slice::<Value>(&buf) else { continue };
                    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                        let reply = match msg.get("error") {
                            Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        if let Some(tx) = p.lock().unwrap().remove(&id) {
                            let _ = tx.send(reply);
                        }
                    } else if let Some(method) = msg.get("method").and_then(Value::as_str) {
                        let ev = CdpEvent {
                            method: method.to_string(),
                            params: msg.get("params").cloned().unwrap_or(Value::Null),
                            session: msg.get("sessionId").and_then(Value::as_str).map(str::to_string),
                        };
                        if events.send(ev).is_err() {
                            break;
                        }
                    }
                }
                // The browser is gone: every waiting call fails now.
                for (_, tx) in p.lock().unwrap().drain() {
                    let _ = tx.send(Err("the browser closed".into()));
                }
                let _ = closed.send(());
            })
            .expect("spawn cdp reader");
        Cdp { out, next: AtomicU64::new(1), pending }
    }
}

impl Cdp {
    /// Send a command to the browser (`session` none) or one of its targets.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.call_within(method, params, session, CALL_TIMEOUT).await
    }

    pub async fn call_within(&self, method: &str, params: Value, session: Option<&str>, limit: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.out.send(serde_json::to_vec(&msg)?).is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("the browser closed");
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("{method}: {e}")),
            Ok(Err(_)) => bail!("the browser closed"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("{method} took longer than {}s", limit.as_secs())).context("the browser is not answering")
            }
        }
    }

    /// Send without waiting for the reply (frame acknowledgements).
    pub fn send(&self, method: &str, params: Value, session: Option<&str>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        if let Ok(v) = serde_json::to_vec(&msg) {
            let _ = self.out.send(v);
        }
    }
}
