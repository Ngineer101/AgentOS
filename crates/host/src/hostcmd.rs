//! Running a command-line program for an agent, as the user, outside the
//! sandbox, after the user approves (the gateway's built-in
//! `agenthost__run_command`).
//!
//! Sessions can't read the user's logins (`~/.config/gh`, cloud CLIs'
//! credentials, the keyring), so `gh pr edit`, `gh issue create`, a deploy
//! CLI and the like fail inside the sandbox. Here the host runs them
//! instead: the agent asks with an argument list, the user sees the exact
//! command (and what goes to its standard input) and approves or denies it,
//! and only then does the host run it in the session's folder.
//!
//! Never through a shell; the program is a bare name found on the host's
//! own `PATH`, never in a folder a session can write. The repository's
//! hooks and fsmonitor never run (git's environment configuration), and
//! nothing prompts. The user can stop being asked for a command prefix
//! ("gh pr edit"), but never for a shell, an interpreter or a build tool,
//! which would let a session run anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest a command may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Output kept for the agent and the transcript (the end, where errors are).
const OUTPUT_LIMIT: usize = 16 * 1024;
const MAX_ARGS: usize = 256;
const MAX_ARGV_BYTES: usize = 64 * 1024;
const MAX_STDIN_BYTES: usize = 256 * 1024;
/// Words after the program a remembered command may have ("pr edit").
const RULE_WORDS: usize = 2;

/// Programs that run whatever they're given, or the project's own code
/// (scripts, build files, tests): approving one once is fine, but never
/// "don't ask again", which would let a session run anything as the user.
const NEVER_REMEMBERED: &[&str] = &[
    // Shells and wrappers that run another program.
    "sh", "bash", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "nu", "pwsh", "elvish", "xonsh",
    "env", "sudo", "doas", "su", "pkexec", "run0", "xargs", "nohup", "time", "timeout", "nice", "ionice",
    "setsid", "stdbuf", "chroot", "unshare", "nsenter", "flatpak-spawn", "systemd-run", "watch",
    "parallel", "script", "expect", "ssh", "open", "xdg-open", "osascript", "find", "busybox", "exec",
    // Interpreters.
    "python", "python3", "python2", "node", "nodejs", "deno", "bun", "ruby", "perl", "php", "lua",
    "luajit", "tclsh", "wish", "R", "Rscript", "julia", "java", "dotnet", "swift", "awk", "gawk",
    // Package managers and build tools (scripts, build files, tests).
    "npm", "npx", "pnpm", "pnpx", "yarn", "bunx", "uv", "uvx", "pip", "pip3", "pipx", "poetry", "pdm",
    "hatch", "tox", "nox", "pytest", "make", "gmake", "just", "cmake", "ninja", "meson", "cargo",
    "rustc", "go", "gradle", "gradlew", "mvn", "ant", "sbt", "rake", "bundle", "gem", "composer",
    "mix", "swiftc", "xcodebuild", "bazel", "buck", "nix", "nix-shell", "docker", "podman",
    "docker-compose", "brew", "git",
    // Deploy and infrastructure tools that run commands from the project's config.
    "terraform", "tofu", "pulumi", "cdk", "serverless", "sls", "wrangler", "firebase", "netlify",
    // Editors, pagers and file tools (they read, write or run anything named).
    "vi", "vim", "nvim", "emacs", "nano", "less", "more", "man", "cat", "head", "tail", "cp", "mv",
    "rm", "dd", "ln", "chmod", "chown", "install", "tee", "tar", "zip", "unzip", "rsync", "scp",
    "sftp", "curl", "wget", "nc", "ncat", "socat",
    // Secrets.
    "openssl", "gpg", "security", "secret-tool", "pass", "op", "bw", "keyring", "aws-vault",
];

/// Subcommands that show or change a login or run extensions: always ask.
const NEVER_REMEMBERED_PREFIXES: &[&[&str]] = &[
    &["gh", "auth"], &["gh", "config"], &["gh", "alias"], &["gh", "extension"], &["gh", "ext"],
    &["glab", "auth"], &["glab", "config"], &["gcloud", "auth"], &["gcloud", "config"],
    &["aws", "configure"], &["aws", "sts"], &["az", "login"], &["az", "account"], &["doctl", "auth"],
    &["fly", "auth"], &["flyctl", "auth"], &["heroku", "auth"], &["vercel", "env"], &["vercel", "build"],
    &["kubectl", "config"],
];

/// A command the host will run, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub dir: PathBuf,
    /// The resolved program.
    pub program: PathBuf,
    /// The argument list as asked, `argv[0]` the program's bare name.
    pub argv: Vec<String>,
    pub stdin: Option<String>,
}

impl Plan {
    /// The command as the user reads it, quoted as a shell would need.
    pub fn command(&self) -> String {
        display(&self.argv)
    }

    /// "Run gh pr edit on this computer".
    pub fn summary(&self) -> String {
        let shown = rule_for(&self.argv).unwrap_or_else(|| vec![self.argv[0].clone()]);
        format!("Run {} on this computer", shown.join(" "))
    }

    /// The prefix the user may let run without asking from now on, if any.
    pub fn rule(&self) -> Option<Vec<String>> {
        rule_for(&self.argv).filter(|r| rememberable(r))
    }
}

/// Quote an argument list for reading (and pasting into a shell).
pub fn display(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

fn quote(a: &str) -> String {
    let plain = !a.is_empty()
        && a.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        a.to_string()
    } else if a.chars().any(|c| c.is_control()) {
        // Newlines and escape codes shown as such, so they can't disguise the command.
        let mut out = String::from("$'");
        for c in a.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\'' => out.push_str("\\'"),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('\'');
        out
    } else {
        format!("'{}'", a.replace('\'', r"'\''"))
    }
}

/// A subcommand word: `pr`, `edit`, `issue`, `deploy` — not a flag, path,
/// number, URL or value.
fn is_word(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase())
        && s.len() <= 32
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A program name: no path, nothing a shell would read specially.
fn is_program(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with(['-', '.'])
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_+.-".contains(c))
}

/// The program and the subcommand words that lead the command, up to the
/// first flag or value: `gh pr edit 6 --body-file -` → `gh pr edit`. None
/// without a subcommand (a rule for the bare program would allow anything).
pub fn rule_for(argv: &[String]) -> Option<Vec<String>> {
    let program = argv.first().filter(|p| is_program(p))?;
    let words: Vec<String> = argv[1..].iter().take_while(|a| is_word(a)).take(RULE_WORDS).cloned().collect();
    if words.is_empty() {
        return None;
    }
    let mut r = vec![program.clone()];
    r.extend(words);
    Some(r)
}

/// Whether a prefix may be let run without asking.
pub fn rememberable(rule: &[String]) -> bool {
    match rule.split_first() {
        Some((program, words)) => {
            is_program(program)
                && !NEVER_REMEMBERED.contains(&base_name(program))
                && !NEVER_REMEMBERED_PREFIXES.iter().any(|p| p[0] == program && words.first().is_some_and(|w| w == p[1]))
                && (1..=RULE_WORDS).contains(&words.len())
                && words.iter().all(|w| is_word(w))
        }
        None => false,
    }
}

/// `python3.12` and `node22` are still python and node.
fn base_name(program: &str) -> &str {
    let p = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if p.is_empty() {
        program
    } else {
        p
    }
}

/// Whether a remembered prefix covers this command.
pub fn matches(rule: &[String], argv: &[String]) -> bool {
    !rule.is_empty() && argv.len() >= rule.len() && rule.iter().zip(argv).all(|(r, a)| r == a)
}

fn runnable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// The first argument naming a file outside `dir` (an absolute path, `~`,
/// `..`, or a link out of it), also after `--flag=` or `@` (`--body-file=/x`,
/// `-F f=@/x`). A command the user let run without asking still asks when it
/// names one: `gh pr edit --body-file ~/.ssh/id_ed25519` would post the key.
pub fn outside_file(dir: &Path, argv: &[String]) -> Option<String> {
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let escapes = |c: &str| -> bool {
        if c.is_empty() {
            return false;
        }
        if c.starts_with(['/', '~']) || c.split(['/', '\\']).any(|part| part == "..") {
            return true;
        }
        let p = dir.join(c);
        // An existing path (or a link) that resolves out of the folder.
        match p.canonicalize() {
            Ok(real) => !real.starts_with(&root),
            Err(_) => p.symlink_metadata().is_ok(),
        }
    };
    argv[1..].iter().find(|a| {
        let mut candidates = vec![a.as_str()];
        if let Some((_, v)) = a.split_once('=') {
            candidates.push(v);
        }
        candidates.extend(candidates.clone().iter().filter_map(|c| c.strip_prefix('@')));
        candidates.into_iter().any(escapes)
    }).cloned()
}

/// Where a program is: in `bin_dir` when given (tests), else on the host's
/// `PATH`, skipping relative entries and any folder under `unsafe_dirs`
/// (folders sessions can write).
pub fn find(program: &str, bin_dir: Option<&Path>, unsafe_dirs: &[PathBuf]) -> Option<PathBuf> {
    if let Some(d) = bin_dir {
        let p = d.join(program);
        return runnable(&p).then_some(p);
    }
    find_on(program, &std::env::var_os("PATH")?, unsafe_dirs)
}

fn find_on(program: &str, path: &std::ffi::OsStr, unsafe_dirs: &[PathBuf]) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .filter(|d| {
            let d = d.canonicalize().unwrap_or_else(|_| d.clone());
            !unsafe_dirs.iter().any(|u| d.starts_with(u.canonicalize().unwrap_or_else(|_| u.clone())))
        })
        .map(|d| d.join(program))
        .find(|p| runnable(p))
}

/// Check a request and build its plan.
pub fn plan(
    dir: &Path,
    argv: &[String],
    stdin: Option<&str>,
    bin_dir: Option<&Path>,
    unsafe_dirs: &[PathBuf],
) -> Result<Plan, String> {
    let Some(program) = argv.first() else {
        return Err("give the command as a list: the program, then its arguments, e.g. [\"gh\", \"pr\", \"edit\", \"6\"]".into());
    };
    if !is_program(program) {
        return Err(format!(
            "{program:?} is not a program name; give a program on the user's PATH by name (no paths, no shell syntax)"
        ));
    }
    if argv.len() > MAX_ARGS || argv.iter().map(String::len).sum::<usize>() > MAX_ARGV_BYTES {
        return Err("the command is too long; pass large text on stdin".into());
    }
    if argv.iter().any(|a| a.contains('\0')) {
        return Err("arguments can't contain NUL".into());
    }
    if stdin.is_some_and(|s| s.len() > MAX_STDIN_BYTES) {
        return Err("stdin is longer than 256 KB".into());
    }
    let Some(path) = find(program, bin_dir, unsafe_dirs) else {
        return Err(format!("{program} is not installed on the user's computer (not on its PATH)"));
    };
    Ok(Plan {
        dir: dir.to_path_buf(),
        program: path,
        argv: argv.to_vec(),
        stdin: stdin.map(str::to_string),
    })
}

/// The outcome of running a plan.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub ok: bool,
    pub code: Option<i32>,
    /// Standard output, then standard error, keeping the end.
    pub output: String,
}

fn tail(mut text: String) -> String {
    if text.len() > OUTPUT_LIMIT {
        let mut cut = text.len() - OUTPUT_LIMIT;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text = format!("[… earlier output left out]\n{}", &text[cut..]);
    }
    text.trim().to_string()
}

/// Run an approved plan as the user, outside any sandbox, with no prompts.
/// git commands it runs get the same protection as the host's own (no
/// hooks, no fsmonitor), through git's environment configuration.
pub async fn run(plan: &Plan) -> Outcome {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.current_dir(&plan.dir)
        .args(&plan.argv[1..])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_1", "false")
        .stdin(if plan.stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let name = &plan.argv[0];
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome { ok: false, code: None, output: format!("could not run {name}: {e}") },
    };
    if let (Some(mut pipe), Some(text)) = (child.stdin.take(), plan.stdin.clone()) {
        // Written alongside the wait, so a program that doesn't read it all can't block us.
        tokio::spawn(async move {
            let _ = pipe.write_all(text.as_bytes()).await;
        });
    }
    match tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output()).await {
        Err(_) => Outcome {
            ok: false,
            code: None,
            output: format!("stopped after {} minutes", COMMAND_TIMEOUT.as_secs() / 60),
        },
        Ok(Err(e)) => Outcome { ok: false, code: None, output: format!("could not run {name}: {e}") },
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.trim().is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            Outcome { ok: out.status.success(), code: out.status.code(), output: tail(text) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rules_are_the_program_and_its_subcommand_words() {
        assert_eq!(rule_for(&v(&["gh", "pr", "edit", "6", "--body-file", "-"])), Some(v(&["gh", "pr", "edit"])));
        assert_eq!(rule_for(&v(&["gh", "issue", "create", "--title", "x"])), Some(v(&["gh", "issue", "create"])));
        assert_eq!(rule_for(&v(&["vercel", "deploy", "--prod"])), Some(v(&["vercel", "deploy"])));
        assert_eq!(rule_for(&v(&["gh", "api", "repos/o/r"])), Some(v(&["gh", "api"])));
        assert_eq!(rule_for(&v(&["gh", "--version"])), None);
        assert_eq!(rule_for(&v(&["gh"])), None);
        assert_eq!(rule_for(&v(&["./run.sh", "deploy"])), None);
    }

    #[test]
    fn shells_interpreters_and_build_tools_are_never_remembered() {
        for argv in [
            v(&["bash", "deploy", "x"]),
            v(&["python3", "manage", "x"]),
            v(&["python3.12", "manage"]),
            v(&["npm", "run", "build"]),
            v(&["make", "release"]),
            v(&["env", "gh", "pr"]),
            v(&["xargs", "sh"]),
            v(&["cargo", "run"]),
            v(&["git", "push", "origin"]),
        ] {
            let p = Plan { dir: "/".into(), program: "/bin/x".into(), argv: argv.clone(), stdin: None };
            assert_eq!(p.rule(), None, "{argv:?}");
        }
        assert!(rememberable(&v(&["gh", "pr", "edit"])));
        assert!(!rememberable(&v(&["gh"])));
        assert!(!rememberable(&v(&["gh", "pr", "edit", "6"])));
        assert!(!rememberable(&v(&["gh", "--repo"])));
    }

    #[test]
    fn login_subcommands_are_never_remembered() {
        assert!(!rememberable(&v(&["gh", "auth", "status"])));
        assert!(!rememberable(&v(&["gh", "extension", "install"])));
        assert!(!rememberable(&v(&["gcloud", "auth"])));
        assert!(rememberable(&v(&["gh", "pr", "comment"])));
    }

    #[test]
    fn finds_arguments_naming_files_outside_the_folder() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "x").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("link")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/key", dir.join("dangling")).unwrap();
        let out = |a: &[&str]| outside_file(&dir, &v(a));
        assert_eq!(out(&["gh", "pr", "edit", "6", "--body-file", "notes.md", "--title", "a/b c"]), None);
        assert_eq!(out(&["gh", "pr", "edit", "6", "--body-file", "-"]), None);
        assert_eq!(out(&["gh", "pr", "edit", "--body-file", "~/.ssh/id_ed25519"]).as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(out(&["gh", "pr", "edit", "--body-file=/etc/passwd"]).as_deref(), Some("--body-file=/etc/passwd"));
        assert_eq!(out(&["gh", "api", "-F", "f=@../secret"]).as_deref(), Some("f=@../secret"));
        assert_eq!(out(&["gh", "api", "-F", "f=@/etc/hosts"]).as_deref(), Some("f=@/etc/hosts"));
        assert_eq!(out(&["gh", "pr", "edit", "--body-file", "link"]).as_deref(), Some("link"));
        assert_eq!(out(&["gh", "pr", "edit", "--body-file", "dangling"]).as_deref(), Some("dangling"));
    }

    #[test]
    fn rules_match_by_prefix_only() {
        let r = v(&["gh", "pr", "edit"]);
        assert!(matches(&r, &v(&["gh", "pr", "edit", "6"])));
        assert!(matches(&r, &v(&["gh", "pr", "edit"])));
        assert!(!matches(&r, &v(&["gh", "pr", "merge", "6"])));
        assert!(!matches(&r, &v(&["gh", "pr"])));
        assert!(!matches(&[], &v(&["gh"])));
    }

    #[test]
    fn commands_read_as_a_shell_would_take_them() {
        assert_eq!(display(&v(&["gh", "pr", "edit", "6", "--title", "Dark bubbles", "--body", "it's"])), r#"gh pr edit 6 --title 'Dark bubbles' --body 'it'\''s'"#);
        assert_eq!(display(&v(&["echo", ""])), "echo ''");
        assert_eq!(display(&v(&["gh", "pr", "edit", "6\n\u{1b}[2Kgh pr view"])), r"gh pr edit $'6\n\u{1b}[2Kgh pr view'");
    }

    #[test]
    fn refuses_paths_and_programs_that_are_not_installed() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert!(plan(d.path(), &v(&["/bin/sh", "-c", "x"]), None, Some(&bin), &[]).is_err());
        assert!(plan(d.path(), &v(&["../gh"]), None, Some(&bin), &[]).is_err());
        assert!(plan(d.path(), &v(&["gh", "pr"]), None, Some(&bin), &[]).unwrap_err().contains("not installed"));
        assert!(plan(d.path(), &[], None, Some(&bin), &[]).is_err());
    }

    #[test]
    fn never_finds_programs_in_folders_sessions_write() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let tools = d.path().join("session-tools/bin");
        std::fs::create_dir_all(&tools).unwrap();
        let p = tools.join("ah-planted-tool");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([PathBuf::from("relative"), tools.clone(), PathBuf::from("/usr/bin")]).unwrap();
        assert_eq!(find_on("ah-planted-tool", &path, &[]), Some(p));
        assert_eq!(find_on("ah-planted-tool", &path, &[d.path().join("session-tools")]), None);
    }

    #[tokio::test]
    async fn runs_the_argument_list_with_stdin_and_no_shell() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let p = bin.join("fakecli");
        std::fs::write(&p, "#!/bin/sh\nfor a in \"$@\"; do echo \"[$a]\"; done\ncat\necho \"$GIT_CONFIG_VALUE_0\" >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plan = plan(d.path(), &v(&["fakecli", "a b", "$(touch pwned)"]), Some("from stdin\n"), Some(&bin), &[]).unwrap();
        let out = run(&plan).await;
        assert!(!out.ok);
        assert_eq!(out.code, Some(3));
        assert_eq!(out.output, "[a b]\n[$(touch pwned)]\nfrom stdin\n/dev/null");
        assert!(!d.path().join("pwned").exists());
    }
}
