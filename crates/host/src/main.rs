//! `agenthostd`: the host daemon.

use agenthost_host::{agents, HostConfig};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "agenthostd", version, about = "Self-hosted agent host daemon")]
struct Cli {
    /// Data directory (database, logs, installed tools).
    #[arg(long, global = true, env = "AGENTHOST_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Unix socket path for clients.
    #[arg(long, global = true, env = "AGENTHOST_SOCKET")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the host (the default).
    Run {
        /// Relay WebSocket URL for remote devices, e.g. wss://relay.example.workers.dev
        #[arg(long, env = "AGENTHOST_RELAY_URL")]
        relay_url: Option<String>,
        /// Offer the scripted fake agent (testing and demos).
        #[arg(long)]
        enable_fake_agent: bool,
        /// Store secrets in an owner-only file even if a keychain is available.
        #[arg(long)]
        file_secrets: bool,
        /// Pipe phone screen sharing to a screen-relay engine you started
        /// yourself (e.g. http://127.0.0.1:8080) instead of starting one.
        #[arg(long, env = "AGENTHOST_SCREEN_ENGINE_URL")]
        screen_engine_url: Option<String>,
        /// That engine's token file (`screen-relay status --json` shows it).
        #[arg(long, env = "AGENTHOST_SCREEN_ENGINE_TOKEN_FILE")]
        screen_engine_token_file: Option<PathBuf>,
    },
    /// Install the pinned agent adapters and sandbox runtime.
    InstallTools,
    /// Check prerequisites and agents, then exit.
    Check,
    /// Run a local development relay (same protocol as the Cloudflare relay).
    DevRelay {
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: String,
    },
}

fn config(cli: &Cli) -> Result<HostConfig> {
    let mut cfg = HostConfig::default_for_user()?;
    if let Some(d) = &cli.data_dir {
        cfg.tools_dir = d.join("tools");
        cfg.data_dir = d.clone();
    }
    if let Some(s) = &cli.socket {
        cfg.socket_path = s.clone();
    }
    Ok(cfg)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("AGENTHOST_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let mut cfg = config(&cli)?;
    match cli.cmd {
        Some(Cmd::InstallTools) => install_tools(&cfg),
        Some(Cmd::DevRelay { listen }) => {
            let r = agenthost_host::relay_dev::DevRelay::start(&listen).await?;
            println!("dev relay on {}", r.url());
            tokio::signal::ctrl_c().await?;
            Ok(())
        }
        Some(Cmd::Check) => {
            cfg.prepare()?;
            let host = agenthost_host::Host::open(cfg)?;
            println!("{}", serde_json::to_string_pretty(&host.info())?);
            Ok(())
        }
        Some(Cmd::Run {
            relay_url,
            enable_fake_agent,
            file_secrets,
            screen_engine_url,
            screen_engine_token_file,
        }) => {
            cfg.screen_engine_token_file = screen_engine_token_file;
            cfg.relay_url = relay_url;
            cfg.screen_engine_url = screen_engine_url;
            cfg.enable_fake_agent = enable_fake_agent;
            cfg.file_secrets = file_secrets;
            run(cfg).await
        }
        None => run(cfg).await,
    }
}

async fn run(cfg: HostConfig) -> Result<()> {
    let host = agenthost_host::Host::open(cfg)?;
    let info = host.info();
    if !info.sandbox.ok {
        tracing::warn!(
            "sandbox not ready: {}",
            info.sandbox.problem.clone().unwrap_or_default()
        );
        for p in info.sandbox.prerequisites.iter().filter(|p| !p.present) {
            tracing::warn!(
                "  missing {}: {}",
                p.name,
                p.install.clone().unwrap_or_default()
            );
        }
    }
    let task = agenthost_host::start(host.clone()).await?;
    tokio::select! {
        _ = task => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm() => {}
    }
    tracing::info!("shutting down");
    host.sessions
        .shutdown_and_wait(std::time::Duration::from_secs(10))
        .await;
    if let Some(d) = host.devices.get() {
        d.shutdown();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = std::fs::remove_file(&host.ctx.cfg.socket_path);
    Ok(())
}

async fn sigterm() {
    let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    s.recv().await;
}

fn install_tools(cfg: &HostConfig) -> Result<()> {
    agenthost_host::config::create_private_dir(&cfg.data_dir)?;
    std::fs::create_dir_all(&cfg.tools_dir)?;
    let pkg = cfg.tools_dir.join("package.json");
    if !pkg.exists() {
        std::fs::write(&pkg, r#"{"name":"agenthost-tools","private":true}"#)?;
    }
    let packages = [
        agents::CODEX_ACP_PACKAGE,
        agents::CLAUDE_ACP_PACKAGE,
        agents::SANDBOX_RUNTIME_PACKAGE,
        agenthost_host::screen::SCREEN_RELAY_PACKAGE,
    ];
    println!("Installing into {}:", cfg.tools_dir.display());
    for p in packages {
        println!("  {p}");
    }
    let status = std::process::Command::new("npm")
        .arg("install")
        .arg("--save-exact")
        .arg("--no-fund")
        .arg("--no-audit")
        .arg("--prefix")
        .arg(&cfg.tools_dir)
        .args(packages)
        .status()
        .context("run npm (is Node.js installed?)")?;
    if !status.success() {
        bail!("npm install failed");
    }
    build_screen_engine(cfg);
    println!("Done.");
    Ok(())
}

/// screen-relay's engine is C++ built on this machine, after its system
/// packages (brew on macOS; apt, dnf, pacman or apk on Linux, which asks for
/// sudo). screen-relay asks before installing them. Only phone screen sharing
/// needs it, so a skipped or failed build doesn't fail install-tools.
fn build_screen_engine(cfg: &HostConfig) {
    let bin = cfg.tools_dir.join("node_modules/.bin/screen-relay");
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("install");
    if !cfg!(target_os = "macos") {
        // X11 for the browser desktop, Wayland to attach to compositors like
        // Hyprland. No XFCE: the browser desktop runs the browser alone.
        cmd.args(["--backend", "all", "--desktop", "none"]);
        if is_arch() {
            install_arch_packages(cfg);
        }
    }
    println!("Building screen-relay's engine (for phone screen sharing):");
    match cmd.status() {
        Ok(s) if s.success() => {}
        _ => println!(
            "  Skipped. Screen sharing needs it: run `{} install{}` when you want it.",
            bin.display(),
            if cfg!(target_os = "macos") { "" } else { " --backend all --desktop none" }
        ),
    }
}

/// Arch and distros built on it (Omarchy, EndeavourOS, CachyOS, …).
fn is_arch() -> bool {
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    os.lines()
        .filter_map(|l| l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")))
        .flat_map(|v| v.trim_matches(|c| c == '"' || c == '\'').split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .any(|id| id == "arch")
}

/// screen-relay installs Arch packages with `pacman -Syu`, a full system
/// upgrade, which Omarchy refuses (it upgrades through `omarchy update`). So
/// install what is missing with a plain `pacman -S --needed` first; screen-relay
/// then finds nothing to install and only builds the engine.
fn install_arch_packages(cfg: &HostConfig) {
    let tsv = cfg.tools_dir.join("node_modules/screen-relay/packages.tsv");
    let Ok(table) = std::fs::read_to_string(&tsv) else { return };
    let wanted = arch_packages(&table, &["core", "x11", "wayland"]);
    // `pacman -T` prints the ones that aren't installed.
    let missing = std::process::Command::new("pacman")
        .arg("-T")
        .args(&wanted)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default();
    if missing.is_empty() {
        return;
    }
    println!("screen-relay needs these packages: {}", missing.join(" "));
    let ok = std::process::Command::new("sudo")
        .args(["pacman", "-S", "--needed"])
        .args(&missing)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        println!("  Not installed. If pacman can't find a package, update first (on Omarchy: omarchy update).");
    }
}

/// The pacman column of screen-relay's packages.tsv for these groups.
fn arch_packages(table: &str, groups: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for row in table.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 5 || !groups.contains(&cols[0]) {
            continue;
        }
        for p in cols[4].split(' ').filter(|p| !p.is_empty() && *p != "-") {
            if !out.iter().any(|o| o == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_packages_come_from_the_pacman_column() {
        let tsv = "# group\tname\tapt\tdnf\tpacman\tapk\n\
core\tcompiler\tbuild-essential\tgcc-c++ make\tbase-devel\tbuild-base\n\
core\tsetpriv\tutil-linux\tutil-linux\tutil-linux\tsetpriv\n\
core\tnone\tfoo\tfoo\t-\tfoo\n\
xfce\tterm\txfce4-terminal\txfce4-terminal\txfce4-terminal\txfce4-terminal\n\
wayland\twl\tlibwayland-dev\twayland-devel\twayland\twayland-dev\n\
wayland\twl2\tlibwayland-bin\twayland-devel\twayland\twayland-dev\n";
        assert_eq!(arch_packages(tsv, &["core", "wayland"]), vec!["base-devel", "util-linux", "wayland"]);
    }
}
