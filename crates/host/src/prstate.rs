//! Where sessions' pull requests stand: open, merged or closed.
//!
//! Every few minutes the host asks GitHub, through the user's own GitHub CLI
//! (`gh pr view <url> --json state`), about each pull request it hasn't
//! seen merged or closed, and tells the clients when one changed (the
//! session list colours its pull request icon by it). Archived sessions
//! aren't checked. Without `gh`, or offline, states stay as they were.

use crate::bus::BusMsg;
use crate::ctx::Ctx;
use agenthost_proto::{PullRequestState, Session};
use std::sync::Arc;
use std::time::Duration;

/// How often open pull requests are checked.
const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
/// The first check, a little after the host starts.
const FIRST_CHECK: Duration = Duration::from_secs(15);
/// Longest one `gh pr view` may take.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PrStates {
    ctx: Arc<Ctx>,
}

impl PrStates {
    pub fn new(ctx: Arc<Ctx>) -> Arc<Self> {
        Arc::new(Self { ctx })
    }

    pub fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_CHECK).await;
            loop {
                me.check().await;
                tokio::time::sleep(CHECK_EVERY).await;
            }
        });
    }

    /// Check every pull request that may still change once; returns how
    /// many changed.
    pub async fn check(&self) -> usize {
        let Some(gh) = crate::installer::find("gh", self.ctx.cfg.install_bin_dir.as_deref()) else { return 0 };
        let mut changed = 0;
        for s in self.ctx.store.session_list(None).unwrap_or_default() {
            let Some(pr) = due(&s) else { continue };
            let state = match view(&gh, &self.ctx.cfg.data_dir, &pr.url).await {
                Ok(state) => state,
                Err(e) => {
                    tracing::debug!("pull request {}: {e}", pr.url);
                    continue;
                }
            };
            if pr.state == Some(state) {
                continue;
            }
            if let Ok(true) = self.ctx.store.session_set_pull_request_state(&s.session_id, &pr.url, state) {
                changed += 1;
                if let Ok(Some(s)) = self.ctx.store.session_get(&s.session_id) {
                    self.ctx.bus.publish(BusMsg::SessionChanged(s));
                }
            }
        }
        changed
    }
}

/// The session's pull request, when it is one to check: not archived, and
/// not yet seen merged or closed.
fn due(s: &Session) -> Option<&agenthost_proto::SessionPullRequest> {
    let pr = s.pull_request.as_ref()?;
    let settled = matches!(pr.state, Some(PullRequestState::Merged | PullRequestState::Closed));
    (s.archived_at.is_none() && !settled).then_some(pr)
}

/// `gh pr view <url> --json state --jq .state`, as the user, outside any
/// sandbox, never prompting.
async fn view(gh: &std::path::Path, dir: &std::path::Path, url: &str) -> Result<PullRequestState, String> {
    // Only links the host recorded itself, but never something gh could
    // read as a flag.
    if !url.starts_with("https://") {
        return Err("not a pull request link".into());
    }
    let mut cmd = tokio::process::Command::new(gh);
    cmd.args(["pr", "view", url, "--json", "state", "--jq", ".state"])
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(GH_TIMEOUT, cmd.output()).await {
        Err(_) => return Err("gh took too long".into()),
        Ok(Err(e)) => return Err(format!("could not run gh: {e}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    PullRequestState::from_gh(&text).ok_or_else(|| format!("gh said {:?}", text.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenthost_proto::SessionPullRequest;

    #[test]
    fn only_open_or_unknown_unarchived_pull_requests_are_checked() {
        let mut s: Session = serde_json::from_value(serde_json::json!({
            "session_id": "ses_1", "workspace_id": "ws", "agent": "fake", "state": "idle",
            "created_at": 0, "updated_at": 0, "last_seq": 0
        }))
        .unwrap();
        assert!(due(&s).is_none());
        let pr = |state| SessionPullRequest { number: 1, url: "https://github.com/o/r/pull/1".into(), state };
        for (state, checked) in [
            (None, true),
            (Some(PullRequestState::Open), true),
            (Some(PullRequestState::Merged), false),
            (Some(PullRequestState::Closed), false),
        ] {
            s.pull_request = Some(pr(state));
            assert_eq!(due(&s).is_some(), checked, "{state:?}");
        }
        s.pull_request = Some(pr(None));
        s.archived_at = Some(1);
        assert!(due(&s).is_none());
    }

    #[test]
    fn gh_states_parse() {
        assert_eq!(PullRequestState::from_gh("OPEN\n"), Some(PullRequestState::Open));
        assert_eq!(PullRequestState::from_gh("MERGED"), Some(PullRequestState::Merged));
        assert_eq!(PullRequestState::from_gh("CLOSED"), Some(PullRequestState::Closed));
        assert_eq!(PullRequestState::from_gh(""), None);
    }
}
