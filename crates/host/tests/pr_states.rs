//! Sessions' pull requests checked with the user's GitHub CLI, in process
//! (no socket): open ones are asked about, merged and closed ones and
//! archived sessions aren't.

mod common;

use agenthost_host::Host;
use agenthost_proto::*;

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

/// A fake `gh` that logs what it was asked and answers by pull request:
/// 1 merged, 2 open, 3 closed, anything else an error.
fn fake_gh(bin: &std::path::Path, log: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$3\" in\n  */pull/1) echo MERGED ;;\n  */pull/2) echo OPEN ;;\n  */pull/3) echo CLOSED ;;\n  *) echo 'no such pull request' >&2; exit 1 ;;\nesac\n",
        log.display()
    );
    let p = bin.join("gh");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn session(id: &str, pr: Option<(u64, Option<PullRequestState>)>, archived: bool) -> Session {
    let mut s: Session = serde_json::from_value(serde_json::json!({
        "session_id": id, "workspace_id": QUICK_WORKSPACE_ID, "agent": "fake", "state": "idle",
        "created_at": 0, "updated_at": 0, "last_seq": 0
    }))
    .unwrap();
    s.pull_request = pr.map(|(n, state)| SessionPullRequest { number: n, url: format!("https://github.com/o/r/pull/{n}"), state });
    s.archived_at = archived.then_some(1);
    s
}

#[tokio::test]
async fn open_pull_requests_are_checked_until_merged_or_closed() {
    let dir = tempdir();
    let mut cfg = common::test_config(dir.path());
    let bin = dir.path().join("programs");
    let log = dir.path().join("gh-log.txt");
    fake_gh(&bin, &log);
    cfg.install_bin_dir = Some(bin);
    let host = Host::open(cfg).unwrap();
    let st = &host.ctx.store;
    st.session_insert(&session("ses_merged", Some((1, None)), false)).unwrap();
    st.session_insert(&session("ses_open", Some((2, Some(PullRequestState::Open))), false)).unwrap();
    st.session_insert(&session("ses_closed", Some((3, None)), false)).unwrap();
    st.session_insert(&session("ses_gone", Some((4, Some(PullRequestState::Open))), false)).unwrap();
    st.session_insert(&session("ses_settled", Some((5, Some(PullRequestState::Merged))), false)).unwrap();
    st.session_insert(&session("ses_archived", Some((6, None)), true)).unwrap();
    st.session_insert(&session("ses_none", None, false)).unwrap();
    let mut bus = host.ctx.bus.subscribe();

    // 1 and 3 changed; 2 is still open; 4's error leaves it as it was.
    assert_eq!(host.pr_states.check().await, 2);
    let state = |id: &str| st.session_get(id).unwrap().unwrap().pull_request.and_then(|p| p.state);
    assert_eq!(state("ses_merged"), Some(PullRequestState::Merged));
    assert_eq!(state("ses_open"), Some(PullRequestState::Open));
    assert_eq!(state("ses_closed"), Some(PullRequestState::Closed));
    assert_eq!(state("ses_gone"), Some(PullRequestState::Open));
    assert_eq!(state("ses_archived"), None);
    let asked = std::fs::read_to_string(&log).unwrap();
    assert!(asked.contains("pr view https://github.com/o/r/pull/1 --json state --jq .state"), "{asked}");
    for n in [5, 6] {
        assert!(!asked.contains(&format!("pull/{n} ")), "asked about {n}: {asked}");
    }

    // Clients heard about both.
    let mut told = vec![];
    while let Ok(m) = bus.try_recv() {
        if let agenthost_host::bus::BusMsg::SessionChanged(s) = m {
            told.push(s.session_id);
        }
    }
    told.sort();
    assert_eq!(told, ["ses_closed", "ses_merged"]);

    // Next time, only the ones still open are asked about.
    std::fs::remove_file(&log).unwrap();
    assert_eq!(host.pr_states.check().await, 0);
    let asked = std::fs::read_to_string(&log).unwrap();
    assert_eq!(asked.lines().count(), 2, "{asked}");
}
