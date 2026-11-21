//! Automations: saved sessions that start on a schedule, from a webhook, or
//! by hand. Each run is an ordinary session (its transcript is the run's
//! output), recorded as an [`AutomationRun`].

use crate::types::AgentKind;
use serde::{Deserialize, Serialize};

/// Host tools an automation may let its runs use without asking.
pub const AUTO_APPROVABLE: &[&str] = &["create_pull_request", "git_push"];

/// Longest prompt an automation may hold.
pub const MAX_AUTOMATION_PROMPT: usize = 20_000;

/// The prompt new Sentry automations start with.
pub const SENTRY_PROMPT: &str = "A Sentry alert fired: {{sentry.title}}\nRule: {{sentry.rule}}\nIn Sentry: {{sentry.url}}\n\n\
Investigate it with the Sentry connector: read the issue, its latest events, stack trace and breadcrumbs, then find the cause \
in this repository. Write up what you found: the cause, how sure you are, and the fix. If the fix is small and clear, make it \
on this branch with a test, commit it, push the branch and open a pull request.";

/// The prompt new GitHub webhook automations start with.
pub const GITHUB_PROMPT: &str = "GitHub sent a {{github.event}} event ({{github.action}}). Its payload is attached as webhook.json.\n\n\
Look at what happened and do what it needs.";

/// The prompt new generic webhook automations start with.
pub const GENERIC_PROMPT: &str = "A webhook arrived. Its payload is attached as webhook.json.\n\nLook at it and do what it needs.";

/// Prompts that suit maintenance on a schedule, offered when creating one.
pub const SCHEDULE_EXAMPLES: &[(&str, &str)] = &[
    (
        "Upgrade dependencies",
        "Upgrade this project's dependencies to their latest compatible versions. Run the build and the tests; fix what breaks. \
If everything passes, commit on this branch, push it and open a pull request listing what changed. If an upgrade needs \
real work, leave it out and say why.",
    ),
    (
        "Routine maintenance",
        "Do a maintenance pass on this repository: run the tests and linters, fix warnings and small problems you find, and \
remove dead code you're sure is unused. Commit on this branch, push it and open a pull request with a short summary.",
    ),
];

/// What starts an automation's runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationTrigger {
    /// Only "Run Now".
    Manual,
    /// A five-field cron expression (`minute hour day-of-month month
    /// day-of-week`) in the host's local time, or `@hourly`, `@daily`,
    /// `@weekly`, `@monthly`.
    Schedule { cron: String },
    /// A POST to the automation's webhook URL on the relay.
    Webhook {
        source: WebhookSource,
        /// Every rule must hold for a delivery to start a run.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        filter: Vec<WebhookMatch>,
        /// A template (`{{payload.issue.id}}`); a delivery whose key matches a
        /// run started within `cooldown_minutes` starts nothing. Left out:
        /// Sentry dedupes per issue or metric alert (`{{sentry.key}}`), other
        /// sources don't dedupe.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dedupe_key: Option<String>,
        #[serde(default = "default_cooldown")]
        cooldown_minutes: u32,
    },
}

fn default_cooldown() -> u32 {
    24 * 60
}

impl AutomationTrigger {
    pub fn kind(&self) -> &'static str {
        match self {
            AutomationTrigger::Manual => "manual",
            AutomationTrigger::Schedule { .. } => "schedule",
            AutomationTrigger::Webhook { .. } => "webhook",
        }
    }
}

/// Who sends a webhook, which decides how its signature is checked and
/// which fields its templates can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookSource {
    /// Sentry integration platform: `Sentry-Hook-Signature` is an
    /// HMAC-SHA256 of the body with the integration's client secret.
    /// Templates get `sentry.*` (title, url, issue_id, rule, resource,
    /// action, key); only new alerts start runs (not resolved ones).
    Sentry,
    /// GitHub: `X-Hub-Signature-256: sha256=<HMAC-SHA256 of the body>`.
    /// Templates get `github.event` and `github.action`.
    Github,
    /// Anything else. With a secret, the request must carry
    /// `Authorization: Bearer <secret>`; without one, the URL is the secret.
    Generic,
}

impl WebhookSource {
    pub fn display_name(&self) -> &'static str {
        match self {
            WebhookSource::Sentry => "Sentry",
            WebhookSource::Github => "GitHub",
            WebhookSource::Generic => "Webhook",
        }
    }
}

/// `path` (`payload.action`, `headers.x-github-event`, `sentry.resource`)
/// must equal `equals` (compared as text).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookMatch {
    pub path: String,
    pub equals: String,
}

/// An automation's webhook endpoint, as clients show it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookInfo {
    /// The URL to give the sender; `None` when the host has no relay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A signing secret is stored (it is never sent back).
    pub has_secret: bool,
    /// The last delivery: when, and what became of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<WebhookDelivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookDelivery {
    pub at: i64,
    /// `started`, `ignored` (filter, dedupe, busy), `refused` (bad signature).
    pub result: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Automation {
    pub automation_id: String,
    pub name: String,
    pub enabled: bool,
    pub workspace_id: String,
    pub agent: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// What each run is asked. Webhook automations may use `{{…}}` fields.
    pub prompt: String,
    pub trigger: AutomationTrigger,
    /// Host tools (from [`AUTO_APPROVABLE`]) its runs use without asking:
    /// `create_pull_request`, and `git_push` of the run's own branch (a
    /// force-push still asks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_approve: Vec<String>,
    /// A run still going after this long is stopped.
    pub max_minutes: u32,
    pub created_at: i64,
    pub updated_at: i64,
    /// The schedule in words ("Every day at 09:00").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_text: Option<String>,
    /// When the schedule fires next (enabled schedules only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<AutomationRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<WebhookInfo>,
}

/// Why a run started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    Schedule,
    /// A scheduled time passed while the host was off or asleep; run once on return.
    CatchUp,
    Webhook,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Running,
    /// The agent finished its turn.
    Succeeded,
    /// The agent stopped with an error or the session failed.
    Failed,
    Cancelled,
    /// Stopped after `max_minutes`.
    TimedOut,
    /// Never started: the previous run was still going, or the time was
    /// missed by too much.
    Skipped,
}

impl RunOutcome {
    pub fn is_done(&self) -> bool {
        *self != RunOutcome::Running
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRun {
    pub run_id: String,
    pub automation_id: String,
    /// The run's session; `None` for a skipped run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub trigger: RunTrigger,
    /// The time the schedule asked for (schedule and catch-up runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<i64>,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    pub outcome: RunOutcome,
    /// The agent's last reply, shortened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Why it was skipped or failed; for webhook runs, what fired it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `automation.create`. A webhook trigger or a non-empty `auto_approve`
/// needs the desktop (admin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationCreateParams {
    pub name: String,
    pub workspace_id: String,
    pub agent: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub prompt: String,
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_approve: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minutes: Option<u32>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The sender's signing secret (Sentry's client secret, GitHub's
    /// webhook secret, a bearer token). Write-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
}

fn yes() -> bool {
    true
}

/// `automation.update`: only the fields given change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AutomationUpdateParams {
    pub automation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    /// `Some("")` (or the agent's own name) for the default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<AutomationTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minutes: Option<u32>,
    /// `Some("")` removes the stored secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    /// Give the webhook a new URL (the old one stops working).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rotate_webhook: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRef {
    pub automation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRunsParams {
    pub automation_id: String,
    /// Newest first; at most this many (default 50, at most 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only runs that started before this time (for paging).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
}

/// `automation.changed`: an automation was created, changed, or a run of it
/// started or finished (`automation`), or it was deleted (`removed`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationChanged {
    pub automation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<Automation>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

/// `automation.check_schedule`: whether a cron expression is valid, in words,
/// and its next few times (so clients can show them while editing).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleCheckParams {
    pub cron: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trigger_shapes() {
        let t: AutomationTrigger = serde_json::from_value(json!({"kind":"schedule","cron":"0 9 * * 1-5"})).unwrap();
        assert_eq!(t, AutomationTrigger::Schedule { cron: "0 9 * * 1-5".into() });
        let t: AutomationTrigger = serde_json::from_value(json!({"kind":"webhook","source":"sentry"})).unwrap();
        assert_eq!(
            t,
            AutomationTrigger::Webhook { source: WebhookSource::Sentry, filter: vec![], dedupe_key: None, cooldown_minutes: 1440 }
        );
        assert_eq!(serde_json::to_value(AutomationTrigger::Manual).unwrap(), json!({"kind":"manual"}));
    }
}
