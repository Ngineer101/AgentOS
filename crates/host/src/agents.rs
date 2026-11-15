//! Per-agent configuration. All agents are driven the same way (ACP over
//! stdio); what differs is here: where the binary is, what it may read,
//! write and reach, how to tell it is logged in, and how to fix it.

use crate::config::HostConfig;
use agenthost_proto::{AgentKind, AgentStatus};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Pinned adapter versions the host installs into its own tools directory.
pub const CODEX_ACP_PACKAGE: &str = "@agentclientprotocol/codex-acp@2.6.0";
pub const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.107.0";
pub const SANDBOX_RUNTIME_PACKAGE: &str = "@anthropic-ai/sandbox-runtime@0.0.83";
/// Oldest OpenCode with a working `opencode acp`.
pub const OPENCODE_MIN_MAJOR: u32 = 2;

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub kind: AgentKind,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Hosts the agent needs for a turn (its model provider).
    pub hosts: Vec<String>,
    /// The agent's own state paths: readable and writable inside its sandbox.
    pub state_paths: Vec<PathBuf>,
    /// Extra read-only paths (the adapter install, the CLI's install dir).
    pub read_paths: Vec<PathBuf>,
    /// Files whose presence means "logged in".
    pub credential_paths: Vec<PathBuf>,
    /// Command that logs the CLI in.
    pub login_fix: String,
    /// Command that installs the agent.
    pub install_fix: String,
    /// ACP session mode to switch to after `session/new` / `session/load`,
    /// when the agent offers it (turns the agent's own sandbox and prompts off).
    pub session_mode: Option<String>,
    /// `_meta` sent with `session/new` and `session/load`.
    pub session_meta: Option<serde_json::Value>,
    /// Extra paths this agent's own sandbox may not read (its personal config).
    pub deny_read: Vec<PathBuf>,
    /// Write a host-controlled config folder and point `XDG_CONFIG_HOME` at it.
    pub host_xdg_config: Option<(&'static str, &'static str)>,
}

/// Every real agent's credential and state paths, so each sandbox can deny
/// the other agents' credentials.
pub fn all_credential_paths(home: &Path) -> Vec<(AgentKind, PathBuf)> {
    vec![
        (AgentKind::Codex, home.join(".codex")),
        (AgentKind::Claude, home.join(".claude")),
        (AgentKind::Claude, home.join(".claude.json")),
        (AgentKind::Opencode, home.join(".local/share/opencode")),
        (AgentKind::Opencode, home.join(".config/opencode")),
    ]
}

fn find_in_path(name: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

static VERSION_CACHE: Mutex<Option<HashMap<PathBuf, Option<String>>>> = Mutex::new(None);

/// `<program> --version`, cached per path.
pub fn program_version(program: &Path) -> Option<String> {
    let mut cache = VERSION_CACHE.lock().unwrap();
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(v) = map.get(program) {
        return v.clone();
    }
    let v = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    map.insert(program.to_path_buf(), v.clone());
    v
}

fn major(version: &str) -> Option<u32> {
    version
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| {
            s.chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        })
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
}

/// Resolve the OpenCode binary: `AGENTHOST_OPENCODE_BIN`, else the first
/// `opencode` on PATH that is new enough to have `opencode acp`.
fn find_opencode() -> (Option<PathBuf>, Option<String>) {
    if let Some(p) = std::env::var_os("AGENTHOST_OPENCODE_BIN") {
        let p = PathBuf::from(p);
        let v = program_version(&p);
        return (Some(p), v);
    }
    let candidates = find_in_path("opencode");
    let mut first = None;
    for c in &candidates {
        let v = program_version(c);
        if first.is_none() {
            first = Some((c.clone(), v.clone()));
        }
        if v.as_deref()
            .and_then(major)
            .map(|m| m >= OPENCODE_MIN_MAJOR)
            .unwrap_or(false)
        {
            return (Some(c.clone()), v);
        }
    }
    match first {
        Some((p, v)) => (Some(p), v),
        None => (None, None),
    }
}

/// The directory holding a binary, with symlinks resolved, so the sandbox can read it.
fn install_root(program: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Ok(real) = std::fs::canonicalize(program) {
        // e.g. .../lib/node_modules/pkg/bin/x -> allow the whole package tree.
        let mut p = real.as_path();
        for _ in 0..4 {
            if let Some(parent) = p.parent() {
                p = parent;
            }
        }
        v.push(p.to_path_buf());
    }
    v
}

/// App-server overrides for every Codex session: no notify command, and the
/// features that load your hooks, plugins, ChatGPT apps (connectors that
/// would bypass the gateway) or drive a browser or the desktop turned off.
pub const CODEX_OVERRIDES: &[&str] = &[
    "notify=[]",
    "features.hooks=false",
    "features.plugins=false",
    "features.remote_plugin=false",
    "features.plugin_discovery_prefetch=false",
    "features.plugin_sharing=false",
    "features.apps=false",
    "features.browser_use=false",
    "features.browser_use_external=false",
    "features.in_app_browser=false",
    "features.computer_use=false",
    "features.skill_mcp_dependency_install=false",
    "features.daemon_auto_start=false",
    "features.mcp_oauth_refresh_coordination=false",
];

/// Names of MCP servers configured in a Codex config file.
fn codex_mcp_names(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let Ok(v) = text.parse::<toml::Table>() else {
        return vec![];
    };
    v.get("mcp_servers")
        .and_then(|t| t.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// Codex merges `-c` overrides into your config rather than replacing it, so
/// every MCP server you configured (globally or in the workspace) is turned
/// off by name. Only the names are read; nothing else from the file is used.
pub fn codex_overrides(cfg: &HostConfig, workspace: &Path) -> Vec<String> {
    let mut out: Vec<String> = CODEX_OVERRIDES.iter().map(|s| s.to_string()).collect();
    let mut names = codex_mcp_names(&cfg.home_dir.join(".codex/config.toml"));
    names.extend(codex_mcp_names(&workspace.join(".codex/config.toml")));
    names.sort();
    names.dedup();
    for n in names {
        if n == "agenthost" {
            continue;
        }
        let key = if n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            n
        } else {
            format!("\"{}\"", n.replace('"', ""))
        };
        out.push(format!("mcp_servers.{key}.enabled=false"));
    }
    out
}

/// The same overrides as a codex-acp thread config map (dotted keys, TOML values parsed).
pub fn codex_thread_config(cfg: &HostConfig, workspace: &Path) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for o in codex_overrides(cfg, workspace) {
        let Some((k, v)) = o.split_once('=') else {
            continue;
        };
        let value = match v {
            "true" => serde_json::Value::Bool(true),
            "false" => serde_json::Value::Bool(false),
            "[]" => serde_json::json!([]),
            "{}" => serde_json::json!({}),
            other => serde_json::Value::String(other.trim_matches('"').to_string()),
        };
        map.insert(k.replace('"', ""), value);
    }
    serde_json::Value::Object(map)
}

/// Write the per-session wrapper codex-acp runs as its Codex binary. It
/// lives next to the session's temp folder, where the sandbox can read but
/// not write it.
pub fn write_codex_wrapper(cfg: &HostConfig, path: &Path, workspace: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let codex_js = cfg
        .tools_dir
        .join("node_modules/@openai/codex/bin/codex.js");
    let node = find_in_path("node")
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("node"));
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let overrides: Vec<String> = codex_overrides(cfg, workspace)
        .iter()
        .map(|o| format!("-c {}", q(o)))
        .collect();
    let script = format!(
        "#!/bin/sh\n# Generated by agenthost: runs the pinned Codex with host-controlled config.\nexec {} {} \"$@\" {}\n",
        q(&node.display().to_string()),
        q(&codex_js.display().to_string()),
        overrides.join(" ")
    );
    std::fs::write(path, &script)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

pub fn spec(kind: AgentKind, cfg: &HostConfig) -> AgentSpec {
    let home = &cfg.home_dir;
    let bin = cfg.tools_dir.join("node_modules/.bin");
    match kind {
        AgentKind::Codex => AgentSpec {
            kind,
            program: bin.join("codex-acp"),
            args: vec![],
            // Host-controlled configuration: codex-acp starts `codex app-server`
            // through CODEX_PATH; our wrapper adds overrides that drop your
            // personal MCP servers, hooks and notify commands.
            // CODEX_PATH is set per session (see `write_codex_wrapper`).
            env: vec![],
            hosts: vec!["chatgpt.com".into()],
            state_paths: vec![home.join(".codex")],
            read_paths: vec![cfg.tools_dir.clone()],
            credential_paths: vec![home.join(".codex/auth.json")],
            login_fix: "codex login".into(),
            install_fix: "agenthostd install-tools".into(),
            // Full access inside our sandbox, never ask: ours is the boundary.
            session_mode: Some("agent-full-access".into()),
            session_meta: None,
            deny_read: vec![],
            host_xdg_config: None,
        },
        AgentKind::Claude => AgentSpec {
            kind,
            program: bin.join("claude-agent-acp"),
            args: vec![],
            // Host-controlled configuration: no claude.ai connectors (they would
            // bypass the gateway), no marketplace installs, no updates or telemetry.
            env: vec![
                ("DISABLE_AUTOUPDATER".into(), "1".into()),
                (
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                    "1".into(),
                ),
                ("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into()),
                (
                    "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL".into(),
                    "1".into(),
                ),
                ("DISABLE_TELEMETRY".into(), "1".into()),
                ("DISABLE_ERROR_REPORTING".into(), "1".into()),
            ],
            hosts: vec![
                "api.anthropic.com".into(),
                "console.anthropic.com".into(),
                "claude.ai".into(),
                "platform.claude.com".into(),
            ],
            state_paths: vec![home.join(".claude"), home.join(".claude.json")],
            read_paths: vec![cfg.tools_dir.clone()],
            credential_paths: if cfg!(target_os = "macos") {
                // On macOS the login lives in the Keychain; the state file is the best signal.
                vec![home.join(".claude.json")]
            } else {
                vec![home.join(".claude/.credentials.json")]
            },
            login_fix: "claude auth login".into(),
            install_fix: "agenthostd install-tools".into(),
            session_mode: None,
            // No user/project/local settings (so no personal hooks, plugins or
            // MCP servers), and only the MCP servers the host passes.
            session_meta: Some(serde_json::json!({
                "claudeCode": {"options": {"settingSources": [], "strictMcpConfig": true, "plugins": []}}
            })),
            deny_read: vec![],
            host_xdg_config: None,
        },
        AgentKind::Opencode => {
            let (program, _) = find_opencode();
            let program = program.unwrap_or_else(|| PathBuf::from("opencode"));
            let read_paths = install_root(&program);
            AgentSpec {
                kind,
                program,
                args: vec!["acp".into()],
                env: vec![
                    ("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into()),
                    ("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "1".into()),
                    ("OPENCODE_DISABLE_CLAUDE_CODE".into(), "1".into()),
                ],
                hosts: vec!["opencode.ai".into(), "*.opencode.ai".into(), "models.dev".into()],
                // Login and history live in the data dir; your personal config
                // (plugins, MCP servers) in ~/.config/opencode is hidden and a
                // host-written config is used instead.
                state_paths: vec![
                    home.join(".local/share/opencode"),
                    home.join(".cache/opencode"),
                    home.join(".local/state/opencode"),
                ],
                read_paths,
                credential_paths: vec![home.join(".local/share/opencode/auth.json")],
                login_fix: "opencode auth login".into(),
                install_fix: "install OpenCode 2.3 or newer (https://opencode.ai) or set AGENTHOST_OPENCODE_BIN".into(),
                session_mode: None,
                session_meta: None,
                deny_read: vec![home.join(".config/opencode")],
                host_xdg_config: Some(("opencode/opencode.json", r#"{"$schema":"https://opencode.ai/config.json","autoupdate":false,"share":"disabled","mcp":{},"plugin":[]}"#)),
            }
        }
        AgentKind::Fake => {
            let program = cfg.fake_agent_path.clone().unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("agenthost-fake-agent")))
                    .unwrap_or_else(|| PathBuf::from("agenthost-fake-agent"))
            });
            let state = home.join(".fake-agent");
            AgentSpec {
                kind,
                read_paths: install_root(&program),
                program,
                args: vec![],
                env: vec![("FAKE_AGENT_HOME".into(), state.display().to_string())],
                hosts: vec![],
                state_paths: vec![state],
                credential_paths: vec![],
                login_fix: "fake-agent login".into(),
                install_fix: "cargo build -p agenthost-fake-agent".into(),
                session_mode: None,
                session_meta: None,
                deny_read: vec![],
                host_xdg_config: None,
            }
        }
    }
}

/// Whether the agent is installed and logged in, with the fix if not.
pub fn status(kind: AgentKind, cfg: &HostConfig) -> AgentStatus {
    let s = spec(kind, cfg);
    let installed = s.program.is_file();
    let version = match kind {
        AgentKind::Opencode | AgentKind::Fake if installed => program_version(&s.program),
        AgentKind::Codex if installed => {
            Some(CODEX_ACP_PACKAGE.rsplit('@').next().unwrap().to_string())
        }
        AgentKind::Claude if installed => {
            Some(CLAUDE_ACP_PACKAGE.rsplit('@').next().unwrap().to_string())
        }
        _ => None,
    };
    let mut problem = None;
    let mut fix = None;
    if !installed {
        problem = Some(format!(
            "{} is not installed ({} not found)",
            kind.display_name(),
            s.program.display()
        ));
        fix = Some(s.install_fix.clone());
    } else if kind == AgentKind::Opencode {
        if let Some(m) = version.as_deref().and_then(major) {
            if m < OPENCODE_MIN_MAJOR {
                problem = Some(format!(
                    "OpenCode {} at {} is too old; 2.3 or newer is needed",
                    version.clone().unwrap_or_default(),
                    s.program.display()
                ));
                fix = Some(s.install_fix.clone());
            }
        }
    }
    let logged_in = s.credential_paths.iter().all(|p| p.exists());
    if problem.is_none() && !logged_in {
        problem = Some("not logged in".into());
        fix = Some(s.login_fix.clone());
    }
    AgentStatus {
        agent: kind,
        display_name: kind.display_name().into(),
        installed,
        version,
        logged_in,
        available: problem.is_none(),
        problem,
        fix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major() {
        assert_eq!(major("1.2.22"), Some(1));
        assert_eq!(major("opencode 2.3.0"), Some(2));
        assert_eq!(major("codex-cli 0.169.0"), Some(0));
        assert_eq!(major("none"), None);
    }
}
