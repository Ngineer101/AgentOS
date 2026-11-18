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
}

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
            fake_agent_unsandboxed: false,
            home_dir: home,
            relay_url: None,
            file_secrets: false,
            agent_env_overrides: vec![],
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
            fake_agent_unsandboxed: false,
            home_dir: home_dir().unwrap_or_else(|_| root.to_path_buf()),
            relay_url: None,
            file_secrets: true,
            agent_env_overrides: vec![],
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("host.db")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Quick tasks' folders (one per session), next to the data folder rather
    /// than inside it, which sandboxes may not read.
    pub fn scratch_dir(&self) -> PathBuf {
        let name = self
            .data_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "agenthost".into());
        self.data_dir.with_file_name(format!("{name}-scratch"))
    }

    /// Create directories with owner-only permissions and validate path lengths.
    pub fn prepare(&self) -> Result<()> {
        for d in [&self.data_dir, &self.runtime_dir, &self.logs_dir(), &self.scratch_dir()] {
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
