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

/// The repository a push, fetch or pull request works in: the session's
/// folder, or `repo`, a folder inside it (a workspace that holds
/// repositories rather than being one). Never a folder outside the
/// session's, through `..` or a symlink.
///
/// With no `repo` and a session folder that isn't in a repository, the error
/// names the repositories inside it, so the agent can pick one.
pub async fn repo_dir(session_dir: &Path, repo: Option<&str>) -> Result<PathBuf, String> {
    let Some(asked) = repo.map(str::trim).filter(|r| !r.is_empty() && *r != ".") else {
        if output(session_dir, &["rev-parse", "--git-dir"]).await.is_ok() {
            return Ok(session_dir.to_path_buf());
        }
        let inside = repos_inside(session_dir);
        if inside.is_empty() {
            return Err("this session's folder is not a git repository".into());
        }
        return Err(format!(
            "this session's folder is not a git repository, but it holds {}: pass that folder as `repo`",
            inside.join(", ")
        ));
    };
    let base = std::fs::canonicalize(session_dir)
        .map_err(|e| format!("this session's folder can't be read ({e})"))?;
    let dir = std::fs::canonicalize(base.join(asked))
        .ok()
        .filter(|d| d.starts_with(&base) && d.is_dir())
        .ok_or_else(|| format!("{asked:?} is not a folder inside this session's folder"))?;
    if output(&dir, &["rev-parse", "--git-dir"]).await.is_err() {
        return Err(format!("{asked:?} is not a git repository"));
    }
    Ok(dir)
}

/// Repositories up to two folders down from `dir`, as relative paths.
pub fn repos_inside(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            // Not following symlinks: only folders really inside.
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let p = e.path();
            if p.join(".git").exists() {
                if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            } else if depth < 2 {
                walk(base, &p, depth + 1, out);
            }
            if out.len() >= 10 {
                return;
            }
        }
    }
    let mut out = vec![];
    walk(dir, dir, 1, &mut out);
    out
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

/// A pull request the host will open with the user's GitHub CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct PrPlan {
    pub dir: PathBuf,
    pub program: PathBuf,
    pub title: String,
    pub body: String,
    pub base: String,
    pub head: String,
    pub draft: bool,
}

impl PrPlan {
    /// The command as the user reads it (the body goes in on stdin).
    pub fn command(&self) -> String {
        format!(
            "gh pr create --base {} --head {} --title {:?}{} --body-file -",
            self.base,
            self.head,
            self.title,
            if self.draft { " --draft" } else { "" }
        )
    }

    pub fn summary(&self) -> String {
        format!("Open a {}pull request: {}", if self.draft { "draft " } else { "" }, self.title)
    }
}

/// Check a pull request request. `head` defaults to the branch checked out
/// in `dir`; `base` to the remote's default branch (gh decides) when empty.
pub async fn plan_pr(
    dir: &Path,
    program: &Path,
    title: &str,
    body: &str,
    base: Option<&str>,
    head: Option<&str>,
    draft: bool,
) -> Result<PrPlan, String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 256 || title.contains(['\n', '\r']) {
        return Err("give the pull request a one-line title of at most 256 characters".into());
    }
    if body.len() > 64 * 1024 {
        return Err("the description is longer than 64 KB".into());
    }
    let head = match head.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => h.to_string(),
        None => output(dir, &["symbolic-ref", "--short", "HEAD"])
            .await
            .map_err(|_| "no branch is checked out here; name the head branch".to_string())?,
    };
    if !valid_branch(dir, &head).await {
        return Err(format!("{head:?} is not a branch name"));
    }
    let base = match base.map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => b.to_string(),
        None => {
            let d = output(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await.unwrap_or_default();
            d.strip_prefix("origin/").unwrap_or("main").to_string()
        }
    };
    if !valid_branch(dir, &base).await {
        return Err(format!("{base:?} is not a branch name"));
    }
    if base == head {
        return Err("the head and base branches are the same".into());
    }
    Ok(PrPlan {
        dir: dir.to_path_buf(),
        program: program.to_path_buf(),
        title: title.to_string(),
        body: body.to_string(),
        base,
        head,
        draft,
    })
}

/// Open the pull request as the user, outside any sandbox. git commands gh
/// runs itself get the same protection as the host's own (no hooks, no
/// fsmonitor), through git's environment configuration.
pub async fn run_pr(plan: &PrPlan) -> Outcome {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.current_dir(&plan.dir)
        .args(["pr", "create", "--base", &plan.base, "--head", &plan.head, "--title", &plan.title, "--body-file", "-"]);
    if plan.draft {
        cmd.arg("--draft");
    }
    cmd.env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_1", "false")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome { ok: false, output: format!("could not run gh: {e}") },
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(plan.body.as_bytes()).await;
    }
    match tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output()).await {
        Err(_) => Outcome { ok: false, output: "stopped after 5 minutes".into() },
        Ok(Err(e)) => Outcome { ok: false, output: format!("could not run gh: {e}") },
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                text.push('\n');
                text.push_str(&err);
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
    async fn finds_the_repository_inside_a_plain_folder() {
        let (t, w) = repo();
        // The temp folder holds `work` (a repository) and `remote.git` (bare,
        // so not listed: no `.git` inside).
        let folder = t.path();
        let err = repo_dir(folder, None).await.unwrap_err();
        assert!(err.contains("holds work") && err.contains("`repo`"), "{err}");
        let dir = repo_dir(folder, Some("work")).await.unwrap();
        assert_eq!(dir, std::fs::canonicalize(&w).unwrap());
        assert_eq!(plan_push(&dir, None, None, None, false, false).await.unwrap().command(), "git push origin main");
        // The session's own repository, and folders inside it, need no `repo`.
        assert_eq!(repo_dir(&w, None).await.unwrap(), w);
        assert_eq!(repo_dir(&w, Some(".")).await.unwrap(), w);
        // Nothing outside the session's folder.
        for bad in ["..", "../remote.git", "/", "nope", "remote.git/../.."] {
            assert!(repo_dir(&w, Some(bad)).await.is_err(), "{bad}");
        }
        // A symlink inside that points outside.
        let inner = folder.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::os::unix::fs::symlink(&w, inner.join("out")).unwrap();
        assert!(repo_dir(&inner, Some("out")).await.is_err());
        // A folder inside, but not a repository.
        std::fs::create_dir_all(inner.join("plain")).unwrap();
        let err = repo_dir(&inner, Some("plain")).await.unwrap_err();
        assert!(err.contains("not a git repository"), "{err}");
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
