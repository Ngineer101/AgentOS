//! Pushing and fetching for an agent, with the user's own git credentials
//! (the gateway's built-in `agenthost__git_push` and `agenthost__git_fetch`).
//!
//! Sessions can't read the user's credentials (`~/.config/gh`,
//! `~/.git-credentials`, `~/.ssh`, the keyring), so a push from inside the
//! sandbox fails. Here the host runs git outside the sandbox instead: a push
//! only after the user approved the exact command, a fetch (which changes
//! nothing on the remote) without asking. Only remotes the repository
//! already has, only branch names, never through a shell, and never running
//! anything from the repository (see [`crate::worktree::git_command`]).

use crate::worktree::git_command;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest a push or fetch may take.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Output kept for the agent and the transcript.
const OUTPUT_LIMIT: usize = 16 * 1024;

/// A push or fetch the host will run, as an argument list after `git`.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub dir: PathBuf,
    pub args: Vec<String>,
    pub remote: String,
    pub branch: Option<String>,
    pub force: bool,
}

impl Plan {
    /// The command as the user reads it.
    pub fn command(&self) -> String {
        format!("git {}", self.args.join(" "))
    }

    /// "Push agenthost/fix-flaky to origin".
    pub fn summary(&self) -> String {
        match &self.branch {
            Some(b) if self.force => format!("Force-push {b} to {}", self.remote),
            Some(b) => format!("Push {b} to {}", self.remote),
            None => format!("Fetch from {}", self.remote),
        }
    }
}

async fn output(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command(dir, args)
        .output()
        .await
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A name git accepts as a branch, and that can't be read as a flag or a
/// refspec (`:` deletes, `+` forces).
async fn valid_branch(dir: &Path, name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('-')
        && !name.contains([':', '+', ' ', '\\'])
        && output(dir, &["check-ref-format", "--branch", name]).await.is_ok()
}

/// The remote to use: the one asked for, else the branch's upstream, else
/// `origin`; it must be one the repository has.
async fn resolve_remote(dir: &Path, asked: Option<&str>, branch: Option<&str>) -> Result<String, String> {
    let remotes: Vec<String> = output(dir, &["remote"])
        .await
        .map_err(|e| format!("this folder is not a git repository with remotes ({e})"))?
        .lines()
        .map(str::to_string)
        .collect();
    if remotes.is_empty() {
        return Err("this repository has no remotes".into());
    }
    let remote = match asked.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => r.to_string(),
        None => {
            let upstream = match branch {
                Some(b) => output(dir, &["config", "--get", &format!("branch.{b}.remote")]).await.ok(),
                None => None,
            };
            upstream
                .filter(|r| remotes.contains(r))
                .unwrap_or_else(|| if remotes.iter().any(|r| r == "origin") { "origin".into() } else { remotes[0].clone() })
        }
    };
    if !remotes.contains(&remote) {
        return Err(format!(
            "{remote:?} is not one of this repository's remotes ({})",
            remotes.join(", ")
        ));
    }
    Ok(remote)
}

/// Check a push request and build its command. `branch` defaults to the
/// branch checked out in `dir`; `remote_branch` to the same name.
pub async fn plan_push(
    dir: &Path,
    remote: Option<&str>,
    branch: Option<&str>,
    remote_branch: Option<&str>,
    force_with_lease: bool,
    set_upstream: bool,
) -> Result<Plan, String> {
    let branch = match branch.map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => b.to_string(),
        None => output(dir, &["symbolic-ref", "--short", "HEAD"])
            .await
            .map_err(|_| "no branch is checked out here; name the branch to push".to_string())?,
    };
    if !valid_branch(dir, &branch).await {
        return Err(format!("{branch:?} is not a branch name"));
    }
    if output(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).await.is_err() {
        return Err(format!("there is no local branch {branch:?}"));
    }
    let target = remote_branch.map(str::trim).filter(|b| !b.is_empty()).unwrap_or(&branch).to_string();
    if !valid_branch(dir, &target).await {
        return Err(format!("{target:?} is not a branch name"));
    }
    let remote = resolve_remote(dir, remote, Some(&branch)).await?;
    let mut args = vec!["push".to_string()];
    if force_with_lease {
        args.push("--force-with-lease".into());
    }
    if set_upstream {
        args.push("--set-upstream".into());
    }
    args.push(remote.clone());
    args.push(if target == branch { branch.clone() } else { format!("{branch}:{target}") });
    Ok(Plan { dir: dir.to_path_buf(), args, remote, branch: Some(target), force: force_with_lease })
}

/// Check a fetch request and build its command.
pub async fn plan_fetch(dir: &Path, remote: Option<&str>) -> Result<Plan, String> {
    let remote = resolve_remote(dir, remote, None).await?;
    Ok(Plan {
        dir: dir.to_path_buf(),
        args: vec!["fetch".into(), remote.clone()],
        remote,
        branch: None,
        force: false,
    })
}

/// The outcome of running a plan.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub ok: bool,
    pub output: String,
}

/// Run a plan as the user, outside any sandbox.
pub async fn run(plan: &Plan) -> Outcome {
    let args: Vec<&str> = plan.args.iter().map(String::as_str).collect();
    let mut cmd = git_command(&plan.dir, &args);
    match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
        Err(_) => Outcome { ok: false, output: format!("stopped after {} minutes", GIT_TIMEOUT.as_secs() / 60) },
        Ok(Err(e)) => Outcome { ok: false, output: format!("could not run git: {e}") },
        Ok(Ok(out)) => {
            // git reports progress and results on stderr.
            let mut text = String::from_utf8_lossy(&out.stderr).to_string();
            let so = String::from_utf8_lossy(&out.stdout);
            if !so.trim().is_empty() {
                text.push('\n');
                text.push_str(&so);
            }
            let (text, _) = crate::util::truncate(text.trim(), OUTPUT_LIMIT);
            Outcome { ok: out.status.success(), output: text }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let s = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(s.status.success(), "{args:?}: {}", String::from_utf8_lossy(&s.stderr));
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let remote = t.path().join("remote.git");
        let work = t.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        sh(t.path(), &["init", "--bare", "-q", remote.to_str().unwrap()]);
        sh(&work, &["init", "-q", "-b", "main"]);
        sh(&work, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "one"]);
        sh(&work, &["remote", "add", "origin", remote.to_str().unwrap()]);
        (t, work)
    }

    #[tokio::test]
    async fn plans_exact_pushes_and_refuses_anything_else() {
        let (_t, w) = repo();
        let p = plan_push(&w, None, None, None, false, true).await.unwrap();
        assert_eq!(p.command(), "git push --set-upstream origin main");
        assert_eq!(p.summary(), "Push main to origin");
        let p = plan_push(&w, Some("origin"), Some("main"), Some("release"), true, false).await.unwrap();
        assert_eq!(p.command(), "git push --force-with-lease origin main:release");
        assert_eq!(p.summary(), "Force-push release to origin");
        for bad in ["--delete", ":main", "+main", "main:other", "a b", "..", "nope"] {
            assert!(plan_push(&w, None, Some(bad), None, false, false).await.is_err(), "{bad}");
        }
        assert!(plan_push(&w, None, None, Some(":x"), false, false).await.is_err());
        assert!(plan_push(&w, Some("evil"), None, None, false, false).await.is_err());
        assert!(plan_push(&w, Some("https://evil.example/x.git"), None, None, false, false).await.is_err());
        assert_eq!(plan_fetch(&w, None).await.unwrap().command(), "git fetch origin");

        let out = run(&plan_push(&w, None, None, None, false, false).await.unwrap()).await;
        assert!(out.ok, "{}", out.output);
        assert!(run(&plan_fetch(&w, None).await.unwrap()).await.ok);
    }

    #[tokio::test]
    async fn pushes_never_run_the_repository_s_hooks() {
        let (_t, w) = repo();
        let hooks = w.join("planted-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let marker = w.join("hook-ran");
        let hook = hooks.join("pre-push");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        sh(&w, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        let out = run(&plan_push(&w, None, None, None, false, false).await.unwrap()).await;
        assert!(out.ok, "{}", out.output);
        assert!(!marker.exists());
    }
}
