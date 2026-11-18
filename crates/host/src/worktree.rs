//! Git worktrees: each session in a git workspace works on a branch of its
//! own, in a worktree under the host's worktrees folder, so several sessions
//! can run in one repository without touching each other's files.
//!
//! Git runs here, on the host and outside any sandbox; inside the sandbox the
//! agent only gets the parts of the repository's `.git` a worktree writes to
//! (see [`GitAccess`]).

use crate::config::HostConfig;
use agenthost_proto::SessionWorktree;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// `git -C <dir> <args>` as the host runs it, outside any sandbox: nothing
/// from the repository runs (no hooks, e.g. `post-checkout` on `worktree
/// add` or `pre-push`, which a repository can point into files the agent
/// writes; no fsmonitor command), and the host's own environment can't point
/// git somewhere else or make it prompt.
pub(crate) fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
        .args(args);
    for v in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_CEILING_DIRECTORIES",
    ] {
        cmd.env_remove(v);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// Run `git -C <dir> <args>` and return its trimmed output.
async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = git_command(dir, args);
    let out = cmd
        .output()
        .await
        .context("run git (is it installed?)")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            if err.is_empty() {
                out.status.to_string()
            } else {
                err
            }
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The repository a folder is in.
pub struct Repo {
    /// The repository's top folder (its main worktree).
    pub toplevel: PathBuf,
    /// The folder relative to the top ("" at the top itself), e.g. `crates/host`.
    pub subdir: String,
}

/// The repository `dir` is in, if it is in one that has a commit to branch
/// from. A repository without commits (or no git at all) is `None`: its
/// sessions share the folder.
pub async fn repo_of(dir: &Path) -> Option<Repo> {
    let toplevel = git(dir, &["rev-parse", "--show-toplevel"]).await.ok()?;
    if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.is_err() {
        tracing::info!(folder = %dir.display(), "git repository has no commits yet; sessions share its folder");
        return None;
    }
    let subdir = git(dir, &["rev-parse", "--show-prefix"])
        .await
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    Some(Repo {
        toplevel: PathBuf::from(toplevel),
        subdir,
    })
}

/// The branch a session works on: `agenthost/<short session id>`.
pub fn branch_for(session_id: &str) -> String {
    let short: String = session_id
        .trim_start_matches("ses_")
        .chars()
        .take(10)
        .collect();
    format!("agenthost/{short}")
}

/// A new worktree for a session, on a new branch from the repository's
/// current `HEAD`. `None` when `workspace` is not in a git repository with a
/// commit. Returns the worktree and the workspace's folder inside it.
pub async fn create(
    cfg: &HostConfig,
    workspace: &Path,
    session_id: &str,
) -> Result<Option<(SessionWorktree, String)>> {
    let Some(repo) = repo_of(workspace).await else {
        return Ok(None);
    };
    crate::config::create_private_dir(&cfg.worktrees_dir())?;
    let base = std::fs::canonicalize(cfg.worktrees_dir()).unwrap_or(cfg.worktrees_dir());
    let dir = base.join(session_id);
    let branch = branch_for(session_id);
    let dir_s = dir.display().to_string();
    git(
        &repo.toplevel,
        &["worktree", "add", "--quiet", "-b", &branch, &dir_s, "HEAD"],
    )
    .await
    .with_context(|| format!("could not create a worktree for the session in {}", repo.toplevel.display()))?;
    Ok(Some((
        SessionWorktree {
            path: dir_s,
            branch,
        },
        repo.subdir,
    )))
}

/// Bring back a worktree that was removed (when its session was archived),
/// from its branch. A no-op while it is there.
pub async fn ensure(workspace: &Path, wt: &SessionWorktree) -> Result<()> {
    let dir = Path::new(&wt.path);
    if dir.join(".git").exists() {
        return Ok(());
    }
    // The old entry (if git still lists it) would refuse the folder.
    let _ = git(workspace, &["worktree", "prune"]).await;
    if let Some(parent) = dir.parent() {
        crate::config::create_private_dir(parent)?;
    }
    git(workspace, &["worktree", "add", "--quiet", &wt.path, &wt.branch])
        .await
        .with_context(|| format!("could not re-create the worktree from branch {}", wt.branch))?;
    Ok(())
}

/// Remove a session's worktree if it has no uncommitted changes (its branch
/// stays either way). Returns whether it was removed; a worktree with
/// changes, or one git refuses to remove, is kept as it is.
pub async fn remove_if_clean(wt: &SessionWorktree) -> Result<bool> {
    let dir = Path::new(&wt.path);
    if !dir.exists() {
        return Ok(false);
    }
    let status = git(dir, &["status", "--porcelain"]).await?;
    if !status.is_empty() {
        tracing::info!(worktree = %wt.path, "worktree has uncommitted changes; keeping it");
        return Ok(false);
    }
    let access = access(dir).await?;
    let main = access
        .common_dir
        .parent()
        .ok_or_else(|| anyhow!("no repository above {}", access.common_dir.display()))?
        .to_path_buf();
    git(&main, &["worktree", "remove", &wt.path]).await?;
    Ok(true)
}

/// Where git keeps a worktree's state.
#[derive(Debug, Clone, PartialEq)]
pub struct GitAccess {
    /// The worktree's top folder.
    pub root: PathBuf,
    /// The repository's `.git` (objects, refs, the main config and hooks).
    pub common_dir: PathBuf,
    /// The worktree's own folder in it, `.git/worktrees/<name>`.
    pub admin_dir: PathBuf,
}

/// The files at the top of a repository's `.git` that a worktree session may
/// not write: each can make your own git run something outside the sandbox
/// (`config`, `hooks`, a submodule's config under `modules`) or point your
/// checkout at a config the agent wrote (`commondir`).
pub const PROTECTED_GIT_ENTRIES: &[&str] = &["config", "config.worktree", "hooks", "commondir", "modules"];

/// Make the protected entries exist, so a Linux sandbox can mount them
/// read-only (bubblewrap can only protect what exists, and its empty
/// placeholder for a missing `commondir` would break git). A `commondir` of
/// "." names the repository itself, which is what git assumes without one;
/// `hooks` and `modules` are empty folders. Seatbelt protects missing paths
/// as they are, so on macOS nothing is created.
pub fn protect_common_dir(common: &Path) -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    for d in ["hooks", "modules"] {
        let p = common.join(d);
        if std::fs::symlink_metadata(&p).is_err() {
            std::fs::create_dir(&p).with_context(|| format!("create {}", p.display()))?;
        }
    }
    let c = common.join("commondir");
    if std::fs::symlink_metadata(&c).is_err() {
        std::fs::write(&c, ".\n").with_context(|| format!("create {}", c.display()))?;
    }
    Ok(())
}

/// The `.git` folders a worktree uses.
pub async fn access(root: &Path) -> Result<GitAccess> {
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let admin = git(root, &["rev-parse", "--absolute-git-dir"]).await?;
    Ok(GitAccess {
        root: root.to_path_buf(),
        common_dir: PathBuf::from(common),
        admin_dir: PathBuf::from(admin),
    })
}
