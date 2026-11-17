//! Sandbox policy and launcher.
//!
//! The host writes one policy per session and compiles it for the backend:
//! bubblewrap on Linux, Seatbelt on macOS, both through Anthropic's
//! `sandbox-runtime` (`srt`) at a pinned version. All network traffic is
//! sent to the host's own egress proxy (see [`crate::proxy`]), which decides
//! every connection; `srt` only provides the isolation and the bridge.
//!
//! A session that cannot be sandboxed does not run. There is no fallback.

use crate::agents::{all_credential_paths, AgentSpec};
use crate::config::HostConfig;
use agenthost_proto::{NetworkPreset, NetworkProfile, Prerequisite, SandboxHealth};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Registries opened by the `packages` profile.
pub const PACKAGE_HOSTS: &[&str] = &[
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "proxy.golang.org",
    "sum.golang.org",
    "storage.googleapis.com",
];

/// Hosts opened by the `github` profile.
pub const GITHUB_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
    "*.githubusercontent.com",
    "uploads.github.com",
];

/// Home-relative folders no sandbox may read.
pub const SECRET_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".azure",
    ".kube",
    ".docker",
    ".config/gcloud",
    ".config/gh",
    ".config/doctl",
    ".config/op",
    ".password-store",
    ".local/share/keyrings",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".terraform.d",
    ".vault-token",
];

/// Everything a session may read, write and reach.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    pub workspace: PathBuf,
    pub temp_dir: PathBuf,
    pub write: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
    /// Re-allowed inside a denied folder (the host's pinned tools).
    pub allow_read: Vec<PathBuf>,
    /// Hosts the egress proxy allows (the agent's plus the workspace profile).
    pub allowed_hosts: Vec<String>,
}

/// Hosts for a network profile, on top of the agent's own.
pub fn profile_hosts(profile: &NetworkProfile) -> Vec<String> {
    let mut hosts = vec![];
    for p in &profile.presets {
        let list = match p {
            NetworkPreset::Packages => PACKAGE_HOSTS,
            NetworkPreset::Github => GITHUB_HOSTS,
        };
        hosts.extend(list.iter().map(|s| s.to_string()));
    }
    hosts.extend(profile.custom_hosts.iter().cloned());
    hosts
}

/// Derive a session's policy from its workspace, agent and network profile.
pub fn policy_for(
    cfg: &HostConfig,
    agent: &AgentSpec,
    workspace: &Path,
    temp_dir: &Path,
    profile: &NetworkProfile,
) -> SandboxPolicy {
    let home = &cfg.home_dir;
    let mut write = vec![workspace.to_path_buf(), temp_dir.to_path_buf()];
    write.extend(agent.state_paths.iter().cloned());

    let mut deny_read: Vec<PathBuf> = SECRET_DIRS.iter().map(|d| home.join(d)).collect();
    // Other agents' credentials and state.
    for (kind, p) in all_credential_paths(home) {
        if kind != agent.kind && !agent.state_paths.iter().any(|s| s == &p) {
            deny_read.push(p);
        }
    }
    deny_read.extend(agent.deny_read.iter().cloned());
    // The host's own data: database, secrets, logs, the client socket, the
    // MCP gateway socket and every other session's temp folder.
    deny_read.push(cfg.data_dir.clone());
    deny_read.push(cfg.runtime_dir.clone());
    if let Some(p) = cfg.socket_path.parent() {
        deny_read.push(p.to_path_buf());
    }
    // A deny inside a writable path would be meaningless; a deny above one is
    // fine (the runtime binds the writable path back in).
    deny_read.retain(|d| !write.iter().any(|w| d.starts_with(w)));
    deny_read.sort();
    deny_read.dedup();
    // Adapters are installed under the data dir; agents must be able to run
    // them. The session's own generated files sit next to its temp folder.
    let allow_read = vec![
        cfg.tools_dir.clone(),
        temp_dir.with_extension("codex"),
        temp_dir.with_extension("cfg"),
    ];

    let mut allowed_hosts = agent.hosts.clone();
    allowed_hosts.extend(profile_hosts(profile));
    allowed_hosts.sort();
    allowed_hosts.dedup();

    SandboxPolicy {
        workspace: workspace.to_path_buf(),
        temp_dir: temp_dir.to_path_buf(),
        write,
        deny_read,
        allow_read,
        allowed_hosts,
    }
}

/// Compile a policy to an `srt` settings file pointing at our proxy ports.
pub fn compile_srt(policy: &SandboxPolicy, http_port: u16, socks_port: u16) -> Value {
    let paths = |v: &[PathBuf]| {
        v.iter()
            .map(|p| Value::String(p.display().to_string()))
            .collect::<Vec<_>>()
    };
    json!({
        "network": {
            // The egress proxy decides; srt needs a list to enable proxying at all.
            "allowedDomains": policy.allowed_hosts,
            "deniedDomains": [],
            "httpProxyPort": http_port,
            "socksProxyPort": socks_port,
            "allowLocalBinding": false,
            "allowUnixSockets": [],
            "allowAllUnixSockets": false
        },
        "filesystem": {
            "denyRead": paths(&policy.deny_read),
            "allowRead": paths(&policy.allow_read),
            "allowWrite": paths(&policy.write),
            "denyWrite": []
        },
        "enableWeakerNestedSandbox": false,
        "enableWeakerNetworkIsolation": false,
        "allowNestedUserNamespaces": false
    })
}

pub fn backend_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "seatbelt"
    } else {
        "bubblewrap"
    }
}

pub fn srt_cli(cfg: &HostConfig) -> PathBuf {
    cfg.tools_dir
        .join("node_modules/@anthropic-ai/sandbox-runtime/dist/cli.js")
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// Check what the sandbox needs on this machine.
pub fn health(cfg: &HostConfig) -> SandboxHealth {
    let mut prereqs = vec![];
    let pkg = |name: &str, arch: &str, deb: &str| {
        format!("sudo pacman -S {arch}  # or: sudo apt install {deb}  (package for {name})")
    };
    if cfg!(target_os = "macos") {
        prereqs.push(Prerequisite {
            name: "sandbox-exec".into(),
            present: Path::new("/usr/bin/sandbox-exec").exists(),
            install: Some("sandbox-exec ships with macOS".into()),
        });
    } else {
        for (bin, arch, deb) in [
            ("bwrap", "bubblewrap", "bubblewrap"),
            ("socat", "socat", "socat"),
            ("rg", "ripgrep", "ripgrep"),
        ] {
            prereqs.push(Prerequisite {
                name: bin.into(),
                present: which(bin).is_some(),
                install: Some(pkg(bin, arch, deb)),
            });
        }
    }
    prereqs.push(Prerequisite {
        name: "node".into(),
        present: which("node").is_some(),
        install: Some("install Node.js 22.12 or newer".into()),
    });
    let srt = srt_cli(cfg);
    prereqs.push(Prerequisite {
        name: "sandbox-runtime".into(),
        present: srt.is_file(),
        install: Some("agenthostd install-tools".into()),
    });
    let ok = prereqs.iter().all(|p| p.present);
    let problem = if ok {
        None
    } else {
        Some(format!(
            "missing: {}",
            prereqs
                .iter()
                .filter(|p| !p.present)
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
    SandboxHealth {
        backend: backend_name().into(),
        ok,
        prerequisites: prereqs,
        runtime_version: srt.is_file().then(|| {
            crate::agents::SANDBOX_RUNTIME_PACKAGE
                .rsplit('@')
                .next()
                .unwrap()
                .to_string()
        }),
        problem,
    }
}

/// Environment for the agent: a clean slate, never the host's own environment.
pub fn agent_env(cfg: &HostConfig, agent: &AgentSpec, temp_dir: &Path) -> Vec<(String, String)> {
    let mut env = vec![];
    for k in [
        "PATH", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "SHELL", "TZ",
    ] {
        if let Ok(v) = std::env::var(k) {
            env.push((k.to_string(), v));
        }
    }
    env.push(("HOME".into(), cfg.home_dir.display().to_string()));
    env.push(("TMPDIR".into(), temp_dir.display().to_string()));
    // srt reads this to set TMPDIR inside the sandbox.
    env.push(("CLAUDE_CODE_TMPDIR".into(), temp_dir.display().to_string()));
    env.push(("CLAUDE_TMPDIR".into(), temp_dir.display().to_string()));
    env.push(("NO_COLOR".into(), "1".into()));
    env.extend(agent.env.iter().cloned());
    env.extend(cfg.agent_env_overrides.iter().cloned());
    env
}

pub struct Launch {
    pub child: Child,
    pub settings_path: Option<PathBuf>,
}

/// Start the agent inside the sandbox.
pub fn launch_sandboxed(
    cfg: &HostConfig,
    agent: &AgentSpec,
    policy: &SandboxPolicy,
    http_port: u16,
    socks_port: u16,
) -> Result<Launch> {
    let h = health(cfg);
    if !h.ok {
        bail!("sandbox unavailable: {}", h.problem.unwrap_or_default());
    }
    if !agent.program.is_file() {
        bail!("{} not found", agent.program.display());
    }
    for p in &policy.write {
        // The workspace and state paths must exist to be bound in.
        if !p.exists() && p.extension().is_none() {
            std::fs::create_dir_all(p).ok();
        }
    }
    let settings = compile_srt(policy, http_port, socks_port);
    let settings_path = policy
        .temp_dir
        .parent()
        .unwrap_or(&policy.temp_dir)
        .join(format!(
            "{}.srt.json",
            policy
                .temp_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("session")
        ));
    std::fs::write(&settings_path, serde_json::to_vec_pretty(&settings)?)
        .context("write sandbox settings")?;

    let node = which("node").context("node not found")?;
    let mut cmd = Command::new(node);
    cmd.arg(srt_cli(cfg))
        .arg("--settings")
        .arg(&settings_path)
        .arg("--")
        .arg(&agent.program)
        .args(&agent.args);
    cmd.env_clear();
    for (k, v) in agent_env(cfg, agent, &policy.temp_dir) {
        cmd.env(k, v);
    }
    cmd.current_dir(&policy.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        cmd.pre_exec(|| {
            // Own process group, so ending a session kills the whole tree.
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().context("start sandbox")?;
    Ok(Launch {
        child,
        settings_path: Some(settings_path),
    })
}

/// Test only, and only for the fake agent: start it without a sandbox,
/// still pointing its HTTP traffic at the egress proxy.
pub fn launch_unsandboxed_fake(
    cfg: &HostConfig,
    agent: &AgentSpec,
    policy: &SandboxPolicy,
    http_port: u16,
) -> Result<Launch> {
    assert_eq!(
        agent.kind,
        agenthost_proto::AgentKind::Fake,
        "only the fake agent may run unsandboxed"
    );
    let mut cmd = Command::new(&agent.program);
    cmd.args(&agent.args).env_clear();
    for (k, v) in agent_env(cfg, agent, &policy.temp_dir) {
        cmd.env(k, v);
    }
    let proxy = format!("http://127.0.0.1:{http_port}");
    cmd.env("HTTP_PROXY", &proxy)
        .env("HTTPS_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env("https_proxy", &proxy);
    cmd.current_dir(&policy.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    Ok(Launch {
        child: cmd.spawn().context("start fake agent")?,
        settings_path: None,
    })
}

/// SIGTERM the launched process only (the sandbox runtime forwards it).
pub fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
}

/// SIGTERM the whole process group, then SIGKILL the leader.
pub fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    let _ = child.start_kill();
}

/// SIGKILL anything left in the group (the leader is already gone).
pub fn kill_group(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// Names the sandbox runtime protects in a workspace (its DANGEROUS_FILES
/// and directories). When one is missing it creates an empty read-only
/// placeholder to mount over, and removes it on a clean exit.
pub const PROTECTED_NAMES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
    ".vscode",
    ".idea",
];

/// Safety net for placeholders left behind when the runtime could not clean
/// up (it was killed). Removes only empty, read-only regular files with a
/// protected name, created after `since`; never anything with content.
pub fn sweep_mount_points(workspace: &Path, since: std::time::SystemTime) {
    use std::os::unix::fs::PermissionsExt;
    let is_placeholder = |p: &Path| -> bool {
        match std::fs::symlink_metadata(p) {
            Ok(m) => {
                m.file_type().is_file()
                    && m.len() == 0
                    && m.permissions().mode() & 0o222 == 0
                    && m.modified().map(|t| t >= since).unwrap_or(false)
            }
            Err(_) => false,
        }
    };
    let mut removed = vec![];
    for name in PROTECTED_NAMES {
        let p = workspace.join(name);
        if is_placeholder(&p) && std::fs::remove_file(&p).is_ok() {
            removed.push(p);
        }
    }
    let claude = workspace.join(".claude");
    for sub in ["commands", "agents"] {
        let p = claude.join(sub);
        if is_placeholder(&p) && std::fs::remove_file(&p).is_ok() {
            removed.push(p);
        }
    }
    if let Ok(meta) = std::fs::symlink_metadata(&claude) {
        let fresh = meta.modified().map(|t| t >= since).unwrap_or(false);
        if meta.is_dir()
            && fresh
            && std::fs::read_dir(&claude)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(&claude);
        }
    }
    if !removed.is_empty() {
        tracing::info!(count = removed.len(), workspace = %workspace.display(), "removed leftover sandbox placeholders");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenthost_proto::AgentKind;

    #[test]
    fn policy_denies_secrets_and_other_agents() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let spec = crate::agents::spec(AgentKind::Codex, &cfg);
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &NetworkProfile::default(),
        );
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.ssh")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.claude")));
        assert!(p
            .deny_read
            .contains(&PathBuf::from("/home/u/.local/share/opencode")));
        assert!(!p.deny_read.contains(&PathBuf::from("/home/u/.codex")));
        assert!(p.deny_read.contains(&cfg.data_dir));
        assert!(p.deny_read.contains(&cfg.runtime_dir));
        assert!(p.allow_read.contains(&cfg.tools_dir));
        assert!(p.write.contains(&PathBuf::from("/home/u/.codex")));
        assert_eq!(p.allowed_hosts, vec!["chatgpt.com".to_string()]);

        let prof = NetworkProfile {
            presets: vec![NetworkPreset::Packages],
            custom_hosts: vec!["example.org".into()],
        };
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &prof,
        );
        assert!(p.allowed_hosts.contains(&"registry.npmjs.org".to_string()));
        assert!(p.allowed_hosts.contains(&"example.org".to_string()));
        let s = compile_srt(&p, 1, 2);
        assert_eq!(s["network"]["httpProxyPort"], 1);
        assert_eq!(s["network"]["socksProxyPort"], 2);
        // The new agents' logins are hidden from the others.
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &NetworkProfile::default(),
        );
        assert!(p
            .deny_read
            .contains(&PathBuf::from("/home/u/.config/cursor")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.gemini")));
    }

    #[test]
    fn cursor_sees_its_login_but_not_your_cursor_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let spec = crate::agents::spec(AgentKind::Cursor, &cfg);
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &NetworkProfile::default(),
        );
        // ~/.cursor holds your MCP servers, hooks and plugins.
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.cursor")));
        assert!(p.write.contains(&PathBuf::from("/home/u/.config/cursor")));
        assert!(!p
            .deny_read
            .contains(&PathBuf::from("/home/u/.config/cursor")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.codex")));
        assert!(spec.direct_gateway);
        assert_eq!(spec.auth_method, Some("cursor_login"));

        let spec = crate::agents::spec(AgentKind::Gemini, &cfg);
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &NetworkProfile::default(),
        );
        assert!(p.write.contains(&PathBuf::from("/home/u/.gemini")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.cursor")));
        let settings: serde_json::Value =
            serde_json::from_str(crate::agents::GEMINI_SYSTEM_SETTINGS).unwrap();
        assert_eq!(settings["mcp"]["allowed"], serde_json::json!(["agenthost"]));
        assert_eq!(settings["hooksConfig"]["enabled"], false);
    }
}
