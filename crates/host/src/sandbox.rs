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

/// Home-relative paths no sandbox may read.
///
/// Your home folder is otherwise readable: an agent needs to see your tools'
/// configuration to work in your repositories, and listing everything it may
/// read is not something anyone could keep correct. So this is a list of what
/// it may not, and it has to cover everything a session could sell or sign in
/// with — not just the obvious credential folders but the places a logged-in
/// browser, a chat app or a shell keeps the same power: a cookie store is a
/// live session, and shell history is full of tokens people pasted once.
///
/// Entries that don't exist (another platform's, a tool you don't use) are
/// harmless. macOS paths are listed alongside the Linux ones.
///
/// If you add one, add it here rather than anywhere else: this is the list
/// `docs/security.md` describes and `tests/m3_escape.rs` checks.
pub const SECRET_DIRS: &[&str] = &[
    // Keys, clouds and credential helpers.
    ".ssh",
    ".aws",
    ".gnupg",
    ".azure",
    ".kube",
    ".docker",
    ".oci",
    ".config/gcloud",
    ".config/gh",
    ".config/glab-cli",
    ".config/hub",
    ".config/doctl",
    ".config/op",
    ".config/flyctl",
    ".config/heroku",
    ".config/netlify",
    ".config/supabase",
    ".config/wrangler",
    ".config/railway",
    ".config/pulumi",
    ".config/sops",
    ".config/rclone",
    ".config/restic",
    ".config/keybase",
    ".vercel",
    ".cloudflared",
    ".pulumi",
    ".scaleway",
    ".aliyun",
    ".password-store",
    ".local/share/keyrings",
    ".gnome2/keyrings",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".gem/credentials",
    ".composer/auth.json",
    ".m2/settings.xml",
    ".gradle/gradle.properties",
    ".terraform.d",
    ".vault-token",
    ".subversion/auth",
    // Tokens that command-line tools park in a shared store.
    ".config/configstore",
    // Databases and file shares.
    ".pgpass",
    ".my.cnf",
    ".mylogin.cnf",
    ".smbcredentials",
    ".dbeaver",
    ".dbeaver4",
    // Shell and tool history: tokens, connection strings, whole commands.
    ".bash_history",
    ".zsh_history",
    ".sh_history",
    ".local/share/fish/fish_history",
    ".python_history",
    ".node_repl_history",
    ".psql_history",
    ".mysql_history",
    ".sqlite_history",
    ".irb_history",
    ".Renviron",
    ".ipython",
    // Browser profiles: cookies are live logins, and the key that decrypts
    // the saved ones sits next to them.
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".config/microsoft-edge",
    ".config/BraveSoftware",
    ".config/vivaldi",
    ".config/opera",
    ".config/epiphany",
    ".local/share/qutebrowser",
    ".var/app/org.mozilla.firefox",
    ".var/app/com.google.Chrome",
    ".var/app/com.brave.Browser",
    "snap/firefox",
    "snap/chromium",
    // Chat and mail apps: the token is the account.
    ".config/discord",
    ".config/discordcanary",
    ".config/Slack",
    ".config/Signal",
    ".config/Element",
    ".config/teams",
    ".config/Mattermost",
    ".local/share/TelegramDesktop",
    ".thunderbird",
    ".purple",
    // macOS: the keychain, and the same browsers, chat apps and mail.
    "Library/Keychains",
    "Library/Cookies",
    "Library/Safari",
    "Library/Messages",
    "Library/Mail",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Application Support/Firefox",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Chromium",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Slack",
    "Library/Application Support/discord",
    "Library/Application Support/Signal",
    "Library/Application Support/Telegram Desktop",
];

/// Everything a session may read, write and reach.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    pub workspace: PathBuf,
    pub temp_dir: PathBuf,
    pub write: Vec<PathBuf>,
    /// Not writable although inside a writable path.
    pub deny_write: Vec<PathBuf>,
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
            NetworkPreset::Web => &[crate::proxy::ANY_PUBLIC_HOST],
        };
        hosts.extend(list.iter().map(|s| s.to_string()));
    }
    hosts.extend(profile.custom_hosts.iter().cloned());
    hosts
}

fn reallow_writable(write: &[PathBuf], deny_read: &[PathBuf], allow_read: &mut Vec<PathBuf>) {
    // Spelled either way: `/tmp/…` and `/private/tmp/…` on macOS.
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    for w in write {
        let under = deny_read
            .iter()
            .any(|d| w.starts_with(d) || real(w).starts_with(real(d)));
        if under && !allow_read.contains(w) {
            allow_read.push(w.clone());
        }
    }
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
    let mut write = vec![
        workspace.to_path_buf(),
        temp_dir.to_path_buf(),
        cfg.session_tools_dir(),
    ];
    write.extend(agent.state_paths.iter().cloned());

    let mut deny_read: Vec<PathBuf> = SECRET_DIRS.iter().map(|d| home.join(d)).collect();
    // Every other login's credentials and state: other agents', this agent's
    // default login when the session runs as a profile, and other profiles'.
    for (kind, p) in all_credential_paths(home) {
        let own = agent.state_paths.iter().any(|s| s == &p);
        if !own && (kind != agent.kind || agent.profile.is_some()) {
            deny_read.push(p);
        }
    }
    for p in &agent.other_logins {
        if !agent.state_paths.iter().any(|s| s == p) {
            deny_read.push(p.clone());
        }
    }
    deny_read.extend(agent.deny_read.iter().cloned());
    // The host's own data: database, secrets, logs, the client socket, the
    // MCP gateway socket and every other session's temp folder.
    deny_read.push(cfg.data_dir.clone());
    deny_read.push(cfg.runtime_dir.clone());
    // Other quick tasks' folders and other sessions' worktrees (this
    // session's own is bound back in when it is one).
    deny_read.push(cfg.scratch_dir());
    deny_read.push(cfg.worktrees_dir());
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
    let mut allow_read = vec![
        cfg.tools_dir.clone(),
        temp_dir.with_extension("codex"),
        temp_dir.with_extension("cfg"),
        // Files the user attached to a prompt: the host writes them, the
        // agent only reads them.
        crate::session::attach_dir(temp_dir),
    ];
    // A writable folder inside a denied one (a quick task's own folder in
    // the scratch folder) must be readable too: Seatbelt does not re-allow
    // reading a path because it may be written.
    reallow_writable(&write, &deny_read, &mut allow_read);

    let mut allowed_hosts = agent.hosts.clone();
    // The runtime only needs a list to turn proxying on; the egress proxy
    // decides, so "any public host" stays out of it.
    allowed_hosts.extend(
        profile_hosts(profile)
            .into_iter()
            .filter(|h| h != crate::proxy::ANY_PUBLIC_HOST),
    );
    allowed_hosts.sort();
    allowed_hosts.dedup();

    SandboxPolicy {
        workspace: workspace.to_path_buf(),
        temp_dir: temp_dir.to_path_buf(),
        write,
        deny_write: vec![],
        deny_read,
        allow_read,
        allowed_hosts,
    }
}

/// The policy of a session working in its own git worktree, with `cwd` the
/// workspace's folder inside it. The worktree is writable instead of the
/// workspace (which stays readable), and so is the repository's `.git`,
/// which committing, branching, fetching and packing refs in a worktree all
/// write to. Not the entries at its top that run something outside the
/// sandbox the next time you use git, or point your checkout elsewhere
/// ([`crate::worktree::PROTECTED_GIT_ENTRIES`]); the worktree's `.git` file
/// and the pointers in its own folder stay read-only for the same reason.
pub fn policy_for_worktree(
    cfg: &HostConfig,
    agent: &AgentSpec,
    cwd: &Path,
    temp_dir: &Path,
    profile: &NetworkProfile,
    git: &crate::worktree::GitAccess,
) -> SandboxPolicy {
    let mut p = policy_for(cfg, agent, cwd, temp_dir, profile);
    p.write.retain(|w| w != cwd);
    let common = &git.common_dir;
    let mut own = vec![git.root.clone(), common.clone()];
    if !git.admin_dir.starts_with(common) {
        own.push(git.admin_dir.clone());
    }
    own.append(&mut p.write);
    p.write = own;
    p.deny_write = vec![
        git.root.join(".git"),
        git.admin_dir.join("commondir"),
        git.admin_dir.join("gitdir"),
    ];
    // On Linux only what exists can be protected (the host made the ones
    // that matter exist); Seatbelt protects missing paths too.
    let protect = |p: &Path| cfg!(target_os = "macos") || p.exists();
    for f in crate::worktree::PROTECTED_GIT_ENTRIES {
        let path = common.join(f);
        if protect(&path) {
            p.deny_write.push(path);
        }
    }
    // Read by git only when the main config turns it on, which the agent
    // can't; denied anyway while it is there.
    for f in ["config", "config.worktree"] {
        if git.admin_dir.join(f).exists() {
            p.deny_write.push(git.admin_dir.join(f));
        }
    }
    p.deny_read.retain(|d| !p.write.iter().any(|w| d.starts_with(w)));
    reallow_writable(&p.write, &p.deny_read, &mut p.allow_read);
    p
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
            "denyWrite": paths(&policy.deny_write)
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
/// Point package managers at the shared session tools folder, so agents can
/// install what they need (and download dependencies) inside the sandbox,
/// and put what they install first on PATH. Your home folder stays read-only.
pub fn session_tools_env(cfg: &HostConfig) -> Vec<(String, String)> {
    let d = cfg.session_tools_dir();
    let p = |rel: &str| d.join(rel).display().to_string();
    let host_path = std::env::var("PATH").unwrap_or_default();
    let path = [
        p("bin"),
        p("npm/bin"),
        p("cargo/bin"),
        p("python/bin"),
    ]
    .join(":");
    vec![
        ("PATH".into(), if host_path.is_empty() { path } else { format!("{path}:{host_path}") }),
        // npm install -g, and the npm cache
        ("NPM_CONFIG_PREFIX".into(), p("npm")),
        ("npm_config_cache".into(), p("cache/npm")),
        // pip install --user, pipx, uv tool
        ("PYTHONUSERBASE".into(), p("python")),
        ("PIP_CACHE_DIR".into(), p("cache/pip")),
        ("PIPX_HOME".into(), p("pipx")),
        ("PIPX_BIN_DIR".into(), p("bin")),
        ("UV_TOOL_DIR".into(), p("uv/tools")),
        ("UV_TOOL_BIN_DIR".into(), p("bin")),
        ("UV_CACHE_DIR".into(), p("cache/uv")),
        ("UV_PYTHON_INSTALL_DIR".into(), p("uv/python")),
        // cargo install, and crate downloads for builds
        ("CARGO_HOME".into(), p("cargo")),
        // go install, modules and the build cache
        ("GOPATH".into(), p("go")),
        ("GOBIN".into(), p("bin")),
        ("GOMODCACHE".into(), p("go/pkg/mod")),
        ("GOCACHE".into(), p("cache/go-build")),
    ]
}

pub fn agent_env(cfg: &HostConfig, agent: &AgentSpec, temp_dir: &Path) -> Vec<(String, String)> {
    let mut env = vec![];
    // PATH comes from `session_tools_env` (the session tools first, then yours).
    for k in ["LANG", "LC_ALL", "TERM", "USER", "LOGNAME", "SHELL", "TZ"] {
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
    env.extend(session_tools_env(cfg));
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

/// Linux: create, in a folder several sessions share, the placeholders
/// bubblewrap would otherwise create and remove itself: empty, read-only
/// files for each missing protected name (and `.claude/commands`,
/// `.claude/agents`, and `.git/hooks` in a repository without one). A
/// sandbox that finds a placeholder already there mounts over it and leaves
/// it in place when it exits (`sandbox-runtime` removes only the mount points
/// it created), so one session ending no longer pulls the protection out
/// from under the others. Returns what it created, for [`SharedFolders`] to
/// remove when the last session in the folder ends.
pub fn create_placeholders(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut created = vec![];
    let mut file = |p: PathBuf| {
        if std::fs::symlink_metadata(&p).is_ok() {
            return;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444));
                created.push(p);
            }
            Err(e) => tracing::warn!(path = %p.display(), "could not create sandbox placeholder: {e}"),
        }
    };
    for name in PROTECTED_NAMES {
        file(dir.join(name));
    }
    let claude = dir.join(".claude");
    let made_claude = std::fs::symlink_metadata(&claude).is_err() && std::fs::create_dir(&claude).is_ok();
    if claude.is_dir() {
        file(claude.join("commands"));
        file(claude.join("agents"));
    }
    if made_claude {
        created.push(claude);
    }
    // An empty hooks folder, as `git init` makes one: kept afterwards.
    let git = dir.join(".git");
    if git.is_dir() && !git.join("hooks").exists() {
        let _ = std::fs::create_dir(git.join("hooks"));
    }
    created
}

/// Folders sessions share (workspaces outside git, or repositories without
/// a commit): how many live sandboxes use each, and the placeholders the host
/// made there (Linux), removed when the last one ends.
#[derive(Default)]
pub struct SharedFolders {
    users: std::sync::Mutex<std::collections::HashMap<PathBuf, (usize, Vec<PathBuf>)>>,
}

impl SharedFolders {
    /// A sandbox is about to start in `dir`.
    pub fn acquire(&self, dir: &Path) {
        let mut users = self.users.lock().unwrap();
        let entry = users.entry(dir.to_path_buf()).or_default();
        entry.0 += 1;
        if cfg!(target_os = "linux") {
            // Seatbelt (macOS) denies by path and needs no mount points.
            let made = create_placeholders(dir);
            entry.1.extend(made);
        }
    }

    /// A sandbox in `dir` has exited. The last one out removes the host's
    /// placeholders and anything left behind since `since`.
    pub fn release(&self, dir: &Path, since: std::time::SystemTime) {
        let mut users = self.users.lock().unwrap();
        let Some(entry) = users.get_mut(dir) else {
            return;
        };
        entry.0 = entry.0.saturating_sub(1);
        if entry.0 > 0 {
            return;
        }
        let (_, made) = users.remove(dir).unwrap_or_default();
        remove_placeholders(&made);
        sweep_mount_points(dir, since);
    }

    pub fn users(&self, dir: &Path) -> usize {
        self.users.lock().unwrap().get(dir).map(|e| e.0).unwrap_or(0)
    }
}

/// Remove placeholders the host created, if they still are placeholders.
fn remove_placeholders(made: &[PathBuf]) {
    use std::os::unix::fs::PermissionsExt;
    // Files first, then the folders that held them.
    for p in made {
        if let Ok(m) = std::fs::symlink_metadata(p) {
            if m.file_type().is_file() && m.len() == 0 && m.permissions().mode() & 0o222 == 0 {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    for p in made {
        if p.is_dir() {
            let _ = std::fs::remove_dir(p); // only when empty
        }
    }
}

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
        // H5: the home folder is readable, so the list of what isn't has to
        // cover everything that is a login in its own right — a browser's
        // cookie store, a chat app's token, the history of what you pasted
        // into a shell — not only the obvious credential folders.
        for secret in [
            ".mozilla",
            ".config/google-chrome",
            ".config/chromium",
            ".config/discord",
            ".config/Slack",
            ".local/share/TelegramDesktop",
            ".bash_history",
            ".zsh_history",
            ".pgpass",
            ".my.cnf",
            ".config/rclone",
            ".config/configstore",
            ".gem/credentials",
            ".netrc",
            "Library/Keychains",
            "Library/Cookies",
            "Library/Application Support/Firefox",
        ] {
            assert!(
                p.deny_read.contains(&PathBuf::from("/home/u").join(secret)),
                "{secret} is readable from inside the sandbox"
            );
        }
        // H2: attached files are staged beside the temp folder, where the
        // agent reads them and cannot write (so it cannot put a symlink
        // where the host is about to write).
        let att = crate::session::attach_dir(Path::new("/tmp/ah/s1"));
        assert!(p.allow_read.contains(&att), "the agent must be able to read its attachments");
        assert!(!p.write.contains(&att), "the agent must not be able to write the attachments folder");
        assert!(!att.starts_with(Path::new("/tmp/ah/s1")), "and it must not be inside the writable temp folder");
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

    fn profile(kind: AgentKind, id: &str, dir: &str) -> agenthost_proto::AgentProfile {
        agenthost_proto::AgentProfile {
            profile_id: id.into(),
            agent: kind,
            name: id.into(),
            config_dir: dir.into(),
            created_at: 0,
        }
    }

    #[test]
    fn agents_install_their_own_tools_into_the_shared_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let tools = cfg.session_tools_dir();
        // Next to the data folder, never inside it or your home folder.
        assert!(!tools.starts_with(&cfg.data_dir) && !tools.starts_with("/home/u"));
        let spec = crate::agents::spec(AgentKind::Claude, &cfg);
        let p = policy_for(&cfg, &spec, Path::new("/work/repo"), Path::new("/tmp/ah/s1"), &NetworkProfile::default());
        assert!(p.write.contains(&tools));
        assert!(!p.write.iter().any(|w| w == Path::new("/home/u")));
        let env: std::collections::HashMap<String, String> =
            agent_env(&cfg, &spec, Path::new("/tmp/ah/s1")).into_iter().collect();
        let t = |rel: &str| tools.join(rel).display().to_string();
        assert_eq!(env["NPM_CONFIG_PREFIX"], t("npm"));
        assert_eq!(env["CARGO_HOME"], t("cargo"));
        assert_eq!(env["PIPX_BIN_DIR"], t("bin"));
        assert_eq!(env["GOBIN"], t("bin"));
        // Their binaries come first on PATH, then yours.
        assert!(env["PATH"].starts_with(&format!("{}:{}", t("bin"), t("npm/bin"))));
        if let Ok(host) = std::env::var("PATH") {
            assert!(env["PATH"].ends_with(&host));
        }
    }

    #[test]
    fn open_web_stays_with_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let spec = crate::agents::spec(AgentKind::Claude, &cfg);
        let web = NetworkProfile { presets: vec![NetworkPreset::Web], custom_hosts: vec![] };
        let p = policy_for(&cfg, &spec, Path::new("/work/repo"), Path::new("/tmp/ah/s1"), &web);
        // The runtime gets the agent's own hosts only; the proxy gets "*".
        assert!(!p.allowed_hosts.iter().any(|h| h == crate::proxy::ANY_PUBLIC_HOST));
        assert!(profile_hosts(&web).contains(&crate::proxy::ANY_PUBLIC_HOST.to_string()));
        // Quick tasks' folders are hidden from every session.
        assert!(p.deny_read.contains(&cfg.scratch_dir()));
    }

    #[test]
    fn worktree_sessions_write_their_worktree_and_git_state_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let spec = crate::agents::spec(AgentKind::Claude, &cfg);
        let root = cfg.worktrees_dir().join("ses_1");
        let git = crate::worktree::GitAccess {
            root: root.clone(),
            common_dir: PathBuf::from("/work/repo/.git"),
            admin_dir: PathBuf::from("/work/repo/.git/worktrees/ses_1"),
        };
        let p = policy_for_worktree(&cfg, &spec, &root.join("sub"), Path::new("/tmp/ah/s1"), &NetworkProfile::default(), &git);
        assert!(p.write.contains(&PathBuf::from("/work/repo/.git")));
        assert!(p.write.contains(&root));
        // Not the workspace; not what in .git runs something or points elsewhere.
        assert!(!p.write.iter().any(|w| w == Path::new("/work/repo")));
        if cfg!(target_os = "macos") {
            for f in ["config", "hooks", "commondir", "modules", "config.worktree"] {
                assert!(p.deny_write.contains(&PathBuf::from("/work/repo/.git").join(f)), "{f}");
            }
        }
        assert!(!p.write.contains(&root.join("sub")));
        assert!(p.deny_write.contains(&root.join(".git")));
        assert!(p.deny_write.contains(&PathBuf::from("/work/repo/.git/worktrees/ses_1/commondir")));
        // Other worktrees are hidden; its own is readable again.
        assert!(p.deny_read.contains(&cfg.worktrees_dir()));
        assert!(p.allow_read.contains(&root));
        let expected = if cfg!(target_os = "macos") { 3 + crate::worktree::PROTECTED_GIT_ENTRIES.len() } else { 3 };
        assert_eq!(compile_srt(&p, 1, 2)["filesystem"]["denyWrite"].as_array().unwrap().len(), expected);

        // A quick task can read the folder it writes.
        let scratch = cfg.scratch_dir().join("ses_2");
        let p = policy_for(&cfg, &spec, &scratch, Path::new("/tmp/ah/s2"), &NetworkProfile::default());
        assert!(p.deny_read.contains(&cfg.scratch_dir()) && p.allow_read.contains(&scratch));
    }

    #[test]
    fn host_placeholders_protect_a_shared_folder_until_the_last_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path();
        std::fs::write(w.join(".bashrc"), "mine").unwrap();
        let made = create_placeholders(w);
        // Yours stays as it is; the missing ones are empty and read-only.
        assert_eq!(std::fs::read_to_string(w.join(".bashrc")).unwrap(), "mine");
        assert!(!made.contains(&w.join(".bashrc")));
        for p in [w.join(".gitconfig"), w.join(".vscode"), w.join(".claude/commands")] {
            let m = std::fs::metadata(&p).unwrap();
            assert!(made.contains(&p) && m.len() == 0 && m.permissions().mode() & 0o222 == 0, "{}", p.display());
        }
        assert!(create_placeholders(w).is_empty(), "made twice");
        remove_placeholders(&made);
        assert!(!w.join(".gitconfig").exists() && !w.join(".claude").exists());
        assert_eq!(std::fs::read_to_string(w.join(".bashrc")).unwrap(), "mine");

        // Counting: only the last one out cleans up.
        let f = SharedFolders::default();
        f.acquire(w);
        f.acquire(w);
        assert_eq!(f.users(w), 2);
        f.release(w, std::time::SystemTime::UNIX_EPOCH);
        assert_eq!(f.users(w), 1);
        if cfg!(target_os = "linux") {
            assert!(w.join(".gitconfig").exists());
        }
        f.release(w, std::time::SystemTime::UNIX_EPOCH);
        assert_eq!(f.users(w), 0);
        assert!(!w.join(".gitconfig").exists());
        assert_eq!(std::fs::read_to_string(w.join(".bashrc")).unwrap(), "mine");
    }

    #[test]
    fn profiles_see_only_their_own_login() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let work = profile(AgentKind::Claude, "prof_w", "/home/u/.claude-work");
        let personal = profile(AgentKind::Claude, "prof_p", "/home/u/.claude-personal");
        let codex = profile(AgentKind::Codex, "prof_c", "/home/u/.codex-work");
        let policy = |spec: &AgentSpec| {
            policy_for(
                &cfg,
                spec,
                Path::new("/work/repo"),
                Path::new("/tmp/ah/s1"),
                &NetworkProfile::default(),
            )
        };

        // Claude as "work": its folder is writable and named by CLAUDE_CONFIG_DIR;
        // the default login and the other profiles are hidden.
        let mut spec = crate::agents::spec_for(AgentKind::Claude, Some(&work), &cfg);
        spec.other_logins = vec![personal.config_dir.clone().into(), codex.config_dir.clone().into()];
        assert!(spec
            .env
            .contains(&("CLAUDE_CONFIG_DIR".into(), "/home/u/.claude-work".into())));
        assert!(spec.login_fix.starts_with("CLAUDE_CONFIG_DIR=/home/u/.claude-work "));
        let p = policy(&spec);
        assert_eq!(
            p.write,
            vec![
                PathBuf::from("/work/repo"),
                PathBuf::from("/tmp/ah/s1"),
                cfg.session_tools_dir(),
                PathBuf::from("/home/u/.claude-work")
            ]
        );
        for hidden in [
            "/home/u/.claude",
            "/home/u/.claude.json",
            "/home/u/.claude-personal",
            "/home/u/.codex-work",
            "/home/u/.codex",
        ] {
            assert!(p.deny_read.contains(&PathBuf::from(hidden)), "{hidden}");
        }

        // The default login: profiles' folders are hidden, its own isn't.
        let mut spec = crate::agents::spec(AgentKind::Claude, &cfg);
        spec.other_logins = vec![work.config_dir.clone().into()];
        let p = policy(&spec);
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.claude-work")));
        assert!(!p.deny_read.contains(&PathBuf::from("/home/u/.claude")));
        assert!(p.write.contains(&PathBuf::from("/home/u/.claude")));

        // Codex reads MCP server names from the profile's config.toml.
        let spec = crate::agents::spec_for(AgentKind::Codex, Some(&codex), &cfg);
        assert_eq!(spec.codex_home(&cfg), PathBuf::from("/home/u/.codex-work"));
        assert!(spec.env.contains(&("CODEX_HOME".into(), "/home/u/.codex-work".into())));
        assert_eq!(spec.credential_paths, vec![PathBuf::from("/home/u/.codex-work/auth.json")]);

        // OpenCode: the folder replaces its data folder.
        let oc = profile(AgentKind::Opencode, "prof_o", "/home/u/oc-work");
        let spec = crate::agents::spec_for(AgentKind::Opencode, Some(&oc), &cfg);
        assert!(spec.env.contains(&("XDG_DATA_HOME".into(), "/home/u/oc-work".into())));
        assert!(!spec.state_paths.contains(&PathBuf::from("/home/u/.local/share/opencode")));
        assert!(policy(&spec)
            .deny_read
            .contains(&PathBuf::from("/home/u/.local/share/opencode")));
        // Antigravity: no profiles yet.
        let ag = profile(AgentKind::Antigravity, "prof_g", "/home/u/agy-work");
        let spec = crate::agents::spec_for(AgentKind::Antigravity, Some(&ag), &cfg);
        assert!(spec.profile_problem.is_some());
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

    }

    #[test]
    fn antigravity_sees_its_state_but_not_your_customizations() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::in_dir(dir.path());
        cfg.home_dir = PathBuf::from("/home/u");
        let spec = crate::agents::spec(AgentKind::Antigravity, &cfg);
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &NetworkProfile::default(),
        );
        // Its settings, login file and conversations: writable.
        assert!(p.write.contains(&PathBuf::from("/home/u/.gemini/antigravity-cli")));
        assert!(p.write.contains(&PathBuf::from("/home/u/.gemini/antigravity")));
        // The rest of ~/.gemini is readable, not writable; your plugins, MCP
        // servers, hooks and skills are hidden.
        assert!(!p.write.contains(&PathBuf::from("/home/u/.gemini")));
        assert!(!p.deny_read.contains(&PathBuf::from("/home/u/.gemini")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.gemini/config")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.gemini/skills")));
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.cursor")));
        assert!(spec.args.starts_with(&["acp".to_string()]));
        assert!(!spec.args.contains(&"--sandbox".to_string()));
        assert!(spec.direct_gateway);
        assert!(spec.login_fix.contains("acp --login"));

        // Other agents can't read any of ~/.gemini.
        let spec = crate::agents::spec(AgentKind::Codex, &cfg);
        let p = policy_for(
            &cfg,
            &spec,
            Path::new("/work/repo"),
            Path::new("/tmp/ah/s1"),
            &NetworkProfile::default(),
        );
        assert!(p.deny_read.contains(&PathBuf::from("/home/u/.gemini")));
    }
}
