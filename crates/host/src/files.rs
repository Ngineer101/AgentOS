//! A session's files and what it changed (`session.changes`,
//! `session.files`, `session.file_read`), read only.
//!
//! Git runs here as it does for worktrees: on the host, outside any sandbox,
//! with nothing from the repository run (see [`worktree::git_command`]), and
//! no external diff or text conversion programs either. Files are read only
//! from inside the session's folder: a path, or a link, that leaves it is
//! refused.

use crate::ctx::Ctx;
use crate::worktree;
use agenthost_proto::*;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

/// Where a session's files are.
pub enum Source {
    /// A folder on disk; `git` when it is in a repository.
    Dir { root: PathBuf, git: bool },
    /// A branch of the repository at `repo`: the session's worktree was
    /// removed when it was archived, and its commits are what is left.
    Branch { repo: PathBuf, branch: String },
}

/// `git -C dir args`, its output as it came.
async fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = worktree::git_command(dir, args).output().await.map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("git {} failed", args.first().copied().unwrap_or("")) } else { err });
    }
    Ok(out.stdout)
}

async fn git_line(dir: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git(dir, args).await?).trim().to_string())
}

fn session_and_workspace(ctx: &Ctx, session_id: &str) -> Result<(Session, Workspace), RpcError> {
    let s = ctx.store.session_get(session_id).map_err(internal)?.ok_or_else(|| RpcError::not_found("no such session"))?;
    let w = ctx.store.workspace_get(&s.workspace_id).map_err(internal)?.ok_or_else(|| RpcError::not_found("the session's workspace is gone"))?;
    Ok((s, w))
}

async fn source(ctx: &Ctx, s: &Session, w: &Workspace) -> Source {
    if w.kind == WorkspaceKind::Scratch {
        return Source::Dir { root: crate::session::scratch_folder(ctx, &s.session_id), git: false };
    }
    if let Some(wt) = &s.worktree {
        let root = PathBuf::from(&wt.path);
        if root.join(".git").exists() {
            return Source::Dir { root, git: true };
        }
        return Source::Branch { repo: PathBuf::from(&w.path), branch: wt.branch.clone() };
    }
    let root = PathBuf::from(&w.path);
    let git = git_line(&root, &["rev-parse", "--is-inside-work-tree"]).await.is_ok_and(|v| v == "true");
    Source::Dir { root, git }
}

pub async fn changes(ctx: &Ctx, session_id: &str) -> Result<SessionChanges, RpcError> {
    let (s, w) = session_and_workspace(ctx, session_id)?;
    let Some(wt) = &s.worktree else {
        return Err(RpcError::invalid_state("this session has no branch of its own to compare"));
    };
    let dir = PathBuf::from(&wt.path);
    let live = dir.join(".git").exists();
    changes_of(Path::new(&w.path), live.then_some(dir.as_path()), &wt.branch).await.map_err(RpcError::invalid_state)
}

/// How long a fetch of the remote's main branch may take before the changes
/// are measured from what was fetched last.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// A repository's remote is asked at most this often.
const FETCH_EVERY: Duration = Duration::from_secs(60);

/// The remote's main branch as this repository tracks it
/// (`refs/remotes/origin/main`), brought up to date first when the remote
/// answers in time. `None` for a repository without an `origin`.
async fn remote_main(repo: &Path) -> Option<String> {
    let mut found = git_line(repo, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]).await.ok().filter(|r| r.starts_with("refs/remotes/origin/"));
    for name in ["main", "master"] {
        if found.is_none() {
            let r = format!("refs/remotes/origin/{name}");
            if git_line(repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")]).await.is_ok() {
                found = Some(r);
            }
        }
    }
    let r = found?;
    let name = r.trim_start_matches("refs/remotes/origin/").to_string();
    static FETCHED: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);
    let due = {
        let mut g = FETCHED.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(HashMap::new);
        let due = map.get(repo).is_none_or(|t| t.elapsed() >= FETCH_EVERY);
        if due {
            map.insert(repo.to_path_buf(), Instant::now());
        }
        due
    };
    if due {
        // Offline, or a remote that wants a password: what was fetched last stands.
        let refspec = format!("+refs/heads/{name}:{r}");
        let _ = tokio::time::timeout(FETCH_TIMEOUT, worktree::git_command(repo, &["fetch", "--quiet", "--no-tags", "origin", &refspec]).output()).await;
    }
    git_line(repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")]).await.ok().map(|_| r)
}

/// What `branch` changed since it left the remote's main branch (fetched
/// first, so a stale local `main` doesn't matter), which is what a pull
/// request from it shows. A repository without a remote compares with the
/// branch `repo` has checked out. With its `worktree`, uncommitted work and
/// untracked files count too.
pub async fn changes_of(repo: &Path, worktree: Option<&Path>, branch: &str) -> Result<SessionChanges, String> {
    let against = remote_main(repo).await;
    let theirs = git_line(repo, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", against.as_deref().unwrap_or("HEAD"))]).await.map_err(|_| "the workspace has no commit to compare with".to_string())?;
    let against = against.map(|r| r.trim_start_matches("refs/remotes/").to_string());
    let (dir, head) = match worktree {
        Some(d) => (d, "HEAD".to_string()),
        None => (repo, format!("refs/heads/{branch}")),
    };
    let base = git_line(dir, &["merge-base", &head, &theirs]).await.map_err(|_| format!("{branch} shares no history with {}", against.as_deref().unwrap_or("the workspace's branch")))?;
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M", &base];
    if worktree.is_none() {
        args.push(&head);
    }
    args.push("--");
    let mut patch = String::from_utf8_lossy(&git(dir, &args).await?).into_owned();
    let mut truncated = false;
    if patch.len() > CHANGES_MAX_BYTES {
        // Whole files only: cut where the last one that fits ends.
        let mut cut = CHANGES_MAX_BYTES;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        let end = patch[..cut].rfind("\ndiff --git ").map(|i| i + 1).unwrap_or(0);
        patch.truncate(end);
        truncated = true;
    }
    if let (Some(dir), false) = (worktree, truncated) {
        let out = git(dir, &["ls-files", "-z", "--others", "--exclude-standard"]).await?;
        for p in out.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let Ok(p) = std::str::from_utf8(p) else { continue };
            let Some(one) = added_file_patch(dir, p) else { continue };
            if patch.len() + one.len() > CHANGES_MAX_BYTES {
                truncated = true;
                break;
            }
            patch.push_str(&one);
        }
    }
    Ok(SessionChanges { branch: branch.to_string(), base, against, patch, truncated })
}

/// A path as `git diff` writes it in a header: quoted when it must be.
fn quoted(prefix: &str, path: &str) -> String {
    if !path.bytes().any(|b| b < 0x20 || b == b'"' || b == b'\\' || b == 0x7f) {
        return format!("{prefix}{path}");
    }
    let mut s = format!("\"{prefix}");
    for c in path.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => s.push_str(&format!("\\{:03o}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// A file git doesn't track yet, as the patch that adds it. `None` for
/// anything that isn't a plain file (links are not followed).
fn added_file_patch(root: &Path, path: &str) -> Option<String> {
    let full = root.join(path);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if !meta.is_file() {
        return None;
    }
    let (a, b) = (quoted("a/", path), quoted("b/", path));
    let mut out = format!("diff --git {a} {b}\nnew file mode 100644\n");
    let text = if meta.len() as usize > FILE_READ_MAX_BYTES { None } else { std::fs::read(&full).ok().and_then(|d| if d.contains(&0) { None } else { String::from_utf8(d).ok() }) };
    let Some(text) = text else {
        out.push_str(&format!("Binary files /dev/null and {b} differ\n"));
        return Some(out);
    };
    if text.is_empty() {
        return Some(out);
    }
    let lines = text.split_inclusive('\n').count();
    out.push_str(&format!("--- /dev/null\n+++ {b}\n@@ -0,0 +1{} @@\n", if lines == 1 { String::new() } else { format!(",{lines}") }));
    for line in text.split_inclusive('\n') {
        out.push('+');
        out.push_str(line);
    }
    if !text.ends_with('\n') {
        out.push_str("\n\\ No newline at end of file\n");
    }
    Some(out)
}

pub async fn list(ctx: &Ctx, session_id: &str) -> Result<SessionFiles, RpcError> {
    let (s, w) = session_and_workspace(ctx, session_id)?;
    list_of(source(ctx, &s, &w).await).await.map_err(RpcError::invalid_state)
}

pub async fn list_of(src: Source) -> Result<SessionFiles, String> {
    let out = match src {
        Source::Dir { root, git: true } => git(&root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).await?,
        Source::Branch { repo, branch } => git(&repo, &["ls-tree", "-r", "-z", "--name-only", &format!("refs/heads/{branch}")]).await.map_err(|_| "this session's folder and branch are gone".to_string())?,
        Source::Dir { root, git: false } => {
            return tokio::task::spawn_blocking(move || walk(&root)).await.map_err(|e| e.to_string());
        }
    };
    let mut files: Vec<String> = out.split(|b| *b == 0).filter(|p| !p.is_empty()).filter_map(|p| std::str::from_utf8(p).ok().map(str::to_string)).collect();
    files.sort();
    files.dedup();
    let truncated = files.len() > FILES_MAX;
    files.truncate(FILES_MAX);
    Ok(SessionFiles { files, truncated })
}

/// Every file under `root`, without `.git` and without following links.
fn walk(root: &Path) -> SessionFiles {
    let mut files = Vec::new();
    let mut truncated = false;
    let mut dirs = vec![PathBuf::new()];
    'all: while let Some(rel) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(ty) = e.file_type() else { continue };
            let path = if rel.as_os_str().is_empty() { PathBuf::from(name) } else { rel.join(name) };
            if ty.is_dir() {
                if name != ".git" {
                    dirs.push(path);
                }
            } else if ty.is_file() || ty.is_symlink() {
                if files.len() == FILES_MAX {
                    truncated = true;
                    break 'all;
                }
                files.push(path.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    SessionFiles { files, truncated }
}

pub async fn read(ctx: &Ctx, p: SessionFileReadParams) -> Result<SessionFile, RpcError> {
    let (s, w) = session_and_workspace(ctx, &p.session_id)?;
    read_of(source(ctx, &s, &w).await, &p.path).await
}

pub async fn read_of(src: Source, path: &str) -> Result<SessionFile, RpcError> {
    let rel = Path::new(path);
    if path.is_empty() || path.contains('\0') || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(RpcError::invalid_params("path must be a file inside the session's folder"));
    }
    let (size, mut data) = match src {
        Source::Dir { root, .. } => {
            let gone = || RpcError::not_found("no such file");
            let root = tokio::fs::canonicalize(&root).await.map_err(|_| gone())?;
            // Links resolve first, so one that points out of the folder is refused.
            let full = tokio::fs::canonicalize(root.join(rel)).await.map_err(|_| gone())?;
            if !full.starts_with(&root) {
                return Err(RpcError::invalid_params("that path leads outside the session's folder"));
            }
            let meta = tokio::fs::metadata(&full).await.map_err(|_| gone())?;
            if !meta.is_file() {
                return Err(RpcError::invalid_params("not a file"));
            }
            use tokio::io::AsyncReadExt;
            let f = tokio::fs::File::open(&full).await.map_err(|_| gone())?;
            let mut data = Vec::new();
            f.take(FILE_READ_MAX_BYTES as u64 + 1).read_to_end(&mut data).await.map_err(internal)?;
            (meta.len().max(data.len() as u64), data)
        }
        Source::Branch { repo, branch } => {
            let data = git(&repo, &["cat-file", "blob", &format!("refs/heads/{branch}:{path}")]).await.map_err(|_| RpcError::not_found("no such file"))?;
            (data.len() as u64, data)
        }
    };
    let truncated = data.len() > FILE_READ_MAX_BYTES;
    data.truncate(FILE_READ_MAX_BYTES);
    let text = if data.contains(&0) {
        None
    } else {
        match String::from_utf8(data) {
            Ok(t) => Some(t),
            // The cut may have landed inside a character.
            Err(e) if truncated && e.utf8_error().valid_up_to() + 4 > e.as_bytes().len() => {
                let n = e.utf8_error().valid_up_to();
                let mut d = e.into_bytes();
                d.truncate(n);
                String::from_utf8(d).ok()
            }
            Err(_) => None,
        }
    };
    Ok(SessionFile { path: path.to_string(), size, text, truncated })
}
