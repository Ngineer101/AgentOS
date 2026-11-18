//! Installing command-line tools on the user's computer, for an agent, after
//! the user approves (the gateway's built-in `agenthost__install_tool`).
//!
//! Sessions run sandboxed and can't change anything outside their workspace,
//! so a tool the user wants installed for themselves goes through here: the
//! agent asks, the user sees the exact command and approves or denies it on
//! the desktop or a phone, and only then does the host run it, as the user,
//! outside the sandbox.
//!
//! What may run is deliberately narrow: one of a few package managers, a
//! registry package name (no URLs, paths, git specs or flags) and an optional
//! plain version, run as an argument list, never through a shell.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest an install may take.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Output kept for the agent and the transcript.
const OUTPUT_LIMIT: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Npm,
    Pipx,
    Uv,
    Cargo,
    Brew,
    Go,
}

impl Manager {
    pub const ALL: [Manager; 6] = [
        Manager::Npm,
        Manager::Pipx,
        Manager::Uv,
        Manager::Cargo,
        Manager::Brew,
        Manager::Go,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Manager::Npm => "npm",
            Manager::Pipx => "pipx",
            Manager::Uv => "uv",
            Manager::Cargo => "cargo",
            Manager::Brew => "brew",
            Manager::Go => "go",
        }
    }

    pub fn parse(s: &str) -> Option<Manager> {
        Manager::ALL.into_iter().find(|m| m.name() == s.trim())
    }

    /// The program it runs.
    fn program(self) -> &'static str {
        self.name()
    }
}

/// An install the user is asked to approve.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub manager: Manager,
    pub package: String,
    pub version: Option<String>,
    /// Program and arguments, exactly as they will run.
    pub argv: Vec<String>,
}

impl Plan {
    /// The command as the user reads it.
    pub fn command(&self) -> String {
        self.argv.join(" ")
    }

    /// "Install agent-desktop 1.3.1 with npm, on this computer".
    pub fn summary(&self) -> String {
        match &self.version {
            Some(v) => format!(
                "Install {} {v} with {}, on this computer",
                self.package,
                self.manager.name()
            ),
            None => format!(
                "Install {} with {}, on this computer",
                self.package,
                self.manager.name()
            ),
        }
    }
}

/// A registry package name: letters, digits and `@ / . _ + -`, not starting
/// with `-`, `.` or `/` (so never a flag, a path or a URL: no `:`).
fn valid_package(p: &str) -> bool {
    let ok_chars = p
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@/._+-".contains(c));
    ok_chars
        && !p.is_empty()
        && p.len() <= 214
        && !p.starts_with(['-', '.', '/'])
        && !p.contains("..")
        && !p.contains("//")
}

/// A plain version ("1.3.1", "0.4.0-beta.2", "latest"): no ranges, no spaces.
fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false)
        && v.chars().all(|c| c.is_ascii_alphanumeric() || "._+~-".contains(c))
}

/// Check a request and build the exact command for it.
pub fn plan(manager: &str, package: &str, version: Option<&str>) -> Result<Plan, String> {
    let m = Manager::parse(manager).ok_or_else(|| {
        format!(
            "unknown package manager {manager:?}; use one of: {}",
            Manager::ALL.map(|m| m.name()).join(", ")
        )
    })?;
    let package = package.trim();
    if !valid_package(package) {
        return Err(format!(
            "{package:?} is not a package name: use the registry name only (no URLs, paths, git specs or flags)"
        ));
    }
    let version = version.map(str::trim).filter(|v| !v.is_empty());
    if let Some(v) = version {
        if !valid_version(v) {
            return Err(format!(
                "{v:?} is not a plain version (e.g. 1.3.1); ranges aren't accepted"
            ));
        }
    }
    let p = package.to_string();
    let argv: Vec<String> = match m {
        Manager::Npm => {
            let spec = match version {
                Some(v) => format!("{p}@{v}"),
                None => p.clone(),
            };
            vec!["npm".into(), "install".into(), "--global".into(), spec]
        }
        Manager::Pipx => {
            let spec = match version {
                Some(v) => format!("{p}=={v}"),
                None => p.clone(),
            };
            vec!["pipx".into(), "install".into(), spec]
        }
        Manager::Uv => {
            let spec = match version {
                Some(v) => format!("{p}=={v}"),
                None => p.clone(),
            };
            vec!["uv".into(), "tool".into(), "install".into(), spec]
        }
        Manager::Cargo => {
            if p.contains(['/', '@']) {
                return Err(format!("{p:?} is not a crate name"));
            }
            let mut a = vec!["cargo".into(), "install".into(), "--locked".into(), p.clone()];
            if let Some(v) = version {
                a.push("--version".into());
                a.push(v.to_string());
            }
            a
        }
        Manager::Brew => {
            if version.is_some() {
                return Err("brew installs the current version; leave the version out".into());
            }
            vec!["brew".into(), "install".into(), p.clone()]
        }
        Manager::Go => {
            // A module path ("github.com/x/y/cmd/z"), installed at a version.
            let first = p.split('/').next().unwrap_or_default();
            if !first.contains('.') || p.contains('@') {
                return Err(format!(
                    "{p:?} is not a Go module path (like github.com/owner/repo/cmd/tool)"
                ));
            }
            vec![
                "go".into(),
                "install".into(),
                format!("{p}@{}", version.unwrap_or("latest")),
            ]
        }
    };
    Ok(Plan {
        manager: m,
        package: p,
        version: version.map(str::to_string),
        argv,
    })
}

/// Where a program is: `bin_dir` when given (tests), else the host's PATH.
fn find(program: &str, bin_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(d) = bin_dir {
        let p = d.join(program);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(program))
            .find(|p| p.is_file())
    })
}

/// The package managers installed on this computer.
pub fn available(bin_dir: Option<&Path>) -> Vec<Manager> {
    Manager::ALL
        .into_iter()
        .filter(|m| find(m.program(), bin_dir).is_some())
        .collect()
}

/// The outcome of running a plan.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub ok: bool,
    /// Combined stdout and stderr, tail-truncated.
    pub output: String,
}

/// Run an approved plan, as the user, outside any sandbox.
pub async fn run(plan: &Plan, bin_dir: Option<&Path>) -> Outcome {
    let Some(program) = find(plan.manager.program(), bin_dir) else {
        return Outcome {
            ok: false,
            output: format!("{} is not installed on this computer", plan.manager.name()),
        };
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&plan.argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if plan.manager == Manager::Brew {
        // Install what was approved, without first updating every formula.
        cmd.env("HOMEBREW_NO_AUTO_UPDATE", "1");
    }
    let result = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output()).await;
    match result {
        Err(_) => Outcome {
            ok: false,
            output: format!(
                "stopped after {} minutes",
                INSTALL_TIMEOUT.as_secs() / 60
            ),
        },
        Ok(Err(e)) => Outcome {
            ok: false,
            output: format!("could not run {}: {e}", plan.manager.name()),
        },
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            if text.len() > OUTPUT_LIMIT {
                let mut cut = text.len() - OUTPUT_LIMIT;
                while !text.is_char_boundary(cut) {
                    cut += 1;
                }
                text = format!("[… earlier output left out]\n{}", &text[cut..]);
            }
            Outcome {
                ok: out.status.success(),
                output: text.trim().to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_exact_commands() {
        let p = plan("npm", "agent-desktop", Some("1.3.1")).unwrap();
        assert_eq!(p.command(), "npm install --global agent-desktop@1.3.1");
        assert_eq!(p.summary(), "Install agent-desktop 1.3.1 with npm, on this computer");
        assert_eq!(
            plan("npm", "@vercel/agent", None).unwrap().command(),
            "npm install --global @vercel/agent"
        );
        assert_eq!(plan("pipx", "httpie", Some("3.2.4")).unwrap().command(), "pipx install httpie==3.2.4");
        assert_eq!(plan("uv", "ruff", None).unwrap().command(), "uv tool install ruff");
        assert_eq!(
            plan("cargo", "ripgrep", Some("14.1.1")).unwrap().command(),
            "cargo install --locked ripgrep --version 14.1.1"
        );
        assert_eq!(plan("brew", "jq", None).unwrap().command(), "brew install jq");
        assert_eq!(
            plan("go", "github.com/charmbracelet/glow", None).unwrap().command(),
            "go install github.com/charmbracelet/glow@latest"
        );
    }

    #[test]
    fn refuses_anything_but_a_package_name() {
        for bad in [
            "--registry=https://evil.example",
            "-g",
            "git+https://github.com/x/y",
            "https://evil.example/x.tgz",
            "file:../x",
            "../x",
            "/tmp/x",
            "a b",
            "x;rm -rf ~",
            "x$(id)",
            "",
        ] {
            assert!(plan("npm", bad, None).is_err(), "{bad}");
        }
        for bad in [">=1.0", "1.0 || 2", "-1", "^1.2", "1 2"] {
            assert!(plan("npm", "x", Some(bad)).is_err(), "{bad}");
        }
        assert!(plan("make", "x", None).is_err());
        assert!(plan("cargo", "@scope/x", None).is_err());
        assert!(plan("brew", "jq", Some("1.7")).is_err());
        assert!(plan("go", "glow", None).is_err());
    }
}
