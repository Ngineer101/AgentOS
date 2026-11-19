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
        agents::GEMINI_CLI_PACKAGE,
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
        // X11 for the browser desktop, Wayland to attach to compositors like Hyprland.
        cmd.args(["--backend", "all"]);
    }
    println!("Building screen-relay's engine (for phone screen sharing):");
    match cmd.status() {
        Ok(s) if s.success() => {}
        _ => println!(
            "  Skipped. Screen sharing needs it: run `{} install` when you want it.",
            bin.display()
        ),
    }
}
