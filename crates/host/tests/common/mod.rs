#![allow(dead_code)]

use agenthost_cli::{as_event, Client};
use agenthost_host::{Host, HostConfig};
use agenthost_proto::jsonrpc::Notification;
use agenthost_proto::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::sync::mpsc;

static BUILD: Once = Once::new();

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Path to the fake agent binary, building it if needed.
pub fn fake_agent() -> PathBuf {
    let p = workspace_root().join("target/debug/agenthost-fake-agent");
    BUILD.call_once(|| {
        let st = std::process::Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "agenthost-fake-agent"])
            .current_dir(workspace_root())
            .status()
            .expect("cargo build fake agent");
        assert!(st.success());
    });
    p
}

pub struct TestHost {
    pub dir: tempfile::TempDir,
    pub host: Arc<Host>,
    pub task: tokio::task::JoinHandle<()>,
    pub workspace: PathBuf,
}

pub fn test_config(dir: &Path) -> HostConfig {
    // Short root so socket paths stay well under the limit.
    let mut cfg = HostConfig::in_dir(dir);
    cfg.enable_fake_agent = true;
    cfg.fake_agent_unsandboxed = true;
    cfg.fake_agent_path = Some(fake_agent());
    cfg.home_dir = dir.join("home");
    std::fs::create_dir_all(&cfg.home_dir).unwrap();
    cfg
}

pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ah")
        .tempdir_in("/tmp")
        .unwrap()
}

impl TestHost {
    pub async fn start() -> TestHost {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(f: impl FnOnce(&mut HostConfig)) -> TestHost {
        let dir = short_tempdir();
        let mut cfg = test_config(dir.path());
        f(&mut cfg);
        Self::start_in(dir, cfg).await
    }

    pub async fn start_in(dir: tempfile::TempDir, cfg: HostConfig) -> TestHost {
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&workspace).unwrap();
        let host = Host::open(cfg).unwrap();
        let task = agenthost_host::start(host.clone()).await.unwrap();
        TestHost {
            dir,
            host,
            task,
            workspace,
        }
    }

    /// Stop serving and stop all actors, keeping the data directory.
    pub async fn stop(self) -> (tempfile::TempDir, HostConfig) {
        self.host.sessions.shutdown();
        if let Some(d) = self.host.devices.get() {
            d.shutdown();
        }
        self.task.abort();
        for _ in 0..50 {
            if self.host.sessions.live_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let cfg = self.host.ctx.cfg.clone();
        let _ = std::fs::remove_file(&cfg.socket_path);
        (self.dir, cfg)
    }

    pub async fn client(&self) -> (Client, mpsc::Receiver<Notification>) {
        Client::connect(&self.host.ctx.cfg.socket_path)
            .await
            .unwrap()
    }

    pub async fn workspace_id(&self, c: &Client) -> String {
        let list: Vec<Workspace> = c
            .call(methods::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .unwrap();
        if let Some(w) = list.first() {
            return w.workspace_id.clone();
        }
        c.add_workspace(self.workspace.to_str().unwrap(), None)
            .await
            .unwrap()
            .workspace_id
    }
}

/// Wait until the session reaches `state`, or panic with its events.
pub async fn wait_state(c: &Client, sid: &str, state: SessionState, timeout: Duration) -> Session {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let s = c.get_session(sid).await.unwrap();
        if s.state == state {
            return s;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "session {sid} is {} ({:?}), wanted {state}",
                s.state, s.state_reason
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Collect events from notifications until `pred` matches one.
pub async fn collect_until(
    notes: &mut mpsc::Receiver<Notification>,
    timeout: Duration,
    mut pred: impl FnMut(&Event) -> bool,
) -> (Vec<Event>, Vec<Notification>) {
    let mut events = vec![];
    let mut all = vec![];
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let n = match tokio::time::timeout_at(deadline, notes.recv()).await {
            Ok(Some(n)) => n,
            Ok(None) => panic!("connection closed; got {events:#?}"),
            Err(_) => panic!("timed out; got {events:#?}"),
        };
        all.push(n.clone());
        if let Some(e) = as_event(&n) {
            let done = pred(&e);
            events.push(e);
            if done {
                return (events, all);
            }
        }
    }
}

pub fn turn_end(e: &Event) -> bool {
    e.kind == EventType::TurnEnd
}
