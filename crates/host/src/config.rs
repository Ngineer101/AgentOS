//! Host configuration and filesystem layout.

use anyhow::{bail, Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Unix socket paths are capped near 107 bytes (`sun_path`); stay well below.
pub const MAX_SOCKET_PATH: usize = 100;

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// SQLite database, logs, installed adapters. Owner-only.
    pub data_dir: PathBuf,
    /// The client API socket. Owner-only.
    pub socket_path: PathBuf,
    /// Short-path root for per-session temp folders and sandbox sockets.
    pub runtime_dir: PathBuf,
    /// Where pinned agent adapters and the sandbox runtime are installed.
    pub tools_dir: PathBuf,
    /// Offer the scripted fake agent (tests and demos).
    pub enable_fake_agent: bool,
    /// Path to the `agenthost-fake-agent` binary.
    pub fake_agent_path: Option<PathBuf>,
    /// Test only: look for package managers here instead of on PATH.
    pub install_bin_dir: Option<PathBuf>,
    /// Test only: run the fake agent without a sandbox. Never applies to real agents.
    pub fake_agent_unsandboxed: bool,
    /// Override the agent's home directory (tests). Real use leaves this unset.
    pub home_dir: PathBuf,
    /// Relay WebSocket URL, e.g. `wss://relay.example.workers.dev`.
    pub relay_url: Option<String>,
    /// Use an owner-only file for secrets instead of probing the OS keychain.
    pub file_secrets: bool,
    /// Test only: extra environment for agent processes.
    pub agent_env_overrides: Vec<(String, String)>,
    /// The integrations.sh catalog `connector.search` asks
    /// (`<base>/api/search?q=…`); `None` turns search off (tests).
    pub integrations_url: Option<String>,
    /// Where connector icons come from (`<base>?domain=…&sz=64`); `None`
    /// fetches none (tests).
    pub favicon_url: Option<String>,
    /// screen-relay's CLI; `None` looks for `screen-relay` on PATH.
    pub screen_relay_bin: Option<PathBuf>,
    /// A screen-relay engine you started yourself (`http://127.0.0.1:8080`).
    /// The host then pipes to it and never starts or stops one.
    pub screen_engine_url: Option<String>,
    /// The token file of that engine, when it has one (it listens beyond
    /// loopback, as in a container).
    pub screen_engine_token_file: Option<PathBuf>,
    /// Test only: how long an engine waits for a viewer before it is stopped.
    pub screen_idle_stop: Option<std::time::Duration>,
    /// Test only: how long one viewer may watch (15 minutes otherwise).
    pub screen_max_view: Option<std::time::Duration>,
}

/// The catalog `connector.search` uses.
pub const INTEGRATIONS_URL: &str = "https://integrations.sh";
/// Google's favicon service, for connector icons.
pub const FAVICON_URL: &str = "https://www.google.com/s2/favicons";

impl HostConfig {
    /// Defaults for the current user on this OS.
    pub fn default_for_user() -> Result<Self> {
        let home = home_dir()?;
        let data_dir = default_data_dir(&home);
        let runtime_dir = default_runtime_dir();
        let socket_path = if cfg!(target_os = "macos") {
            data_dir.join("host.sock")
        } else {
            runtime_dir.join("host.sock")
        };
        Ok(Self {
            tools_dir: data_dir.join("tools"),
            data_dir,
            socket_path,
            runtime_dir,
            enable_fake_agent: false,
            fake_agent_path: None,
            install_bin_dir: None,
            fake_agent_unsandboxed: false,
            home_dir: home,
            relay_url: None,
            file_secrets: false,
            agent_env_overrides: vec![],
            integrations_url: Some(INTEGRATIONS_URL.into()),
            favicon_url: Some(FAVICON_URL.into()),
            screen_relay_bin: None,
            screen_engine_url: None,
            screen_engine_token_file: None,
            screen_idle_stop: None,
            screen_max_view: None,
        })
    }

    /// A self-contained layout under one directory (tests, portable installs).
    pub fn in_dir(root: &Path) -> Self {
        let data_dir = root.join("data");
        Self {
            tools_dir: data_dir.join("tools"),
            socket_path: root.join("h.sock"),
            runtime_dir: root.join("rt"),
            data_dir,
            enable_fake_agent: false,
            fake_agent_path: None,
            install_bin_dir: None,
            fake_agent_unsandboxed: false,
            home_dir: home_dir().unwrap_or_else(|_| root.to_path_buf()),
            relay_url: None,
            file_secrets: true,
            agent_env_overrides: vec![],
            // Off: tests never reach the network unless they ask.
            integrations_url: None,
            favicon_url: None,
            screen_relay_bin: None,
            screen_engine_url: None,
            screen_engine_token_file: None,
            screen_idle_stop: None,
            screen_max_view: None,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("host.db")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// A folder next to the data folder (not inside it, which sandboxes may not read).
    fn sibling(&self, suffix: &str) -> PathBuf {
        let name = self
            .data_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "agenthost".into());
        self.data_dir.with_file_name(format!("{name}-{suffix}"))
    }

    /// Quick tasks' folders, one per session.
    pub fn scratch_dir(&self) -> PathBuf {
        self.sibling("scratch")
    }

    /// Tools agents install for themselves (npm -g, pipx, cargo install, …)
    /// and package caches: writable in every session, shared between them,
    /// and separate from your own home folder.
    pub fn session_tools_dir(&self) -> PathBuf {
        self.sibling("session-tools")
    }

    /// Git worktrees, one per session in a git workspace: each session works
    /// on a branch of its own, in a folder only it can see.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.sibling("worktrees")
    }

    /// Create directories with owner-only permissions and validate path lengths.
    pub fn prepare(&self) -> Result<()> {
        for d in [
            &self.data_dir,
            &self.runtime_dir,
            &self.logs_dir(),
            &self.scratch_dir(),
            &self.session_tools_dir(),
            &self.worktrees_dir(),
        ] {
            create_private_dir(d)?;
        }
        if let Some(parent) = self.socket_path.parent() {
            create_private_dir(parent)?;
        }
        let len = self.socket_path.as_os_str().len();
        if len > MAX_SOCKET_PATH {
            bail!(
                "socket path {} is {len} bytes; Unix sockets need under {MAX_SOCKET_PATH}",
                self.socket_path.display()
            );
        }
        Ok(())
    }
}

pub fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod {}", dir.display()))?;
    Ok(())
}

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn default_data_dir(home: &Path) -> PathBuf {
    if let Some(d) = std::env::var_os("AGENTHOST_DATA_DIR") {
        return PathBuf::from(d);
    }
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/agenthost")
    } else if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(x).join("agenthost")
    } else {
        home.join(".local/share/agenthost")
    }
}

fn default_runtime_dir() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    if !cfg!(target_os = "macos") {
        if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR") {
            let p = PathBuf::from(x).join("agenthost");
            if p.as_os_str().len() < 40 {
                return p;
            }
        }
    }
    PathBuf::from(format!("/tmp/agenthost-{uid}"))
}

/// The default socket path a client should dial.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENTHOST_SOCKET") {
        return PathBuf::from(p);
    }
    HostConfig::default_for_user()
        .map(|c| c.socket_path)
        .unwrap_or_else(|_| PathBuf::from("/tmp/agenthost.sock"))
}
