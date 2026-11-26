//! `agenthost`: the command-line client. Everything the desktop app can do
//! is possible here, using only the client protocol.

use agenthost_cli::{as_event, Client};
use agenthost_proto::jsonrpc::Notification;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "agenthost",
    version,
    about = "Control AgentOS, a self-hosted agent host"
)]
struct Cli {
    /// Host socket (default: the per-user socket, or $AGENTHOST_SOCKET).
    #[arg(long, global = true, env = "AGENTHOST_SOCKET")]
    socket: Option<PathBuf>,
    /// Print raw JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Act as a paired device: connect through the relay using this state
    /// file (created by `agenthost remote pair`) instead of the local socket.
    #[arg(long, global = true, env = "AGENTHOST_REMOTE")]
    remote: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Host status: sandbox health, agents, relay.
    Status,
    /// Manage workspaces.
    #[command(subcommand, alias = "ws")]
    Workspace(WsCmd),
    /// Projects: new workspaces inside the project folders (as a phone can).
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Agent logins: add a profile per subscription (its own config folder).
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Manage sessions.
    #[command(subcommand, alias = "s")]
    Session(SessionCmd),
    /// Start a session, send one prompt, print the reply.
    Run {
        /// Workspace id or folder path.
        workspace: String,
        /// codex, claude, opencode, cursor, antigravity (or fake, if enabled);
        /// add :name for a profile, e.g. claude:work.
        agent: String,
        prompt: String,
        /// End the session afterwards.
        #[arg(long)]
        end: bool,
        /// Attach a file (repeatable).
        #[arg(long = "attach", short = 'a')]
        attach: Vec<PathBuf>,
    },
    /// Interactive chat in a new or existing session.
    Chat {
        /// Workspace id or folder path (for a new session).
        workspace: Option<String>,
        /// Agent, or agent:profile (default codex).
        agent: Option<String>,
        /// Continue this session instead.
        #[arg(long)]
        session: Option<String>,
    },
    /// List pending MCP approvals.
    Approvals,
    /// Approve a pending MCP call.
    Approve { approval_id: String },
    /// Deny a pending MCP call.
    Deny { approval_id: String },
    /// Manage MCP connectors.
    #[command(subcommand, alias = "conn")]
    Connector(ConnCmd),
    /// Paired devices.
    #[command(subcommand, alias = "dev")]
    Device(DeviceCmd),
    /// Voice input: the local speech model.
    #[command(subcommand)]
    Voice(VoiceCmd),
    /// Screen sharing with paired phones (through screen-relay).
    #[command(subcommand)]
    Screen(ScreenCmd),
    /// Push notifications to paired iPhones (through Apple's APNs).
    #[command(subcommand)]
    Push(PushCmd),
    /// Automations: sessions that run on a schedule, from a webhook or by hand.
    #[command(subcommand, alias = "auto")]
    Automation(AutoCmd),
    /// Notes: ideas and todos kept with a workspace for later.
    #[command(subcommand)]
    Note(NoteCmd),
    /// Send any protocol method.
    Raw {
        method: String,
        params: Option<String>,
    },
    /// Pair this CLI as a remote device (like a phone).
    #[command(subcommand)]
    Remote(RemoteCmd),
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// The project folders, or the subfolders of one (or of a folder inside one).
    Ls { path: Option<String> },
    /// Make a folder inside a project folder a workspace.
    Use { path: String },
    /// Create a new folder in a project folder (or inside one) as a workspace.
    New { parent: String, name: String },
    /// Show or replace the project folders (desktop only; default ~/Code).
    Roots {
        /// Replace them (comma separated).
        #[arg(long, value_delimiter = ',')]
        set: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Every agent login: the default ones and the profiles.
    List,
    /// Add a profile: another login of an agent, kept in its own config
    /// folder (what CLAUDE_CONFIG_DIR, CODEX_HOME, or XDG_DATA_HOME for
    /// OpenCode point at). Use it as agent:name.
    Add {
        /// codex, claude, opencode or cursor.
        agent: String,
        /// A short name, e.g. work.
        name: String,
        /// The agent's config folder, e.g. ~/.claude-work.
        #[arg(long)]
        config_dir: String,
    },
    /// Remove a profile (agent:name or its id). Its folder is left as it is.
    Rm { profile: String },
}

#[derive(Subcommand)]
enum VoiceCmd {
    /// Whether the speech model is installed.
    Status,
    /// Download and install the speech model, showing progress.
    Download,
    /// Transcribe a WAV recording.
    Transcribe {
        wav: PathBuf,
        /// Language hint, e.g. en (detected when absent).
        #[arg(long)]
        language: Option<String>,
    },
}

#[derive(Subcommand)]
enum ScreenCmd {
    /// What phones can see, whether it is allowed, and what is running.
    Status,
    /// Let paired phones watch the screen (desktop only).
    Allow {
        /// Let them click and type too.
        #[arg(long)]
        control: bool,
    },
    /// Stop letting phones see the screen; ends any stream (desktop only).
    Off,
    /// Stop screen sharing now.
    Stop,
    /// Watch as a phone would and save the H.264 stream (needs --remote).
    /// Play it with `ffplay out.h264`.
    Record {
        out: PathBuf,
        /// desktop or browser.
        #[arg(long, default_value = "desktop")]
        mode: String,
        /// The monitor to show in desktop mode, by name (see `screen
        /// status`); the first one by default.
        #[arg(long)]
        output: Option<String>,
        /// Stop after this many seconds.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Click at x,y (fractions of the screen, e.g. 0.5,0.5) after the
        /// first frame; needs control to be allowed.
        #[arg(long, value_parser = parse_point)]
        click: Option<(f64, f64)>,
        /// Type this text after the first frame; needs control.
        #[arg(long)]
        r#type: Option<String>,
    },
}

#[derive(Subcommand)]
enum PushCmd {
    /// Whether the host has an APNs key, and which phones get notifications.
    Status,
    /// Give the host the APNs key of the Apple Developer account the iOS app
    /// is signed with (Certificates, Identifiers & Profiles → Keys). A phone
    /// can do this while pairing instead.
    Setup {
        /// The key file, AuthKey_<KEY ID>.p8.
        #[arg(long)]
        key: PathBuf,
        /// The key's id (read from the file name when left out).
        #[arg(long)]
        key_id: Option<String>,
        /// Your Apple team id (Apple Developer → Membership).
        #[arg(long)]
        team_id: String,
    },
    /// Send a test notification to every registered phone, or one.
    Test {
        #[arg(long)]
        device: Option<String>,
    },
    /// Forget the APNs key; phones stop getting notifications.
    RemoveKey,
}

#[derive(Subcommand)]
enum AutoCmd {
    /// List automations, their next run and last result.
    Ls,
    /// One automation, with its webhook URL if it has one.
    Show { id: String },
    /// Add an automation. Give one of --cron, --webhook or --manual.
    Add {
        name: String,
        /// Workspace id, name or folder.
        #[arg(long, short)]
        workspace: String,
        /// Agent, or agent:profile.
        #[arg(long, short)]
        agent: String,
        /// What each run should do (a webhook's has a preset).
        #[arg(long, short)]
        prompt: Option<String>,
        /// Read the prompt from a file.
        #[arg(long, conflicts_with = "prompt")]
        prompt_file: Option<PathBuf>,
        /// Five-field cron in this computer's time ("0 3 * * *"), or @daily, @weekly…
        #[arg(long, conflicts_with_all = ["webhook", "manual"])]
        cron: Option<String>,
        /// sentry, github or generic: runs start from POSTs to its URL.
        #[arg(long, conflicts_with = "manual")]
        webhook: Option<String>,
        /// Only by hand (`agenthost automation run`).
        #[arg(long)]
        manual: bool,
        /// The sender's secret: Sentry's client secret, GitHub's webhook
        /// secret, or a bearer token for generic webhooks.
        #[arg(long)]
        secret: Option<String>,
        /// Webhook rule FIELD=VALUE, e.g. payload.action=opened (repeatable).
        #[arg(long = "filter")]
        filter: Vec<String>,
        /// Webhook dedupe key template, e.g. {{payload.issue.id}}.
        #[arg(long)]
        dedupe: Option<String>,
        /// Minutes a dedupe key holds (default a day).
        #[arg(long)]
        cooldown: Option<u32>,
        /// Tools its runs use without asking: create_pull_request, git_push.
        #[arg(long, value_delimiter = ',')]
        auto_approve: Vec<String>,
        /// Stop a run after this many minutes (default 60).
        #[arg(long)]
        max_minutes: Option<u32>,
        /// Add it switched off.
        #[arg(long)]
        off: bool,
    },
    /// Change an automation.
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long, conflicts_with = "off")]
        on: bool,
        #[arg(long)]
        off: bool,
        /// Replace the pre-approved tools ("" for none).
        #[arg(long, value_delimiter = ',')]
        auto_approve: Option<Vec<String>>,
        #[arg(long)]
        secret: Option<String>,
        /// Give the webhook a new URL; the old one stops working.
        #[arg(long)]
        rotate_webhook: bool,
        #[arg(long)]
        max_minutes: Option<u32>,
    },
    /// Start a run now.
    Run {
        id: String,
        /// Wait for it to finish and print the summary.
        #[arg(long)]
        wait: bool,
    },
    /// Its runs, newest first.
    Runs {
        id: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Delete an automation (its runs' sessions stay).
    Rm { id: String },
    /// Check a cron expression and show its next times.
    Check { cron: String },
}

#[derive(Subcommand)]
enum NoteCmd {
    /// Open notes (with --all, done ones too), by workspace.
    Ls {
        /// Only this workspace (id, name or folder).
        #[arg(long, short)]
        workspace: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Add a note; the first line is its title.
    Add {
        text: String,
        /// Workspace id, name or folder.
        #[arg(long, short)]
        workspace: String,
    },
    /// Replace a note's text.
    Edit { id: String, text: String },
    /// Mark a note done.
    Done { id: String },
    /// Open a done note again.
    Reopen { id: String },
    /// Delete a note (sessions started from it stay).
    Rm { id: String },
}

async fn note_cmd(c: &Client, cmd: NoteCmd, json_out: bool) -> Result<()> {
    let show = |n: &Note| -> Result<()> {
        if json_out {
            print_json(&serde_json::to_value(n)?);
        } else {
            print_note(n);
        }
        Ok(())
    };
    match cmd {
        NoteCmd::Ls { workspace, all } => {
            let ws = match &workspace {
                Some(w) => Some(resolve_workspace(c, w).await?.workspace_id),
                None => None,
            };
            let list: Vec<Note> = c.call(m::NOTE_LIST, json!({"workspace_id": ws})).await?;
            let list: Vec<Note> = list.into_iter().filter(|n| all || n.done_at.is_none()).collect();
            if json_out {
                print_json(&serde_json::to_value(&list)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("No notes. Add one with `agenthost note add \"…\" -w <workspace>`.");
            }
            let names: Vec<Workspace> = c.call(m::WORKSPACE_LIST, json!({})).await?;
            let mut last = None;
            for n in &list {
                if last != Some(&n.workspace_id) {
                    let name = names.iter().find(|w| w.workspace_id == n.workspace_id).map(|w| w.name.as_str()).unwrap_or(&n.workspace_id);
                    println!("{name}");
                    last = Some(&n.workspace_id);
                }
                print_note(n);
            }
        }
        NoteCmd::Add { text, workspace } => {
            let ws = resolve_workspace(c, &workspace).await?;
            show(&c.call(m::NOTE_CREATE, json!({"workspace_id": ws.workspace_id, "text": text})).await?)?
        }
        NoteCmd::Edit { id, text } => show(&c.call(m::NOTE_UPDATE, json!({"note_id": id, "text": text})).await?)?,
        NoteCmd::Done { id } => show(&c.call(m::NOTE_UPDATE, json!({"note_id": id, "done": true})).await?)?,
        NoteCmd::Reopen { id } => show(&c.call(m::NOTE_UPDATE, json!({"note_id": id, "done": false})).await?)?,
        NoteCmd::Rm { id } => {
            let _: Value = c.call(m::NOTE_DELETE, json!({"note_id": id})).await?;
            if !json_out {
                println!("Deleted {id}");
            }
        }
    }
    Ok(())
}

/// "  [ ] Title  (note_…, 2 sessions)" and the body's first line under it.
fn print_note(n: &Note) {
    let mark = if n.done_at.is_some() { "[x]" } else { "[ ]" };
    let sessions = match n.session_ids.len() {
        0 => String::new(),
        1 => ", 1 session".into(),
        k => format!(", {k} sessions"),
    };
    println!("  {mark} {}  ({}{sessions})", n.title(), n.note_id);
    if let Some(line) = n.body().lines().map(str::trim).find(|l| !l.is_empty()) {
        println!("      {line}");
    }
}

/// "in 3h 20m", "5m ago".
fn when(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if ts <= now {
        return ago(ts);
    }
    let m = (ts - now + 59_999) / 60_000;
    match m {
        m if m < 60 => format!("in {m}m"),
        m if m < 1440 => format!("in {}h {}m", m / 60, m % 60),
        m => format!("in {}d {}h", m / 1440, (m % 1440) / 60),
    }
}

fn outcome_word(o: RunOutcome) -> &'static str {
    match o {
        RunOutcome::Running => "running",
        RunOutcome::Succeeded => "done",
        RunOutcome::Failed => "failed",
        RunOutcome::Cancelled => "stopped",
        RunOutcome::TimedOut => "timed out",
        RunOutcome::Skipped => "skipped",
    }
}

fn print_automation(a: &Automation, long: bool) {
    let what = match &a.trigger {
        AutomationTrigger::Manual => "by hand".to_string(),
        AutomationTrigger::Schedule { .. } => a.schedule_text.clone().unwrap_or_default(),
        AutomationTrigger::Webhook { source, .. } => format!("{} webhook", source.display_name()),
    };
    let state = if a.enabled { "" } else { "  (off)" };
    let next = a.next_run_at.map(|t| format!("  next {}", when(t))).unwrap_or_default();
    let last = a
        .last_run
        .as_ref()
        .map(|r| format!("  last {} {}", outcome_word(r.outcome), ago(r.started_at)))
        .unwrap_or_default();
    println!("{}  {}  {what}{state}{next}{last}", a.automation_id, a.name);
    if !long {
        return;
    }
    println!("  agent: {}  workspace: {}  max {} min", a.agent.as_str(), a.workspace_id, a.max_minutes);
    if !a.auto_approve.is_empty() {
        println!("  runs without asking: {}", a.auto_approve.join(", "));
    }
    if let Some(w) = &a.webhook {
        match &w.url {
            Some(u) => println!("  webhook URL: {u}"),
            None => println!("  webhook URL: none (the host has no relay; start it with --relay-url)"),
        }
        println!("  secret: {}", if w.has_secret { "stored" } else { "none" });
        if let Some(d) = &w.last_delivery {
            println!("  last delivery {}: {} — {}", ago(d.at), d.result, d.detail);
        }
    }
    println!("  prompt:\n{}", a.prompt.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"));
}

fn print_run(r: &AutomationRun) {
    let trig = match r.trigger {
        RunTrigger::Schedule => "schedule",
        RunTrigger::CatchUp => "catch-up",
        RunTrigger::Webhook => "webhook",
        RunTrigger::Manual => "by hand",
    };
    println!(
        "{}  {}  {}  {}  {}",
        r.run_id,
        ago(r.started_at),
        trig,
        outcome_word(r.outcome),
        r.session_id.as_deref().unwrap_or("-")
    );
    if let Some(d) = &r.detail {
        println!("    {d}");
    }
    if let Some(s) = &r.summary {
        println!("    {s}");
    }
}

async fn automation_cmd(c: &Client, cmd: AutoCmd, json_out: bool) -> Result<()> {
    let show = |a: &Automation| -> Result<()> {
        if json_out {
            print_json(&serde_json::to_value(a)?);
        } else {
            print_automation(a, true);
        }
        Ok(())
    };
    match cmd {
        AutoCmd::Ls => {
            let list: Vec<Automation> = c.call(m::AUTOMATION_LIST, json!({})).await?;
            if json_out {
                print_json(&serde_json::to_value(&list)?);
            } else if list.is_empty() {
                println!("No automations. Add one with `agenthost automation add`.");
            }
            if !json_out {
                for a in &list {
                    print_automation(a, false);
                }
            }
        }
        AutoCmd::Show { id } => show(&c.call(m::AUTOMATION_GET, json!({"automation_id": id})).await?)?,
        AutoCmd::Add {
            name, workspace, agent, prompt, prompt_file, cron, webhook, manual, secret, filter, dedupe, cooldown,
            auto_approve, max_minutes, off,
        } => {
            let ws = resolve_workspace(c, &workspace).await?;
            let (agent, profile_id) = resolve_agent(c, &agent).await?;
            let trigger = match (cron, webhook, manual) {
                (Some(cron), None, false) => AutomationTrigger::Schedule { cron },
                (None, Some(src), false) => {
                    let source = match src.as_str() {
                        "sentry" => WebhookSource::Sentry,
                        "github" => WebhookSource::Github,
                        "generic" => WebhookSource::Generic,
                        other => bail!("unknown webhook source {other:?}; use sentry, github or generic"),
                    };
                    let filter = filter
                        .iter()
                        .map(|f| {
                            f.split_once('=')
                                .map(|(p, v)| WebhookMatch { path: p.trim().into(), equals: v.into() })
                                .ok_or_else(|| anyhow!("a filter is FIELD=VALUE, not {f:?}"))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    AutomationTrigger::Webhook { source, filter, dedupe_key: dedupe, cooldown_minutes: cooldown.unwrap_or(24 * 60) }
                }
                (None, None, true) => AutomationTrigger::Manual,
                _ => bail!("give one of --cron, --webhook or --manual"),
            };
            let prompt = match (prompt, prompt_file) {
                (Some(p), _) => p,
                (None, Some(f)) => std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?,
                (None, None) => match &trigger {
                    AutomationTrigger::Webhook { source: WebhookSource::Sentry, .. } => SENTRY_PROMPT.into(),
                    AutomationTrigger::Webhook { source: WebhookSource::Github, .. } => GITHUB_PROMPT.into(),
                    AutomationTrigger::Webhook { .. } => GENERIC_PROMPT.into(),
                    _ => bail!("say what each run should do with --prompt or --prompt-file"),
                },
            };
            let p = AutomationCreateParams {
                name,
                workspace_id: ws.workspace_id,
                agent,
                profile_id,
                prompt,
                trigger,
                auto_approve,
                max_minutes,
                enabled: !off,
                webhook_secret: secret,
            };
            show(&c.call(m::AUTOMATION_CREATE, serde_json::to_value(&p)?).await?)?;
        }
        AutoCmd::Edit { id, name, prompt, cron, on, off, auto_approve, secret, rotate_webhook, max_minutes } => {
            let p = AutomationUpdateParams {
                automation_id: id,
                name,
                enabled: if on { Some(true) } else if off { Some(false) } else { None },
                prompt,
                trigger: cron.map(|cron| AutomationTrigger::Schedule { cron }),
                auto_approve: auto_approve.map(|v| v.into_iter().filter(|t| !t.is_empty()).collect()),
                max_minutes,
                webhook_secret: secret,
                rotate_webhook,
                ..Default::default()
            };
            show(&c.call(m::AUTOMATION_UPDATE, serde_json::to_value(&p)?).await?)?;
        }
        AutoCmd::Run { id, wait } => {
            let mut r: AutomationRun = c.call(m::AUTOMATION_RUN_NOW, json!({"automation_id": id})).await?;
            while wait && r.outcome == RunOutcome::Running {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let runs: Vec<AutomationRun> = c.call(m::AUTOMATION_RUNS, json!({"automation_id": id, "limit": 20})).await?;
                if let Some(now) = runs.into_iter().find(|x| x.run_id == r.run_id) {
                    r = now;
                }
            }
            if json_out {
                print_json(&serde_json::to_value(&r)?);
            } else {
                print_run(&r);
            }
        }
        AutoCmd::Runs { id, limit } => {
            let runs: Vec<AutomationRun> = c.call(m::AUTOMATION_RUNS, json!({"automation_id": id, "limit": limit})).await?;
            if json_out {
                print_json(&serde_json::to_value(&runs)?);
            } else if runs.is_empty() {
                println!("No runs yet.");
            }
            if !json_out {
                runs.iter().for_each(print_run);
            }
        }
        AutoCmd::Rm { id } => {
            c.request(m::AUTOMATION_DELETE, json!({"automation_id": id})).await?;
            if !json_out {
                println!("Deleted.");
            }
        }
        AutoCmd::Check { cron } => {
            let r: ScheduleCheck = c.call(m::AUTOMATION_CHECK_SCHEDULE, json!({"cron": cron})).await?;
            if json_out {
                print_json(&serde_json::to_value(&r)?);
            } else if let Some(e) = &r.error {
                bail!("{e}");
            } else {
                println!("{}", r.text.unwrap_or_default());
                for t in r.next {
                    println!("  {}", when(t));
                }
            }
        }
    }
    Ok(())
}

/// `AuthKey_ABC123DEFG.p8` → `ABC123DEFG`.
fn key_id_from_file_name(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.strip_prefix("AuthKey_")?;
    (id.len() == 10 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_ascii_uppercase())
}

async fn push_cmd(c: &Client, cmd: PushCmd, json_out: bool) -> Result<()> {
    let st: PushStatus = match cmd {
        PushCmd::Status => c.call(m::PUSH_STATUS, json!({})).await?,
        PushCmd::Setup { key, key_id, team_id } => {
            let Some(key_id) = key_id.or_else(|| key_id_from_file_name(&key)) else {
                bail!("can't read the key id from the file name; add --key-id");
            };
            let pem = std::fs::read_to_string(&key)
                .with_context(|| format!("reading {}", key.display()))?;
            c.call(m::PUSH_SET_KEY, json!({"key_p8": pem, "key_id": key_id, "team_id": team_id}))
                .await?
        }
        PushCmd::Test { device } => {
            let r: PushTestResult = c.call(m::PUSH_TEST, json!({"device_id": device})).await?;
            if json_out {
                print_json(&serde_json::to_value(&r)?);
            } else {
                println!("Sent to {} phone{}.", r.sent, if r.sent == 1 { "" } else { "s" });
                for e in &r.errors {
                    println!("Not sent: {e}");
                }
            }
            return Ok(());
        }
        PushCmd::RemoveKey => c.call(m::PUSH_CLEAR_KEY, json!({})).await?,
    };
    if json_out {
        print_json(&serde_json::to_value(&st)?);
        return Ok(());
    }
    let direct = st.devices.iter().any(|d| d.gateway.is_none());
    match (&st.key_id, &st.team_id) {
        (Some(k), Some(t)) if st.configured => println!("APNs key: {k} (team {t})"),
        _ if !direct => println!("APNs key: none (not needed: a relay sends to Apple for every phone)"),
        _ => println!("APNs key: not set up (`agenthost push setup`); phones marked \"own key\" get nothing until it is"),
    }
    if let Some(d) = &st.this_device {
        println!("This device: {} ({})", d.name, d.environment.as_str());
    }
    if st.devices.is_empty() && st.this_device.is_none() {
        println!("No phone has turned notifications on.");
    }
    for d in &st.devices {
        let mut what = vec![];
        if d.prefs.approvals {
            what.push("approvals");
        }
        if d.prefs.turn_done {
            what.push("finished turns");
        }
        if d.prefs.failed {
            what.push("failures");
        }
        if d.prefs.automations {
            what.push("automations");
        }
        let what = if what.is_empty() { "nothing".to_string() } else { what.join(", ") };
        let via = match &d.gateway {
            Some(url) => format!("via {}", url.trim_start_matches("https://")),
            None => "own key".to_string(),
        };
        print!("{}  {}  {}  {}  {}", d.device_id, d.name, d.environment.as_str(), via, what);
        match &d.last_error {
            Some(e) => println!("  (last refused: {e})"),
            None => println!(),
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// Pair using the QR payload shown by `agenthost device pair`.
    Pair {
        qr: String,
        /// Where to keep this device's keys.
        #[arg(long)]
        state: PathBuf,
        #[arg(long, default_value = "AgentOS CLI")]
        name: String,
    },
}

#[derive(Subcommand)]
enum WsCmd {
    List,
    /// Add a folder as a workspace.
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
        /// Network presets: packages, github, web (any public site) (comma separated).
        #[arg(long, value_delimiter = ',')]
        profile: Vec<String>,
        /// Extra allowed hosts.
        #[arg(long = "host")]
        hosts: Vec<String>,
    },
    /// Change a workspace.
    Set {
        workspace: String,
        #[arg(long)]
        name: Option<String>,
        /// Replace the network presets: packages, github, web ("agent-only" for none).
        #[arg(long, value_delimiter = ',')]
        profile: Option<Vec<String>>,
        /// Add one host to the custom profile.
        #[arg(long)]
        add_host: Option<String>,
        /// Replace the custom host list.
        #[arg(long, value_delimiter = ',')]
        hosts: Option<Vec<String>>,
        /// Enabled connector ids (comma separated, "none" for none).
        #[arg(long, value_delimiter = ',')]
        connectors: Option<Vec<String>>,
    },
    Rm {
        workspace: String,
    },
    /// Show or set the login new sessions here start with (agent or
    /// agent:profile; "none" clears it).
    Default {
        workspace: String,
        agent: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    List {
        #[arg(long)]
        workspace: Option<String>,
        /// Keep printing changes.
        #[arg(long)]
        watch: bool,
        /// List archived sessions instead.
        #[arg(long)]
        archived: bool,
    },
    /// Start a session.
    New {
        workspace: String,
        /// Agent, or agent:profile.
        agent: String,
    },
    /// Print a session's transcript.
    Show {
        session: String,
    },
    /// Send a prompt.
    Prompt {
        session: String,
        text: String,
        /// Wait for the turn to end, printing output.
        #[arg(long)]
        wait: bool,
        /// Client prompt id (for safe retries).
        #[arg(long)]
        id: Option<String>,
    },
    /// Stream a session's events.
    Attach {
        session: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    Cancel {
        session: String,
    },
    End {
        session: String,
    },
    /// End a session (if live) and hide it from the list.
    Archive {
        session: String,
    },
    /// Show an archived session in the list again.
    Unarchive {
        session: String,
    },
    /// Show the agent's settings for a session (model, effort, …), or change one.
    Config {
        session: String,
        /// The setting to change, e.g. `model`.
        config_id: Option<String>,
        /// Its new value.
        value: Option<String>,
    },
    /// Ask a side question (/btw) without interrupting the session.
    Btw {
        session: String,
        question: String,
        /// Continue this side conversation.
        #[arg(long)]
        side: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConnCmd {
    List,
    /// Add a remote MCP server (streamable HTTP).
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
        /// none, bearer or oauth.
        #[arg(long)]
        auth: Option<String>,
        /// Bearer token (read from stdin if "-").
        #[arg(long)]
        token: Option<String>,
    },
    /// Change policy or settings.
    Set {
        connector: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        enabled: Option<bool>,
        /// auto, ask or disabled.
        #[arg(long)]
        write_policy: Option<String>,
        #[arg(long)]
        read_policy: Option<String>,
        /// Per-tool policy, tool=auto|ask|disabled|default (repeatable).
        #[arg(long = "tool")]
        tools: Vec<String>,
    },
    /// Sign in with OAuth.
    SignIn {
        connector: String,
    },
    /// Re-fetch the tool list.
    Refresh {
        connector: String,
    },
    Rm {
        connector: String,
    },
}

#[derive(Subcommand)]
enum DeviceCmd {
    List,
    /// Pair a phone: shows a QR code, then asks you to confirm the code.
    Pair,
    Rename {
        device: String,
        name: String,
    },
    Revoke {
        device: String,
    },
}

fn socket_path(cli: &Cli) -> PathBuf {
    if let Some(s) = &cli.socket {
        return s.clone();
    }
    let uid = unsafe_getuid();
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_default();
        return PathBuf::from(format!(
            "{home}/Library/Application Support/agenthost/host.sock"
        ));
    }
    if let Ok(x) = std::env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(x).join("agenthost");
        if p.as_os_str().len() < 40 {
            return p.join("host.sock");
        }
    }
    PathBuf::from(format!("/tmp/agenthost-{uid}/host.sock"))
}

fn unsafe_getuid() -> u32 {
    // Avoid a libc dependency: read it from /proc or `id -u`.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|u| u.parse().ok())
        })
        .or_else(|| {
            std::process::Command::new("id")
                .arg("-u")
                .output()
                .ok()
                .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        })
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

async fn resolve_workspace(c: &Client, s: &str) -> Result<Workspace> {
    let list: Vec<Workspace> = c.call(m::WORKSPACE_LIST, json!({})).await?;
    // "quick": a quick task, in a fresh folder of its own.
    let s = if s == "quick" { QUICK_WORKSPACE_ID } else { s };
    if let Some(w) = list.iter().find(|w| w.workspace_id == s || w.name == s) {
        return Ok(w.clone());
    }
    let p = std::fs::canonicalize(s).map_err(|_| anyhow!("no workspace {s:?}"))?;
    let ps = p.display().to_string();
    if let Some(w) = list.into_iter().find(|w| w.path == ps) {
        return Ok(w);
    }
    bail!(
        "{} is not a workspace; add it with: agenthost workspace add {}",
        ps,
        ps
    )
}

fn parse_agent(s: &str) -> Result<AgentKind> {
    AgentKind::parse(s).ok_or_else(|| {
        anyhow!("unknown agent {s:?}; use codex, claude, opencode, cursor or antigravity")
    })
}

/// `claude` (the default login), `claude:work` (a profile by name) or a
/// profile id, as the agent and the profile id to start a session with.
async fn resolve_agent(c: &Client, s: &str) -> Result<(AgentKind, Option<String>)> {
    let (kind, name) = match s.split_once(':') {
        Some((k, n)) => (Some(parse_agent(k)?), Some(n.trim())),
        None if s.starts_with("prof_") => (None, None),
        None => return Ok((parse_agent(s)?, None)),
    };
    let info: HostInfo = c.call(m::HOST_INFO, json!({})).await?;
    let found = info.agents.iter().find(|a| match (kind, name) {
        (Some(k), Some(n)) => {
            a.agent == k
                && a.profile_name
                    .as_deref()
                    .map(|p| p.eq_ignore_ascii_case(n))
                    .unwrap_or(n.eq_ignore_ascii_case("default"))
        }
        _ => a.profile_id.as_deref() == Some(s),
    });
    match found {
        Some(a) => Ok((a.agent, a.profile_id.clone())),
        None => {
            let names: Vec<String> = info
                .agents
                .iter()
                .filter(|a| kind.map(|k| a.agent == k).unwrap_or(true))
                .filter_map(|a| a.profile_name.as_ref().map(|n| format!("{}:{n}", a.agent)))
                .collect();
            bail!(
                "no agent profile {s:?}{}",
                if names.is_empty() {
                    "; add one with: agenthost agent add <agent> <name> --config-dir <folder>".to_string()
                } else {
                    format!("; profiles: {}", names.join(", "))
                }
            )
        }
    }
}

fn parse_presets(v: &[String]) -> Result<Vec<NetworkPreset>> {
    let mut out = vec![];
    for p in v {
        match p.trim() {
            "packages" => out.push(NetworkPreset::Packages),
            "web" => out.push(NetworkPreset::Web),
            "github" => out.push(NetworkPreset::Github),
            "agent-only" | "" => {}
            other => bail!("unknown profile {other:?}; use packages, github, web or agent-only"),
        }
    }
    Ok(out)
}

fn parse_policy(s: &str) -> Result<ToolPolicy> {
    Ok(match s {
        "auto" => ToolPolicy::Auto,
        "ask" => ToolPolicy::Ask,
        "disabled" | "off" => ToolPolicy::Disabled,
        _ => bail!("policy must be auto, ask or disabled"),
    })
}

async fn run(cli: Cli) -> Result<()> {
    if let Cmd::Remote(RemoteCmd::Pair { qr, state, name }) = &cli.cmd {
        println!("Pairing with the host…");
        let st = agenthost_cli::remote::pair(qr, name, |code| {
            println!("Confirmation code: {code}\nCheck that the host shows the same code, and confirm there.");
        })
        .await?;
        st.save(state)?;
        println!(
            "Paired as {} with {}. Use: agenthost --remote {} status",
            st.device_id,
            st.host_name,
            state.display()
        );
        return Ok(());
    }
    let (c, mut notes) = match &cli.remote {
        Some(state) => {
            let st = agenthost_cli::remote::DeviceState::load(state)?;
            let (c, n, _closed) = agenthost_cli::remote::connect(&st).await?;
            (c, n)
        }
        None => Client::connect(&socket_path(&cli)).await?,
    };
    let json_out = cli.json;
    let remote_state = cli.remote.clone();
    match cli.cmd {
        Cmd::Screen(cmd) => screen_cmd(&c, cmd, remote_state.as_deref(), json_out).await?,
        Cmd::Push(cmd) => push_cmd(&c, cmd, json_out).await?,
        Cmd::Automation(cmd) => automation_cmd(&c, cmd, json_out).await?,
        Cmd::Note(cmd) => note_cmd(&c, cmd, json_out).await?,
        Cmd::Status => {
            let info: HostInfo = c.call(m::HOST_INFO, json!({})).await?;
            if json_out {
                print_json(&serde_json::to_value(&info)?);
            } else {
                print_status(&info);
            }
        }
        Cmd::Project(p) => match p {
            ProjectCmd::Ls { path } => {
                let path = path.map(|p| {
                    std::fs::canonicalize(&p)
                        .map(|c| c.display().to_string())
                        .unwrap_or(p)
                });
                let b: ProjectBrowse = c.call(m::PROJECT_BROWSE, json!({"path": path})).await?;
                if json_out {
                    print_json(&serde_json::to_value(&b)?);
                } else {
                    if b.path.is_none() {
                        println!("project folders:");
                    }
                    for f in &b.folders {
                        let mark = if f.workspace_id.is_some() { "  (workspace)" } else { "" };
                        println!("  {}{mark}", if b.path.is_none() { &f.path } else { &f.name });
                    }
                    if b.path.is_some() && b.folders.is_empty() {
                        println!("  (no folders)");
                    }
                }
            }
            ProjectCmd::Use { path } => {
                let path = std::fs::canonicalize(&path)
                    .map(|c| c.display().to_string())
                    .unwrap_or(path);
                let w: Workspace = c.call(m::PROJECT_CREATE, json!({"path": path})).await?;
                print_workspace_added(&w, json_out)?;
            }
            ProjectCmd::New { parent, name } => {
                let parent = std::fs::canonicalize(&parent)
                    .map(|c| c.display().to_string())
                    .unwrap_or(parent);
                let w: Workspace = c
                    .call(m::PROJECT_CREATE, json!({"parent": parent, "name": name}))
                    .await?;
                print_workspace_added(&w, json_out)?;
            }
            ProjectCmd::Roots { set } => {
                let roots: Vec<String> = match set {
                    Some(r) => c.call(m::PROJECT_SET_ROOTS, json!({"roots": r})).await?,
                    None => {
                        let b: ProjectBrowse = c.call(m::PROJECT_BROWSE, json!({})).await?;
                        b.roots
                    }
                };
                if json_out {
                    print_json(&serde_json::to_value(&roots)?);
                } else {
                    for r in roots {
                        println!("{r}");
                    }
                }
            }
        },
        Cmd::Agent(a) => match a {
            AgentCmd::List => {
                let info: HostInfo = c.call(m::HOST_INFO, json!({})).await?;
                if json_out {
                    print_json(&serde_json::to_value(&info.agents)?);
                } else {
                    print_agents(&info.agents);
                }
            }
            AgentCmd::Add {
                agent,
                name,
                config_dir,
            } => {
                let p = c.add_profile(parse_agent(&agent)?, &name, &config_dir).await?;
                if json_out {
                    print_json(&serde_json::to_value(&p)?);
                } else {
                    println!(
                        "Added {} profile {} ({}, {}). Start it as {}:{}",
                        p.agent.display_name(),
                        p.name,
                        p.config_dir,
                        p.profile_id,
                        p.agent,
                        p.name.to_lowercase()
                    );
                }
            }
            AgentCmd::Rm { profile } => {
                let id = if profile.starts_with("prof_") {
                    profile.clone()
                } else {
                    match resolve_agent(&c, &profile).await? {
                        (_, Some(id)) if id.starts_with("prof_") => id,
                        _ => bail!("{profile:?} is a default login, not a profile"),
                    }
                };
                c.remove_profile(&id).await?;
                if !json_out {
                    println!("Removed {profile}");
                }
            }
        },
        Cmd::Workspace(w) => match w {
            WsCmd::List => {
                let list: Vec<Workspace> = c.call(m::WORKSPACE_LIST, json!({})).await?;
                if json_out {
                    print_json(&serde_json::to_value(&list)?);
                } else if list.is_empty() {
                    println!("No workspaces. Add one with: agenthost workspace add <folder>");
                } else {
                    for w in list {
                        println!(
                            "{}  {:<20} {:<6} {:<18} {}",
                            w.workspace_id,
                            w.name,
                            format!("{:?}", w.kind).to_lowercase(),
                            w.network.label(),
                            w.path
                        );
                        if !w.connectors.is_empty() {
                            println!("    connectors: {}", w.connectors.join(", "));
                        }
                    }
                }
            }
            WsCmd::Add {
                path,
                name,
                profile,
                hosts,
            } => {
                let path =
                    std::fs::canonicalize(&path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
                let network = NetworkProfile {
                    presets: parse_presets(&profile)?,
                    custom_hosts: hosts,
                };
                let w: Workspace = c
                    .call(
                        m::WORKSPACE_ADD,
                        json!({"path": path, "name": name, "network": network}),
                    )
                    .await?;
                if json_out {
                    print_json(&serde_json::to_value(&w)?);
                } else {
                    println!(
                        "Added {} ({}) as {}",
                        w.path,
                        w.network.label(),
                        w.workspace_id
                    );
                }
            }
            WsCmd::Set {
                workspace,
                name,
                profile,
                add_host,
                hosts,
                connectors,
            } => {
                let w = resolve_workspace(&c, &workspace).await?;
                let mut params = json!({"workspace_id": w.workspace_id, "name": name, "add_custom_host": add_host});
                if profile.is_some() || hosts.is_some() {
                    let presets = match profile {
                        Some(p) => parse_presets(&p)?,
                        None => w.network.presets.clone(),
                    };
                    params["network"] = serde_json::to_value(NetworkProfile {
                        presets,
                        custom_hosts: hosts.unwrap_or(w.network.custom_hosts.clone()),
                    })?;
                }
                if let Some(cs) = connectors {
                    let cs: Vec<String> = cs
                        .into_iter()
                        .filter(|c| c != "none" && !c.is_empty())
                        .collect();
                    params["connectors"] = json!(cs);
                }
                let w: Workspace = c.call(m::WORKSPACE_UPDATE, params).await?;
                if json_out {
                    print_json(&serde_json::to_value(&w)?);
                } else {
                    println!(
                        "{}: network {} {:?}, connectors [{}]",
                        w.name,
                        w.network.label(),
                        w.network.custom_hosts,
                        w.connectors.join(", ")
                    );
                }
            }
            WsCmd::Default { workspace, agent } => {
                let w = resolve_workspace(&c, &workspace).await?;
                let w = match agent.as_deref() {
                    None => w,
                    Some("none") => {
                        c.call::<Workspace>(
                            m::WORKSPACE_SET_DEFAULT_AGENT,
                            json!({"workspace_id": w.workspace_id, "agent": null}),
                        )
                        .await?
                    }
                    Some(a) => {
                        let (kind, profile) = resolve_agent(&c, a).await?;
                        c.call::<Workspace>(
                            m::WORKSPACE_SET_DEFAULT_AGENT,
                            json!({"workspace_id": w.workspace_id, "agent": kind, "profile_id": profile}),
                        )
                        .await?
                    }
                };
                match w.default_agent {
                    Some(a) => println!(
                        "{}: {}{}",
                        w.name,
                        a,
                        w.default_profile_id
                            .map(|p| format!(" (profile {p})"))
                            .unwrap_or_default()
                    ),
                    None => println!("{}: no default yet (the next session sets it)", w.name),
                }
            }
            WsCmd::Rm { workspace } => {
                let w = resolve_workspace(&c, &workspace).await?;
                c.request(m::WORKSPACE_REMOVE, json!({"workspace_id": w.workspace_id}))
                    .await?;
                println!("Removed {}", w.path);
            }
        },
        Cmd::Session(s) => match s {
            SessionCmd::List {
                workspace,
                watch,
                archived,
            } => {
                let ws = match &workspace {
                    Some(w) => Some(resolve_workspace(&c, w).await?.workspace_id),
                    None => None,
                };
                let list: Vec<Session> = c
                    .call(
                        m::SESSION_LIST,
                        json!({"workspace_id": ws, "watch": watch, "archived": archived}),
                    )
                    .await?;
                let names: Vec<Workspace> = c
                    .call(m::WORKSPACE_LIST, json!({}))
                    .await
                    .unwrap_or_default();
                if json_out {
                    print_json(&serde_json::to_value(&list)?);
                } else {
                    print_sessions(&list, &names);
                }
                if watch {
                    while let Some(n) = notes.recv().await {
                        if n.method == m::N_SESSION_CHANGED {
                            if let Ok(s) = serde_json::from_value::<Session>(n.params.clone()) {
                                if json_out {
                                    print_json(&n.params);
                                } else {
                                    println!(
                                        "{}  {:<9} {:<16} {}",
                                        s.session_id,
                                        s.agent,
                                        s.state,
                                        s.state_reason.unwrap_or_default()
                                    );
                                }
                            }
                        }
                    }
                }
            }
            SessionCmd::New { workspace, agent } => {
                let w = resolve_workspace(&c, &workspace).await?;
                let (kind, profile) = resolve_agent(&c, &agent).await?;
                let s = c
                    .create_session_as(&w.workspace_id, kind, profile.as_deref())
                    .await?;
                let s = wait_ready(&c, &mut notes, &s.session_id, json_out).await?;
                if json_out {
                    print_json(&serde_json::to_value(&s)?);
                } else {
                    println!("{}", s.session_id);
                }
            }
            SessionCmd::Show { session } => {
                c.subscribe(&session, 0).await?;
                let s = c.get_session(&session).await?;
                let mut r = Renderer::new(json_out);
                // Drain the replay up to the current last seq.
                while r.last_seq < s.last_seq {
                    match tokio::time::timeout(Duration::from_secs(5), notes.recv()).await {
                        Ok(Some(n)) => {
                            r.note(&n);
                        }
                        _ => break,
                    }
                }
            }
            SessionCmd::Prompt {
                session,
                text,
                wait,
                id,
            } => {
                let before = c.get_session(&session).await?;
                if wait {
                    c.subscribe(&session, before.last_seq).await?;
                }
                let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let r = c.prompt_with_id(&session, &id, &text).await?;
                if !r.accepted && !json_out {
                    eprintln!("(prompt {id} was already sent; not sent again)");
                }
                if wait {
                    let mut rd = Renderer::new(json_out);
                    rd.skip_prompt_seq = Some(r.seq);
                    follow_turn(&mut notes, &mut rd, r.seq).await;
                } else if json_out {
                    print_json(&serde_json::to_value(&r)?);
                }
            }
            SessionCmd::Attach { session, after } => {
                c.subscribe(&session, after).await?;
                let mut r = Renderer::new(json_out);
                while let Some(n) = notes.recv().await {
                    r.note(&n);
                }
            }
            SessionCmd::Cancel { session } => {
                c.request(m::SESSION_CANCEL, json!({"session_id": session}))
                    .await?;
            }
            SessionCmd::End { session } => {
                c.request(m::SESSION_END, json!({"session_id": session}))
                    .await?;
            }
            SessionCmd::Archive { session } => {
                c.request(m::SESSION_ARCHIVE, json!({"session_id": session}))
                    .await?;
            }
            SessionCmd::Unarchive { session } => {
                c.request(m::SESSION_UNARCHIVE, json!({"session_id": session}))
                    .await?;
            }
            SessionCmd::Config {
                session,
                config_id,
                value,
            } => {
                let s = match (config_id, value) {
                    (Some(id), Some(v)) => {
                        c.call::<Session>(
                            m::SESSION_SET_CONFIG,
                            json!({"session_id": session, "config_id": id, "value": v}),
                        )
                        .await?
                    }
                    (None, None) => c.get_session(&session).await?,
                    _ => anyhow::bail!("give both the setting and its new value, e.g. `session config {session} model <value>`"),
                };
                if json_out {
                    print_json(&serde_json::to_value(&s.config_options)?);
                } else if s.config_options.is_empty() {
                    println!("{} reports no settings for this session", s.agent.display_name());
                } else {
                    for o in &s.config_options {
                        println!("{} ({}):", o.name, o.id);
                        for v in &o.options {
                            let mark = if v.value == o.current_value { "*" } else { " " };
                            println!("  {mark} {}  {}", v.value, v.name);
                        }
                    }
                }
            }
            SessionCmd::Btw {
                session,
                question,
                side,
            } => {
                let before = c.get_session(&session).await?;
                c.subscribe(&session, before.last_seq).await?;
                let r: SessionSideAskResult = c
                    .call(
                        m::SESSION_SIDE_ASK,
                        json!({"session_id": session, "side_id": side, "text": question}),
                    )
                    .await?;
                if !json_out {
                    eprintln!("side conversation {} (continue it with --side {})", r.side_id, r.side_id);
                }
                while let Some(n) = notes.recv().await {
                    let Some(e) = as_event(&n) else { continue };
                    if e.payload["side_id"] != r.side_id.as_str() {
                        continue;
                    }
                    if json_out {
                        print_json(&serde_json::to_value(&e)?);
                    }
                    match e.kind {
                        EventType::SideMessage if !json_out => {
                            println!("{}", e.payload["text"].as_str().unwrap_or(""))
                        }
                        EventType::SideEnd => {
                            if let Some(err) = e.payload["error"].as_str() {
                                anyhow::bail!("the side question failed: {err}");
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
        },
        Cmd::Run {
            workspace,
            agent,
            prompt,
            end,
            attach,
        } => {
            let mut ids = vec![];
            for f in &attach {
                ids.push(c.upload_file(f).await?.attachment_id);
            }
            let w = resolve_workspace(&c, &workspace).await?;
            let (kind, profile) = resolve_agent(&c, &agent).await?;
            let s = c
                .create_session_as(&w.workspace_id, kind, profile.as_deref())
                .await?;
            let s = wait_ready(&c, &mut notes, &s.session_id, json_out).await?;
            if !json_out {
                eprintln!("session {}", s.session_id);
            }
            c.subscribe(&s.session_id, s.last_seq).await?;
            let prompt_id = uuid::Uuid::new_v4().to_string();
            let r = c
                .prompt_with_attachments(&s.session_id, &prompt_id, &prompt, &ids)
                .await?;
            let mut rd = Renderer::new(json_out);
            rd.skip_prompt_seq = Some(r.seq);
            follow_turn(&mut notes, &mut rd, r.seq).await;
            if end {
                c.request(m::SESSION_END, json!({"session_id": s.session_id}))
                    .await?;
            }
        }
        Cmd::Voice(v) => match v {
            VoiceCmd::Status => {
                let st: VoiceStatus = c.call(m::VOICE_STATUS, json!({})).await?;
                if json_out {
                    println!("{}", serde_json::to_string_pretty(&st)?);
                } else {
                    print_voice(&st);
                }
            }
            VoiceCmd::Download => {
                let mut st: VoiceStatus = c.call(m::VOICE_DOWNLOAD, json!({})).await?;
                while st.progress.is_some() {
                    if let Some(p) = &st.progress {
                        eprint!(
                            "\rdownloading {}: {} / {} MB   ",
                            st.model,
                            p.received / 1_000_000,
                            p.total / 1_000_000
                        );
                    }
                    match notes.recv().await {
                        Some(n) if n.method == m::N_VOICE_CHANGED => {
                            st = serde_json::from_value(n.params)?;
                        }
                        Some(_) => {}
                        None => bail!("lost the host connection"),
                    }
                }
                eprintln!();
                print_voice(&st);
                if !st.ready {
                    bail!("the speech model is not installed");
                }
            }
            VoiceCmd::Transcribe { wav, language } => {
                let data = std::fs::read(&wav).with_context(|| format!("read {}", wav.display()))?;
                let text = c.transcribe_wav(&data, language.as_deref()).await?;
                if json_out {
                    println!("{}", json!({"text": text}));
                } else {
                    println!("{text}");
                }
            }
        },
        Cmd::Chat {
            workspace,
            agent,
            session,
        } => {
            let sid = match session {
                Some(s) => s,
                None => {
                    let w = resolve_workspace(
                        &c,
                        workspace
                            .as_deref()
                            .ok_or_else(|| anyhow!("give a workspace and agent, or --session"))?,
                    )
                    .await?;
                    // No agent given: the workspace's default, else Codex.
                    let (kind, profile) = match agent.as_deref() {
                        Some(a) => resolve_agent(&c, a).await?,
                        None => match w.default_agent {
                            Some(k) => (k, w.default_profile_id.clone()),
                            None => (AgentKind::Codex, None),
                        },
                    };
                    let s = c
                        .create_session_as(&w.workspace_id, kind, profile.as_deref())
                        .await?;
                    wait_ready(&c, &mut notes, &s.session_id, false)
                        .await?
                        .session_id
                }
            };
            let s = c.get_session(&sid).await?;
            c.subscribe(&sid, 0).await?;
            let mut rd = Renderer::new(false);
            while rd.last_seq < s.last_seq {
                match tokio::time::timeout(Duration::from_secs(5), notes.recv()).await {
                    Ok(Some(n)) => {
                        rd.note(&n);
                    }
                    _ => break,
                }
            }
            eprintln!("session {sid} — type a prompt, /cancel, /end or /quit");
            let (line_tx, mut line_rx) = mpsc::channel::<String>(16);
            std::thread::spawn(move || {
                let stdin = std::io::stdin();
                for line in stdin.lock().lines().map_while(Result::ok) {
                    if line_tx.blocking_send(line).is_err() {
                        break;
                    }
                }
            });
            print!("> ");
            let _ = std::io::stdout().flush();
            loop {
                tokio::select! {
                    line = line_rx.recv() => {
                        let Some(line) = line else { break };
                        let t = line.trim();
                        match t {
                            "" => {}
                            "/quit" | "/exit" => break,
                            "/cancel" => { let _ = c.request(m::SESSION_CANCEL, json!({"session_id": sid})).await; }
                            "/end" => { let _ = c.request(m::SESSION_END, json!({"session_id": sid})).await; break; }
                            _ => {
                                let r = c.prompt(&sid, t).await?;
                                rd.skip_prompt_seq = Some(r.seq);
                            }
                        }
                    }
                    n = notes.recv() => {
                        let Some(n) = n else { break };
                        let ended = rd.note(&n);
                        if ended { print!("> "); let _ = std::io::stdout().flush(); }
                    }
                }
            }
        }
        Cmd::Approvals => {
            let list: Vec<PendingApproval> = c.call(m::APPROVAL_LIST, json!({})).await?;
            if json_out {
                print_json(&serde_json::to_value(&list)?);
            } else if list.is_empty() {
                println!("No pending approvals.");
            } else {
                for a in list {
                    println!(
                        "{}  {} / {}  session {}\n    {}",
                        a.approval_id, a.connector_name, a.tool, a.session_id, a.arguments
                    );
                }
            }
        }
        Cmd::Approve { approval_id } => {
            c.request(
                m::APPROVAL_RESPOND,
                json!({"approval_id": approval_id, "decision": "approve"}),
            )
            .await?;
            println!("Approved.");
        }
        Cmd::Deny { approval_id } => {
            c.request(
                m::APPROVAL_RESPOND,
                json!({"approval_id": approval_id, "decision": "deny"}),
            )
            .await?;
            println!("Denied.");
        }
        Cmd::Remote(_) => unreachable!(),
        Cmd::Connector(cc) => connector_cmd(&c, cc, json_out).await?,
        Cmd::Device(d) => device_cmd(&c, &mut notes, d, json_out).await?,
        Cmd::Raw { method, params } => {
            let p: Value = match params {
                Some(s) => serde_json::from_str(&s)?,
                None => json!({}),
            };
            let v = c.request(&method, p).await.map_err(|e| anyhow!(e))?;
            print_json(&v);
        }
    }
    Ok(())
}

async fn connector_cmd(c: &Client, cmd: ConnCmd, json_out: bool) -> Result<()> {
    match cmd {
        ConnCmd::List => {
            let list: Vec<Connector> = c.call(m::CONNECTOR_LIST, json!({})).await?;
            if json_out {
                print_json(&serde_json::to_value(&list)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("No connectors. Add one with: agenthost connector add <url>");
            }
            for k in list {
                println!(
                    "{}  {:<16} {:<14} {}  read:{:?} write:{:?}{}",
                    k.connector_id,
                    k.name,
                    k.status,
                    k.url,
                    k.read_policy,
                    k.write_policy,
                    if k.enabled { "" } else { "  (disabled)" }
                );
                if let Some(d) = k.status_detail {
                    println!("    {d}");
                }
                for t in k.tools {
                    println!(
                        "    {:<5} {:<9} {}",
                        format!("{:?}", t.kind).to_lowercase(),
                        format!("{:?}", t.policy).to_lowercase(),
                        t.name
                    );
                }
            }
        }
        ConnCmd::Add {
            url,
            name,
            auth,
            token,
        } => {
            let token = match token.as_deref() {
                Some("-") => {
                    let mut s = String::new();
                    std::io::stdin().read_line(&mut s)?;
                    Some(s.trim().to_string())
                }
                t => t.map(str::to_string),
            };
            let k: Connector = c
                .call(
                    m::CONNECTOR_ADD,
                    json!({"url": url, "name": name, "auth": auth, "token": token}),
                )
                .await?;
            if json_out {
                print_json(&serde_json::to_value(&k)?);
            } else {
                println!(
                    "Added {} as {} ({}, {} tools)",
                    k.url,
                    k.connector_id,
                    k.status,
                    k.tools.len()
                );
                if k.status == "needs_sign_in" {
                    sign_in(c, &k.connector_id).await?;
                }
            }
        }
        ConnCmd::Set {
            connector,
            name,
            enabled,
            write_policy,
            read_policy,
            tools,
        } => {
            let mut tp = serde_json::Map::new();
            for t in tools {
                let (n, p) = t
                    .split_once('=')
                    .ok_or_else(|| anyhow!("--tool takes name=policy"))?;
                tp.insert(
                    n.into(),
                    if p == "default" {
                        Value::Null
                    } else {
                        serde_json::to_value(parse_policy(p)?)?
                    },
                );
            }
            let params = json!({
                "connector_id": connector,
                "name": name,
                "enabled": enabled,
                "write_policy": write_policy.map(|p| parse_policy(&p)).transpose()?,
                "read_policy": read_policy.map(|p| parse_policy(&p)).transpose()?,
                "tool_policies": if tp.is_empty() { Value::Null } else { Value::Object(tp) },
            });
            let k: Connector = c.call(m::CONNECTOR_UPDATE, params).await?;
            if json_out {
                print_json(&serde_json::to_value(&k)?);
            } else {
                println!("Updated {}", k.name);
            }
        }
        ConnCmd::SignIn { connector } => sign_in(c, &connector).await?,
        ConnCmd::Refresh { connector } => {
            let k: Connector = c
                .call(m::CONNECTOR_REFRESH, json!({"connector_id": connector}))
                .await?;
            println!("{}: {} ({} tools)", k.name, k.status, k.tools.len());
        }
        ConnCmd::Rm { connector } => {
            c.request(m::CONNECTOR_REMOVE, json!({"connector_id": connector}))
                .await
                .map_err(|e| anyhow!(e))?;
            println!("Removed.");
        }
    }
    Ok(())
}

/// Open the connector's OAuth sign-in in the browser and wait for it to finish.
async fn sign_in(c: &Client, connector: &str) -> Result<()> {
    let r: ConnectorSignInResult = c
        .call(m::CONNECTOR_SIGN_IN, json!({"connector_id": connector}))
        .await?;
    let Some(u) = r.authorize_url else {
        println!("{}", r.status);
        return Ok(());
    };
    println!("Opening your browser to sign in. If it doesn't open, visit:\n\n  {u}\n");
    let _ = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(&u)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn();
    println!("Waiting for sign-in to finish…");
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let list: Vec<Connector> = c.call(m::CONNECTOR_LIST, json!({})).await?;
        if let Some(k) = list.iter().find(|k| k.connector_id == connector) {
            if k.status == "ok" {
                println!("Signed in to {}. {} tools.", k.name, k.tools.len());
                return Ok(());
            }
            if k.status == "error" {
                bail!(
                    "sign-in failed: {}",
                    k.status_detail.clone().unwrap_or_default()
                );
            }
        }
    }
    bail!("timed out waiting for sign-in")
}

async fn device_cmd(
    c: &Client,
    notes: &mut mpsc::Receiver<Notification>,
    cmd: DeviceCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        DeviceCmd::List => {
            let list: Vec<Device> = c.call(m::DEVICE_LIST, json!({})).await?;
            if json_out {
                print_json(&serde_json::to_value(&list)?);
            } else if list.is_empty() {
                println!("No paired devices. Pair one with: agenthost device pair");
            } else {
                for d in list {
                    let seen = d.last_seen_at.map(ago).unwrap_or_else(|| "never".into());
                    println!("{}  {:<24} last seen {}", d.device_id, d.name, seen);
                }
            }
        }
        DeviceCmd::Pair => {
            let r: PairStartResult = c.call(m::DEVICE_PAIR_START, json!({})).await?;
            if json_out {
                print_json(&serde_json::to_value(&r)?);
            } else {
                print_qr(&r.qr_payload);
                println!("Scan this with the app, or paste this into the desktop app:\n\n  {}\n", r.qr_payload);
                println!("It expires in 2 minutes.\n");
            }
            loop {
                let Some(n) = notes.recv().await else {
                    bail!("connection closed")
                };
                if n.method == m::N_PAIRING_PENDING {
                    let p: PairingPending = serde_json::from_value(n.params)?;
                    if p.pairing_id != r.pairing_id {
                        continue;
                    }
                    println!(
                        "Device \"{}\" wants to pair.\nCode on the phone should read:  {}\n",
                        p.device_name, p.short_code
                    );
                    print!("Does it match? [y/N] ");
                    std::io::stdout().flush()?;
                    let ans = tokio::task::spawn_blocking(|| {
                        let mut s = String::new();
                        std::io::stdin().read_line(&mut s).ok();
                        s
                    })
                    .await?;
                    let accept = matches!(ans.trim(), "y" | "Y" | "yes");
                    c.request(
                        m::DEVICE_PAIR_CONFIRM,
                        json!({"pairing_id": p.pairing_id, "accept": accept}),
                    )
                    .await
                    .map_err(|e| anyhow!(e))?;
                    if !accept {
                        println!("Refused.");
                        return Ok(());
                    }
                } else if n.method == m::N_PAIRING_DONE
                    && n.params["pairing_id"] == json!(r.pairing_id)
                {
                    if n.params["ok"] == json!(true) {
                        println!(
                            "Paired {} ({}).",
                            n.params["device_name"].as_str().unwrap_or("device"),
                            n.params["device_id"].as_str().unwrap_or("")
                        );
                    } else {
                        println!(
                            "Pairing ended: {}",
                            n.params["reason"].as_str().unwrap_or("refused")
                        );
                    }
                    return Ok(());
                }
            }
        }
        DeviceCmd::Rename { device, name } => {
            c.request(m::DEVICE_RENAME, json!({"device_id": device, "name": name}))
                .await
                .map_err(|e| anyhow!(e))?;
            println!("Renamed.");
        }
        DeviceCmd::Revoke { device } => {
            c.request(m::DEVICE_REVOKE, json!({"device_id": device}))
                .await
                .map_err(|e| anyhow!(e))?;
            println!("Revoked {device}. It has been disconnected and cannot reconnect.");
        }
    }
    Ok(())
}

fn print_qr(payload: &str) {
    match qrcode::QrCode::new(payload.as_bytes()) {
        Ok(code) => {
            let s = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build();
            println!("{s}");
        }
        Err(_) => println!("{payload}"),
    }
}

fn ago(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let s = (now - ts) / 1000;
    match s {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

fn print_workspace_added(w: &Workspace, json_out: bool) -> Result<()> {
    if json_out {
        print_json(&serde_json::to_value(w)?);
    } else {
        println!("{}  {}  {}", w.workspace_id, w.name, w.path);
    }
    Ok(())
}

/// One line per login; profiles show their name (use it as agent:name) and folder.
fn print_agents(agents: &[AgentStatus]) {
    for a in agents {
        let v = a.version.clone().unwrap_or_default();
        let name = match &a.profile_name {
            Some(n) => format!("{}:{n}", a.agent),
            None => a.agent.to_string(),
        };
        let state = if a.available {
            "ready".to_string()
        } else {
            format!(
                "{} — fix: {}",
                a.problem.clone().unwrap_or_default(),
                a.fix.clone().unwrap_or_default()
            )
        };
        println!("  {name:<18} {v:<10} {state}");
        if let Some(dir) = &a.config_dir {
            println!("  {:<18} {:<10} {dir}", "", "");
        }
    }
}

fn print_status(info: &HostInfo) {
    println!("host {}  v{}  {}", info.host_id, info.version, info.os);
    let sb = &info.sandbox;
    println!(
        "\nsandbox: {} {}",
        sb.backend,
        if sb.ok { "ok" } else { "NOT READY" }
    );
    for p in &sb.prerequisites {
        if !p.present {
            println!(
                "  missing {:<16} install: {}",
                p.name,
                p.install.clone().unwrap_or_default()
            );
        }
    }
    println!("\nagents:");
    print_agents(&info.agents);
    let r = &info.relay;
    println!(
        "\nrelay: {}",
        if !r.configured {
            "not configured".to_string()
        } else if r.connected {
            format!("connected ({})", r.url.clone().unwrap_or_default())
        } else {
            format!(
                "disconnected ({}) {}",
                r.url.clone().unwrap_or_default(),
                r.error.clone().unwrap_or_default()
            )
        }
    );
}

fn print_sessions(list: &[Session], ws: &[Workspace]) {
    if list.is_empty() {
        println!("No sessions.");
        return;
    }
    let mut by_ws: Vec<(&str, Vec<&Session>)> = vec![];
    for s in list {
        match by_ws.iter_mut().find(|(w, _)| *w == s.workspace_id) {
            Some((_, v)) => v.push(s),
            None => by_ws.push((&s.workspace_id, vec![s])),
        }
    }
    for (w, ss) in by_ws {
        let name = ws
            .iter()
            .find(|x| x.workspace_id == w)
            .map(|x| x.name.as_str())
            .unwrap_or(w);
        println!("{name}");
        for s in ss {
            println!(
                "  {}  {:<9} {:<16} {}  {}",
                s.session_id,
                s.agent.as_str(),
                s.state.as_str(),
                ago(s.updated_at),
                s.state_reason.clone().unwrap_or_default()
            );
        }
    }
}

/// Wait for a new session to leave `created`/`starting` (polls; does not subscribe).
async fn wait_ready(
    c: &Client,
    notes: &mut mpsc::Receiver<Notification>,
    sid: &str,
    _quiet: bool,
) -> Result<Session> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let s = c.get_session(sid).await?;
        match s.state {
            SessionState::Idle | SessionState::Running | SessionState::WaitingApproval => {
                return Ok(s)
            }
            SessionState::Failed => {
                // The last event is the failure; show its fix and stderr.
                let mut extra = String::new();
                if c.subscribe(sid, s.last_seq - 1).await.is_ok() {
                    if let Ok(Some(n)) =
                        tokio::time::timeout(Duration::from_secs(2), notes.recv()).await
                    {
                        if let Some(e) = as_event(&n) {
                            if let Some(fix) = e.payload["fix"].as_str() {
                                extra.push_str(&format!("\n  fix: {fix}"));
                            }
                            if let Some(se) = e.payload["stderr"].as_str().filter(|s| !s.is_empty())
                            {
                                extra.push_str(&format!(
                                    "\n  agent output:\n    {}",
                                    first_lines(se, 15).replace('\n', "\n    ")
                                ));
                            }
                        }
                    }
                }
                bail!(
                    "session failed: {}{extra}",
                    s.state_reason.unwrap_or_default()
                );
            }
            SessionState::Ended => bail!("session ended"),
            _ => {}
        }
        if tokio::time::Instant::now() > deadline {
            bail!("session did not start within 120s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Print notifications until the turn started by `prompt_seq` ends.
async fn follow_turn(notes: &mut mpsc::Receiver<Notification>, rd: &mut Renderer, prompt_seq: i64) {
    while let Some(n) = notes.recv().await {
        rd.note(&n);
        if let Some(e) = as_event(&n) {
            if e.kind == EventType::TurnEnd
                && e.payload["prompt_seq"]
                    .as_i64()
                    .map(|s| s >= prompt_seq)
                    .unwrap_or(true)
            {
                break;
            }
            if e.kind == EventType::SessionState
                && matches!(e.payload["state"].as_str(), Some("failed") | Some("ended"))
            {
                break;
            }
        }
    }
}

/// Turns events into a readable transcript.
struct Renderer {
    json: bool,
    streamed: String,
    last_seq: i64,
    skip_prompt_seq: Option<i64>,
}

impl Renderer {
    fn new(json: bool) -> Self {
        Self {
            json,
            streamed: String::new(),
            last_seq: 0,
            skip_prompt_seq: None,
        }
    }

    /// Returns true when a turn ended.
    fn note(&mut self, n: &Notification) -> bool {
        if self.json {
            println!(
                "{}",
                serde_json::to_string(&json!({"method": n.method, "params": n.params})).unwrap()
            );
            if let Some(e) = as_event(n) {
                self.last_seq = e.seq;
                return e.kind == EventType::TurnEnd;
            }
            return false;
        }
        if n.method == m::N_SESSION_DELTA {
            if let Ok(d) = serde_json::from_value::<SessionDelta>(n.params.clone()) {
                if d.kind == EventType::AgentMessage {
                    print!("{}", d.text);
                    self.streamed.push_str(&d.text);
                    let _ = std::io::stdout().flush();
                }
            }
            return false;
        }
        if n.method == m::N_DEVICE_REVOKED {
            println!("This device was removed.");
            return false;
        }
        let Some(e) = as_event(n) else { return false };
        self.last_seq = e.seq;
        let p = &e.payload;
        match e.kind {
            EventType::Prompt => {
                if self.skip_prompt_seq != Some(e.seq) {
                    println!("\n> {}", p["text"].as_str().unwrap_or(""));
                }
            }
            EventType::AgentMessage => {
                let text = p["text"].as_str().unwrap_or("");
                if !self.streamed.is_empty() && text.starts_with(self.streamed.as_str()) {
                    print!("{}", &text[self.streamed.len()..]);
                    println!();
                } else {
                    if !self.streamed.is_empty() {
                        println!();
                    }
                    println!("{text}");
                }
                self.streamed.clear();
            }
            EventType::SideQuestion => {
                println!("\n> /btw {}", p["text"].as_str().unwrap_or(""));
            }
            EventType::SideMessage => {
                println!("\x1b[2m(btw) {}\x1b[0m", p["text"].as_str().unwrap_or(""));
            }
            EventType::SideEnd => {}
            EventType::AgentFile => {
                if let Ok(f) = serde_json::from_value::<AgentFile>(p.clone()) {
                    let what = if f.kind == AgentFileKind::Recording { "a recording" } else { "a file" };
                    let kb = f.attachment.size.div_ceil(1024);
                    println!("[sent you {what}] {} ({kb} KB, {})", f.attachment.name, f.attachment.attachment_id);
                    if let Some(c) = f.caption.filter(|c| !c.is_empty()) {
                        println!("  {}", first_lines(&c, 3));
                    }
                }
            }
            EventType::SubagentMessage => {
                // A subagent's words, under its call (one level: subagents
                // don't start subagents).
                let t = first_lines(p["text"].as_str().unwrap_or(""), 3);
                println!("\x1b[2m    │ {}\x1b[0m", t.replace('\n', "\n    │ "));
            }
            EventType::AgentThought => {
                let t = p["text"].as_str().unwrap_or("");
                println!("\x1b[2m(thinking) {}\x1b[0m", first_lines(t, 3));
            }
            EventType::ToolCall => {
                let args = match &p["arguments"] {
                    Value::Null => String::new(),
                    v => first_lines(&v.to_string(), 1),
                };
                let src = if p["source"] == "mcp" { " [mcp]" } else { "" };
                let indent = if p["parent_tool_call_id"].is_string() { "    " } else { "" };
                if let Some(sub) = p["subagent"].as_object() {
                    let what = sub
                        .get("description")
                        .and_then(Value::as_str)
                        .or(p["name"].as_str())
                        .unwrap_or("subagent");
                    let kind = sub
                        .get("agent_type")
                        .and_then(Value::as_str)
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default();
                    println!("{indent}\x1b[35m⤷ subagent: {what}{kind}\x1b[0m");
                } else {
                    println!(
                        "{indent}\x1b[36m⚙ {}{src}\x1b[0m {}",
                        p["name"].as_str().unwrap_or("tool"),
                        truncate_str(&args, 160)
                    );
                }
            }
            EventType::ToolResult => {
                let out = p["output"].as_str().unwrap_or("");
                let mark = if p["status"] == "completed" {
                    "✓"
                } else {
                    "✗"
                };
                let denied = if p["sandbox_denied"] == true {
                    " (blocked by the sandbox)"
                } else {
                    ""
                };
                let indent = if p["parent_tool_call_id"].is_string() { "    " } else { "" };
                println!(
                    "{indent}  {mark} {}{denied}",
                    truncate_str(&first_lines(out, 4), 400).replace('\n', &format!("\n{indent}    "))
                );
            }
            EventType::NetDenied => {
                let host = p["host"].as_str().unwrap_or("?");
                println!(
                    "\x1b[33m⛔ network: {host}:{} denied — {}\x1b[0m\n   allow it with: agenthost workspace set {} --add-host {host}",
                    p["port"],
                    p["reason"].as_str().unwrap_or(""),
                    p["workspace_id"].as_str().unwrap_or("<workspace>")
                );
            }
            EventType::ApprovalRequested => {
                println!(
                    "\x1b[35m⏸ approval needed: {} / {}  {}\x1b[0m\n   agenthost approve {}   |   agenthost deny {}",
                    p["connector_name"].as_str().unwrap_or(""),
                    p["tool"].as_str().unwrap_or(""),
                    truncate_str(&p["arguments"].to_string(), 200),
                    p["approval_id"].as_str().unwrap_or(""),
                    p["approval_id"].as_str().unwrap_or("")
                );
            }
            EventType::ApprovalResolved if p["withdrawn"] == true => {
                println!("  approval withdrawn: the agent stopped waiting");
            }
            EventType::ApprovalResolved => {
                println!(
                    "  approval {} by {}",
                    p["decision"].as_str().unwrap_or(""),
                    p["device"].as_str().unwrap_or("host")
                );
            }
            EventType::SessionState => {
                let state = p["state"].as_str().unwrap_or("");
                let reason = p["reason"]
                    .as_str()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default();
                if matches!(state, "failed" | "ended" | "starting" | "waiting_approval") {
                    println!("\x1b[2m[{state}{reason}]\x1b[0m");
                    if let Some(fix) = p["fix"].as_str() {
                        println!("   fix: {fix}");
                    }
                    if let Some(se) = p["stderr"].as_str().filter(|s| !s.is_empty()) {
                        println!("\x1b[2m{}\x1b[0m", first_lines(se, 12));
                    }
                }
            }
            EventType::TurnEnd => {
                if !self.streamed.is_empty() {
                    println!();
                    self.streamed.clear();
                }
                let sr = p["stop_reason"].as_str().unwrap_or("");
                if sr != "end_turn" {
                    let hr = p["host_reason"]
                        .as_str()
                        .map(|r| format!(" — {r}"))
                        .unwrap_or_default();
                    println!("\x1b[2m[turn {sr}{hr}]\x1b[0m");
                }
                return true;
            }
        }
        false
    }
}

fn first_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut out = lines.iter().take(n).cloned().collect::<Vec<_>>().join("\n");
    if lines.len() > n {
        out.push_str(&format!(" … (+{} lines)", lines.len() - n));
    }
    out
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// `x,y` as fractions of the screen.
fn parse_point(s: &str) -> Result<(f64, f64), String> {
    let (x, y) = s.split_once(',').ok_or("expected x,y, e.g. 0.5,0.5")?;
    let f = |v: &str| v.trim().parse::<f64>().map_err(|_| format!("{v:?} is not a number"));
    Ok((f(x)?, f(y)?))
}

async fn screen_cmd(
    c: &Client,
    cmd: ScreenCmd,
    remote: Option<&std::path::Path>,
    json_out: bool,
) -> Result<()> {
    use agenthost_proto::screen::{self as wire, Input, ServerMessage};
    let st: ScreenStatus = match cmd {
        ScreenCmd::Status => c.call(m::SCREEN_STATUS, json!({})).await?,
        ScreenCmd::Allow { control } => {
            c.call(m::SCREEN_SET_POLICY, json!({"enabled": true, "allow_control": control}))
                .await?
        }
        ScreenCmd::Off => c.call(m::SCREEN_SET_POLICY, json!({"enabled": false})).await?,
        ScreenCmd::Stop => c.call(m::SCREEN_STOP, json!({})).await?,
        ScreenCmd::Record { out, mode, output, seconds, click, r#type } => {
            let Some(state) = remote else {
                bail!("record watches as a paired device would: add --remote <state file>");
            };
            let state = agenthost_cli::remote::DeviceState::load(state)?;
            let control = click.is_some() || r#type.is_some();
            let start: ScreenStartResult = c
                .call(m::SCREEN_START, json!({"mode": mode, "control": control, "output": output}))
                .await?;
            let mut s = agenthost_cli::remote::open_screen(&state, &start.ticket).await?;
            let mut file = std::fs::File::create(&out)?;
            let t0 = std::time::Instant::now();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
            let (mut frames, mut bytes, mut size, mut acted) = (0u64, 0u64, (0, 0), !control);
            loop {
                let msg = tokio::select! {
                    m = s.incoming.recv() => m,
                    _ = tokio::time::sleep_until(deadline) => break,
                };
                let Some(msg) = msg else {
                    bail!("the stream ended: {}", s.closed.borrow().clone().unwrap_or_default());
                };
                let ServerMessage::Frame(f) = wire::decode(&msg)? else { continue };
                use std::io::Write;
                file.write_all(&f.data)?;
                s.send(wire::ack(f.generation, f.sequence, 0)).await?;
                frames += 1;
                bytes += f.data.len() as u64;
                size = (f.width, f.height);
                if !acted {
                    acted = true;
                    let frac = |v: f64| (v.clamp(0.0, 1.0) * wire::COORD_MAX as f64) as u16;
                    if let Some((px, py)) = click {
                        let (x, y) = (frac(px), frac(py));
                        for i in [
                            Input::PointerMove { x, y },
                            Input::Button { button: 1, down: true, x, y },
                            Input::Button { button: 1, down: false, x, y },
                        ] {
                            s.send(i.message(f.generation)).await?;
                        }
                    }
                    for ch in r#type.iter().flat_map(|t| t.chars()) {
                        s.send(Input::Text(ch).message(f.generation)).await?;
                    }
                }
            }
            let secs = t0.elapsed().as_secs_f64();
            let summary = json!({
                "file": out.display().to_string(), "frames": frames, "width": size.0, "height": size.1,
                "seconds": secs, "kbps": (bytes as f64 * 8.0 / 1000.0 / secs).round(), "control": start.control,
                "output": start.output,
            });
            if json_out {
                print_json(&summary);
            } else {
                println!(
                    "Saved {frames} frames ({}x{}, {} kbps average) to {}. Play it with: ffplay {}",
                    size.0, size.1, summary["kbps"], out.display(), out.display()
                );
            }
            return Ok(());
        }
    };
    if json_out {
        print_json(&serde_json::to_value(&st)?);
        return Ok(());
    }
    let p = st.policy;
    println!(
        "Phones: {}",
        match (p.enabled, p.allow_control) {
            (false, _) => "not allowed (agenthost screen allow)",
            (true, false) => "may watch",
            (true, true) => "may watch, click and type",
        }
    );
    for mode in &st.modes {
        let name = match mode.mode {
            ScreenMode::Desktop => "Your desktop",
            ScreenMode::Browser => "Browser on host",
        };
        println!("  {} {name}: {}", if mode.available { "✓" } else { "✗" }, mode.detail);
    }
    if !st.outputs.is_empty() {
        println!("Monitors (record --output NAME; the first is the default):");
        for o in &st.outputs {
            let about = if o.description.is_empty() { String::new() } else { format!(", {}", o.description) };
            let focus = if o.focused { ", focused" } else { "" };
            println!("  {}: {}x{}{about}{focus}", o.name, o.width, o.height);
        }
    }
    match &st.running {
        Some(r) => println!(
            "Running: {:?}{}{}, started by {} {}, {} viewer(s)",
            r.mode,
            r.output.as_ref().map(|o| format!(" on {o}")).unwrap_or_default(),
            if r.control { " with control" } else { ", view only" },
            r.started_by,
            ago(r.started_at),
            r.viewers
        ),
        None => println!("Not running."),
    }
    if let Some(e) = &st.error {
        println!("Last error: {e}");
    }
    Ok(())
}

fn print_voice(st: &VoiceStatus) {
    let state = if st.ready {
        "installed".to_string()
    } else if let Some(p) = &st.progress {
        format!("downloading ({} of {} MB)", p.received / 1_000_000, p.total / 1_000_000)
    } else {
        format!(
            "not installed ({} MB download): run `agenthost voice download`",
            st.download_bytes / 1_000_000
        )
    };
    println!("{}: {state}", st.model);
    if let Some(e) = &st.error {
        println!("  {e}");
    }
}
