//! Agent profiles: more than one login of the same agent (a work and a
//! personal subscription, say), each in its own config folder.
//!
//! A profile's folder becomes writable inside the sandbox of every session
//! that uses it and is hidden from every other session, so it must not
//! overlap anything else the sandbox protects or exposes: your home folder,
//! secrets folders, the host's own data, another login, or a workspace.

use crate::agents;
use crate::host::Host;
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::{AgentKind, AgentProfile, AgentProfileAddParams};
use std::path::{Path, PathBuf};

/// Longest profile name, in characters.
pub const MAX_NAME: usize = 40;

/// One path inside the other, or the same.
pub fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Symlinks resolved where the path exists (`/tmp` → `/private/tmp` on macOS).
fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `~` and `~/…` against the host user's home.
fn expand(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Every profile's folder, for checks elsewhere (workspaces).
pub fn folders(h: &Host) -> Vec<PathBuf> {
    h.ctx
        .store
        .profile_list()
        .unwrap_or_default()
        .into_iter()
        .map(|p| PathBuf::from(p.config_dir))
        .collect()
}

pub fn add(h: &Host, p: AgentProfileAddParams) -> Result<AgentProfile, RpcError> {
    let cfg = &h.ctx.cfg;
    if !h.agent_kinds().contains(&p.agent) {
        return Err(RpcError::invalid_params(format!(
            "{} is not offered on this host",
            p.agent.display_name()
        )));
    }
    if p.agent.profile_env().is_none() {
        return Err(RpcError::invalid_params(format!(
            "{} can't use a separate config folder on this system",
            p.agent.display_name()
        )));
    }
    let name = p.name.trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(RpcError::invalid_params(format!(
            "a profile name has 1 to {MAX_NAME} characters"
        )));
    }
    if name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case(p.agent.as_str()) {
        return Err(RpcError::invalid_params(format!(
            "\"{name}\" is the name of the default login; pick another"
        )));
    }

    let raw = expand(p.config_dir.trim(), &cfg.home_dir);
    if !raw.is_absolute() {
        return Err(RpcError::invalid_params(
            "the config folder must be an absolute path (or start with ~/)",
        ));
    }
    let dir = std::fs::canonicalize(&raw)
        .map_err(|e| RpcError::invalid_params(format!("{}: {e}", raw.display())))?;
    if !dir.is_dir() {
        return Err(RpcError::invalid_params(format!(
            "{} is not a folder",
            dir.display()
        )));
    }
    check_folder(h, p.agent, &dir).map_err(RpcError::invalid_params)?;

    let existing = h.ctx.store.profile_list().map_err(internal)?;
    if existing
        .iter()
        .any(|x| x.agent == p.agent && x.name.eq_ignore_ascii_case(&name))
    {
        return Err(RpcError::invalid_state(format!(
            "{} already has a profile named {name}",
            p.agent.display_name()
        )));
    }
    let profile = AgentProfile {
        profile_id: crate::util::new_id("prof"),
        agent: p.agent,
        name,
        config_dir: dir.display().to_string(),
        created_at: crate::util::now_ms(),
    };
    h.ctx.store.profile_insert(&profile).map_err(internal)?;
    Ok(profile)
}

pub fn remove(h: &Host, id: &str) -> Result<(), RpcError> {
    let live = h
        .ctx
        .store
        .session_list(None)
        .map_err(internal)?
        .into_iter()
        .filter(|s| !s.state.is_terminal() && s.profile_id.as_deref() == Some(id))
        .count();
    if live > 0 {
        return Err(RpcError::invalid_state(format!(
            "end the {live} live session(s) using this profile first"
        )));
    }
    if !h.ctx.store.profile_remove(id).map_err(internal)? {
        return Err(RpcError::not_found("no such agent profile"));
    }
    Ok(())
}

/// Why `dir` (absolute, symlinks resolved) can't be a profile folder.
fn check_folder(h: &Host, agent: AgentKind, dir: &Path) -> Result<(), String> {
    let cfg = &h.ctx.cfg;
    let home = real(&cfg.home_dir);
    if home.starts_with(dir) {
        return Err(format!(
            "{} holds your home folder; use a folder of its own, such as ~/.claude-work",
            dir.display()
        ));
    }
    let mut host_data = vec![cfg.data_dir.clone(), cfg.runtime_dir.clone()];
    if let Some(p) = cfg.socket_path.parent() {
        host_data.push(p.to_path_buf());
    }
    for p in host_data {
        if overlaps(dir, &real(&p)) {
            return Err("the config folder may not contain or be inside the host's own data".into());
        }
    }
    for secret in crate::sandbox::SECRET_DIRS {
        if overlaps(dir, &home.join(secret)) {
            return Err(format!(
                "the config folder may not contain or be inside a secrets folder (~/{secret})"
            ));
        }
    }
    // Every agent's default login and state.
    let mut defaults: Vec<(AgentKind, PathBuf)> = agents::all_credential_paths(&cfg.home_dir);
    for kind in AgentKind::REAL.iter().copied().chain([AgentKind::Fake]) {
        for p in agents::spec(kind, cfg).state_paths {
            defaults.push((kind, p));
        }
    }
    for (kind, p) in defaults {
        let p = real(&p);
        if overlaps(dir, &p) {
            let hint = if kind == agent {
                " (that login is already offered as the default one)"
            } else {
                ""
            };
            return Err(format!(
                "the config folder overlaps {}, used by {}'s default login{hint}",
                p.display(),
                kind.display_name()
            ));
        }
    }
    for other in h.ctx.store.profile_list().map_err(|e| e.to_string())? {
        if overlaps(dir, &real(Path::new(&other.config_dir))) {
            return Err(format!(
                "the config folder overlaps {}, used by the {} profile {}",
                other.config_dir,
                other.agent.display_name(),
                other.name
            ));
        }
    }
    // A workspace is writable to its sessions: a login inside one (or a
    // workspace inside a login) would be readable by any agent there.
    for w in h.ctx.store.workspace_list().map_err(|e| e.to_string())? {
        if overlaps(dir, &real(Path::new(&w.path))) {
            return Err(format!(
                "the config folder overlaps the workspace {} ({})",
                w.name, w.path
            ));
        }
    }
    Ok(())
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_and_expand() {
        assert!(overlaps(Path::new("/a/b"), Path::new("/a")));
        assert!(overlaps(Path::new("/a"), Path::new("/a/b")));
        assert!(overlaps(Path::new("/a"), Path::new("/a")));
        assert!(!overlaps(Path::new("/a/b"), Path::new("/a/bc")));
        let home = Path::new("/home/u");
        assert_eq!(expand("~/.claude-work", home), PathBuf::from("/home/u/.claude-work"));
        assert_eq!(expand("~", home), PathBuf::from("/home/u"));
        assert_eq!(expand("/x", home), PathBuf::from("/x"));
    }
}
