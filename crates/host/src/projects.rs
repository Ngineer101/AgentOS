//! Projects: new workspaces made from any client, phones included, but only
//! inside the project folders chosen on the desktop (default `~/Code`).
//!
//! Adding an arbitrary folder as a workspace stays an admin action: a
//! workspace is readable and writable to agents. Here a phone can only browse
//! inside the project folders and pick or create a folder there; the usual
//! workspace checks (no home, secrets, host data, agent logins) still apply.

use crate::host::Host;
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::*;
use std::path::{Path, PathBuf};

const ROOTS_KEY: &str = "project_roots";
/// Most project folders, and most subfolders listed at once.
const MAX_ROOTS: usize = 10;
const MAX_LISTED: usize = 500;
/// Longest new folder name, in characters.
const MAX_NAME: usize = 100;

/// The project folders as configured (absolute; they may not exist yet).
pub fn roots(h: &Host) -> Vec<PathBuf> {
    match h.ctx.store.setting_get::<Vec<String>>(ROOTS_KEY) {
        Ok(Some(v)) => v.into_iter().map(PathBuf::from).collect(),
        _ => vec![h.ctx.cfg.home_dir.join("Code")],
    }
}

/// Symlinks resolved, also for a path that doesn't exist yet: its nearest
/// existing folder is resolved and the rest appended.
fn real(p: &Path) -> PathBuf {
    if let Ok(r) = std::fs::canonicalize(p) {
        return r;
    }
    let mut rest = vec![];
    let mut cur = p;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            rest.push(name.to_os_string());
        }
        if let Ok(r) = std::fs::canonicalize(parent) {
            return rest.iter().rev().fold(r, |acc, n| acc.join(n));
        }
        cur = parent;
    }
    p.to_path_buf()
}

fn expand(raw: &str, home: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// The configured root `path` is, or is inside (both with symlinks resolved).
fn root_of(h: &Host, path: &Path) -> Option<PathBuf> {
    roots(h)
        .into_iter()
        .map(|r| real(&r))
        .find(|r| path.starts_with(r))
}

fn existing_folder(raw: &str) -> Result<PathBuf, RpcError> {
    let p = PathBuf::from(raw);
    if !p.is_absolute() {
        return Err(RpcError::invalid_params("path must be absolute"));
    }
    let p = std::fs::canonicalize(&p)
        .map_err(|e| RpcError::invalid_params(format!("{}: {e}", p.display())))?;
    if !p.is_dir() {
        return Err(RpcError::invalid_params(format!("{} is not a folder", p.display())));
    }
    Ok(p)
}

fn outside() -> RpcError {
    RpcError::invalid_params(
        "that folder is outside the project folders (set them on the desktop: Workspaces)",
    )
}

pub fn browse(h: &Host, p: ProjectBrowseParams) -> Result<ProjectBrowse, RpcError> {
    let roots = roots(h);
    let root_strings: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
    let workspaces = h.ctx.store.workspace_list().map_err(internal)?;
    let ws_for = |path: &Path| {
        let s = path.display().to_string();
        workspaces
            .iter()
            .find(|w| w.path == s)
            .map(|w| w.workspace_id.clone())
    };
    let Some(raw) = p.path else {
        let folders = roots
            .iter()
            .map(|r| ProjectFolder {
                name: r.display().to_string(),
                path: r.display().to_string(),
                workspace_id: ws_for(&real(r)),
            })
            .collect();
        return Ok(ProjectBrowse {
            roots: root_strings,
            path: None,
            parent: None,
            folders,
        });
    };
    // A project folder that doesn't exist yet lists as empty.
    if let Some(r) = roots.iter().find(|r| r.as_os_str() == raw.as_str()) {
        if !r.exists() {
            return Ok(ProjectBrowse {
                roots: root_strings,
                path: Some(raw),
                parent: None,
                folders: vec![],
            });
        }
    }
    let path = existing_folder(&raw)?;
    let root = root_of(h, &path).ok_or_else(outside)?;
    let mut folders = vec![];
    let entries = std::fs::read_dir(&path)
        .map_err(|e| RpcError::invalid_params(format!("{}: {e}", path.display())))?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let full = real(&e.path());
        // Folders only, and a symlink may not lead out of the project folder.
        if !full.is_dir() || !full.starts_with(&root) {
            continue;
        }
        folders.push(ProjectFolder {
            workspace_id: ws_for(&full),
            name,
            path: full.display().to_string(),
        });
    }
    folders.sort_by_key(|f| f.name.to_lowercase());
    folders.truncate(MAX_LISTED);
    let parent = if path == root {
        None
    } else {
        path.parent().map(|p| p.display().to_string())
    };
    Ok(ProjectBrowse {
        roots: root_strings,
        path: Some(path.display().to_string()),
        parent,
        folders,
    })
}

fn check_name(name: &str) -> Result<String, RpcError> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_NAME
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\0');
    if !ok {
        return Err(RpcError::invalid_params(format!(
            "a folder name has 1 to {MAX_NAME} characters, no \"/\", and doesn't start with \".\""
        )));
    }
    Ok(name.to_string())
}

pub fn create(h: &Host, p: ProjectCreateParams) -> Result<Workspace, RpcError> {
    let target = match (p.path, p.parent, p.name) {
        (Some(path), None, None) => {
            let path = existing_folder(&path)?;
            let root = root_of(h, &path).ok_or_else(outside)?;
            if path == root {
                return Err(RpcError::invalid_params(
                    "pick a folder inside the project folder, not the project folder itself",
                ));
            }
            path
        }
        (None, Some(parent), Some(name)) => {
            let name = check_name(&name)?;
            // A project folder that doesn't exist yet is created first.
            let configured = roots(h)
                .into_iter()
                .find(|r| r.as_os_str() == parent.as_str());
            let parent = match configured {
                Some(r) if !r.exists() => {
                    std::fs::create_dir_all(&r).map_err(|e| {
                        RpcError::invalid_params(format!("create {}: {e}", r.display()))
                    })?;
                    real(&r)
                }
                _ => existing_folder(&parent)?,
            };
            root_of(h, &parent).ok_or_else(outside)?;
            let target = parent.join(&name);
            if target.exists() {
                return Err(RpcError::invalid_state(format!(
                    "{} already exists; pick it from the list",
                    target.display()
                )));
            }
            std::fs::create_dir(&target)
                .map_err(|e| RpcError::invalid_params(format!("create {}: {e}", target.display())))?;
            target
        }
        _ => {
            return Err(RpcError::invalid_params(
                "give either path, or parent and name",
            ))
        }
    };
    // Already a workspace: use it.
    if let Some(w) = h
        .ctx
        .store
        .workspace_by_path(&target.display().to_string())
        .map_err(internal)?
    {
        return Ok(w);
    }
    crate::api::workspace_add(
        h,
        WorkspaceAddParams {
            path: target.display().to_string(),
            name: None,
            kind: None,
            network: None,
        },
    )
}

pub fn set_roots(h: &Host, p: ProjectRootsParams) -> Result<Vec<String>, RpcError> {
    let cfg = &h.ctx.cfg;
    if p.roots.len() > MAX_ROOTS {
        return Err(RpcError::invalid_params(format!("at most {MAX_ROOTS} project folders")));
    }
    let home = real(&cfg.home_dir);
    let mut out: Vec<PathBuf> = vec![];
    for raw in &p.roots {
        let dir = expand(raw, &cfg.home_dir);
        if !dir.is_absolute() {
            return Err(RpcError::invalid_params(format!(
                "{raw}: a project folder must be an absolute path (or start with ~/)"
            )));
        }
        let dir = real(&dir);
        if home.starts_with(&dir) {
            return Err(RpcError::invalid_params(format!(
                "{}: holds your home folder; pick a folder of projects, such as ~/Code",
                dir.display()
            )));
        }
        for protected in [
            cfg.data_dir.clone(),
            cfg.runtime_dir.clone(),
            cfg.scratch_dir(),
            cfg.session_tools_dir(),
        ] {
            if crate::profiles::overlaps(&dir, &real(&protected)) {
                return Err(RpcError::invalid_params(format!(
                    "{}: may not contain or be inside the host's own data",
                    dir.display()
                )));
            }
        }
        for secret in crate::sandbox::SECRET_DIRS {
            if crate::profiles::overlaps(&dir, &home.join(secret)) {
                return Err(RpcError::invalid_params(format!(
                    "{}: may not contain or be inside a secrets folder (~/{secret})",
                    dir.display()
                )));
            }
        }
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    let strings: Vec<String> = out.iter().map(|d| d.display().to_string()).collect();
    h.ctx.store.setting_set(ROOTS_KEY, &strings).map_err(internal)?;
    Ok(strings)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}
