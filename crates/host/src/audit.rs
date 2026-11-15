//! Security-relevant log lines (capability refusals, pairing, revocation),
//! appended as JSON lines to an owner-only file in the data directory.

use crate::util::now_ms;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Audit {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl Audit {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn log(&self, what: &str, detail: Value) {
        tracing::warn!(what, %detail, "audit");
        let Some(p) = &self.path else { return };
        let _g = self.lock.lock().unwrap();
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(p)
        {
            let _ = writeln!(
                f,
                "{}",
                json!({"ts": now_ms(), "what": what, "detail": detail})
            );
        }
    }
}
