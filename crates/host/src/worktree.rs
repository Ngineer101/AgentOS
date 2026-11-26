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

/// Settings that stop a repository's own config from making git run
/// something: each of these can name a command (or a transport helper that
/// is one) that git executes as the user, outside the sandbox, and a session
/// writes its repository's config whenever that repository isn't the
/// protected worktree the host made for it (a folder the agent ran `git
/// init` in, a shared folder, a quick task).
///
/// `-c` wins over every config file, so these hold whatever the repository
/// says. They are also passed on to the git commands git itself starts
/// (through `GIT_CONFIG_PARAMETERS`), so a local `upload-pack` gets them
/// too. `remote.<name>.uploadpack` and `.receivepack` can't be pinned here —
/// their key contains the remote's name, and a `-c` for a key the repository
/// also sets only competes with it — so
/// [`crate::gitremote::Plan::run_args`] pins those on the command line.
const FIXED_GIT_CONFIG: &[&str] = &[
    // Hooks the repository points at files the agent writes. Yours are
    // skipped too: nothing from a repository runs here, by design.
    "core.hooksPath=/dev/null",
    // A filesystem monitor command.
    "core.fsmonitor=false",
    // `ext::sh -c …` as a remote URL is a command by design.
    "protocol.ext.allow=never",
    // A submodule brings its own config and URLs, from inside the worktree.
    "fetch.recurseSubmodules=no",
];

/// The three of these that you might legitimately set yourself, with the
/// default to fall back on. A repository's value is ignored; yours is kept.
const USER_GIT_CONFIG: &[(&str, &str)] = &[
    // Run through `sh -c` for an ssh remote.
    ("core.sshCommand", "ssh"),
    // A `!command` helper is a shell command git runs to "ask" for a
    // password. An empty value resets the list, so yours are added back
    // after it and the repository's are left out.
    ("credential.helper", ""),
    // A proxy command for `git://`. `none` means no proxy, for every domain.
    ("core.gitProxy", "none"),
];

/// Every `-c` setting the host passes git.
///
/// For the settings you might have set yourself, the value comes from your
/// own config — global and system scope, neither of which any sandbox can
/// write — rather than a flat default, so a `core.sshCommand` that picks a
/// key, or a credential helper you rely on, keeps working while the
/// repository's copy of the same setting is ignored. Read once.
pub(crate) fn safe_git_config() -> &'static [String] {
    static CONFIG: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CONFIG.get_or_init(|| build_safe_git_config(&[]))
}

/// [`safe_git_config`], reading your config with `env` added to git's
/// environment (tests point it at a config of their own).
fn build_safe_git_config(env: &[(&str, &str)]) -> Vec<String> {
    let mut out: Vec<String> = FIXED_GIT_CONFIG.iter().map(|c| c.to_string()).collect();
    for (key, default) in USER_GIT_CONFIG {
        // Reset first: this drops whatever the repository set.
        out.push(format!("{key}={default}"));
        // Then yours, in git's own order of precedence.
        if *key == "credential.helper" {
            out.extend(user_credential_helpers(env));
            continue;
        }
        for scope in ["--system", "--global"] {
            for value in user_git_config(scope, key, env) {
                out.push(format!("{key}={value}"));
            }
        }
    }
    out
}

/// Your credential helpers, as `-c` settings in the order git reads them.
///
/// Not only `credential.helper`: a helper is usually kept for one site, as
/// `credential.https://github.com.helper`, which is how `gh auth setup-git`
/// writes GitHub's. git keeps one list for a URL, filled from both kinds of
/// key, and the empty `credential.helper` that drops the repository's
/// helpers empties all of it. So every helper of yours goes back, under its
/// own key (it still answers only for its own site) and with your own empty
/// values, which reset the list where you meant them to.
fn user_credential_helpers(env: &[(&str, &str)]) -> Vec<String> {
    let mut out = vec![];
    for scope in ["--system", "--global"] {
        // git matches the pattern against the key with its section and
        // variable in lower case and the URL between them as written.
        let found = git_config(&[scope, "--includes", "-z", "--get-regexp", r"^credential\.(.*\.)?helper$"], env);
        // With `-z` a setting is its key, a newline, its value and a NUL.
        for entry in found.split('\0').filter(|e| !e.is_empty()) {
            let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
            // `-c key=value` ends the key at the first `=`: a URL with one
            // in it can't be passed on, and is left out.
            if !key.contains('=') {
                out.push(format!("{key}={value}"));
            }
        }
    }
    out
}

/// One config key's values from a scope outside any repository, and the
/// files it includes.
fn user_git_config(scope: &str, key: &str, env: &[(&str, &str)]) -> Vec<String> {
    git_config(&[scope, "--includes", "--get-all", key], env)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// What `git config <args>` prints, or nothing if it fails (no such key).
fn git_config(args: &[&str], env: &[(&str, &str)]) -> String {
    let out = std::process::Command::new("git")
        .arg("config")
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

/// `git -C <dir> <args>` as the host runs it, outside any sandbox: nothing
/// from the repository runs (no hooks, e.g. `post-checkout` on `worktree
/// add` or `pre-push`, which a repository can point into files the agent
/// writes; no fsmonitor command, no command from its config at all, see
/// [`safe_git_config`]), and the host's own environment can't point git
/// somewhere else or make it prompt.
pub(crate) fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for c in safe_git_config() {
        cmd.arg("-c").arg(c);
    }
    cmd.args(args);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// H1: the settings that hold a repository's config can't be a flat
    /// override, or a `core.sshCommand` you set yourself (to pick a key, say)
    /// would stop working. Yours is read from a scope no sandbox can write;
    /// the repository's is dropped either way.
    #[test]
    fn your_own_config_is_kept_while_a_repository_s_is_dropped() {
        let c = safe_git_config();
        let value_of = |key: &str| -> Vec<&str> {
            c.iter()
                .filter_map(|s| s.strip_prefix(&format!("{key}=")))
                .collect()
        };
        // Each of these is reset before anything is added back, so whatever
        // a repository says for them never survives.
        for (key, default) in USER_GIT_CONFIG {
            let vals = value_of(key);
            assert_eq!(vals.first(), Some(default), "{key} must be reset first");
        }
        // The flat safety settings are there once and are not negotiable.
        for fixed in FIXED_GIT_CONFIG {
            assert!(c.iter().any(|s| s == fixed), "{fixed}");
        }
        // The lookup itself works: this machine has a global user.email, and
        // the same call shape is what reads the settings above.
        let scoped = user_git_config("--global", "user.email", &[]);
        let global_set = std::process::Command::new("git")
            .args(["config", "--global", "--get", "user.email"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert_eq!(
            !scoped.is_empty(),
            global_set,
            "reading your own config is broken, so your settings would be silently dropped"
        );
    }

    /// A login kept for one site (`credential.https://github.com.helper`,
    /// as `gh auth setup-git` writes it) must still answer: the reset that
    /// drops a repository's helpers empties git's whole list for the URL,
    /// so without yours added back under its own key a push over HTTPS has
    /// nobody to ask ("could not read Username"). Checked with git itself,
    /// asked for a login the way a push asks.
    #[test]
    fn a_login_kept_for_one_site_still_answers_and_a_repository_s_never_does() {
        let t = tempfile::tempdir().unwrap();
        let sh = |dir: &Path, args: &[&str]| {
            let s = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(s.status.success(), "{args:?}: {}", String::from_utf8_lossy(&s.stderr));
        };
        // Your config, as the GitHub CLI leaves it, with a helper for every
        // site in a file it includes.
        let (global, included) = (t.path().join("gitconfig"), t.path().join("more"));
        std::fs::write(&included, "[credential]\n\thelper = \"!f() { echo username=anywhere; echo password=p1; }; f\"\n").unwrap();
        std::fs::write(
            &global,
            format!(
                "[include]\n\tpath = {}\n[credential \"https://github.com\"]\n\thelper =\n\thelper = \"!f() {{ echo username=you; echo password=p2; }}; f\"\n",
                included.display()
            ),
        )
        .unwrap();
        // A repository that names helpers of its own, of both kinds.
        let repo = t.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        sh(&repo, &["init", "-q"]);
        let marker = t.path().join("config-ran");
        let fire = format!("!f() {{ touch {}; echo username=theirs; echo password=p3; }}; f", marker.display());
        sh(&repo, &["config", "credential.helper", &fire]);
        sh(&repo, &["config", "credential.https://github.com.helper", &fire]);
        sh(&repo, &["config", "credential.https://example.org.helper", &fire]);

        let env = [("GIT_CONFIG_GLOBAL", global.to_str().unwrap()), ("GIT_CONFIG_SYSTEM", "/dev/null")];
        let config = build_safe_git_config(&env);
        let login_for = |host: &str| -> String {
            use std::io::Write;
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C").arg(&repo);
            for c in &config {
                cmd.arg("-c").arg(c);
            }
            let mut child = cmd
                .args(["credential", "fill"])
                .envs(env)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(format!("protocol=https\nhost={host}\n\n").as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
        };
        // GitHub: your helper for it, alone (your own empty value put the
        // one for every site aside, as it does without the host).
        let github = login_for("github.com");
        assert!(github.contains("username=you") && github.contains("password=p2"), "your GitHub login was dropped: {github}");
        // Any other site: your helper for every site, from the included file.
        let other = login_for("example.org");
        assert!(other.contains("username=anywhere"), "your login for other sites was dropped: {other}");
        // The repository's helpers answered for neither, and never ran.
        assert!(!github.contains("theirs") && !other.contains("theirs"), "{github}{other}");
        assert!(!marker.exists(), "a helper the repository's config named ran");
    }
}
