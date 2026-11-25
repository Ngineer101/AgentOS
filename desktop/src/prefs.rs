//! What the app remembers between launches on this computer: which sidebar
//! groups are collapsed, how wide the sidebar and the right panel are, and which host to connect to when one was paired.
//! A small JSON file in the platform's config folder; a missing or unreadable
//! one is the defaults. Pins live on the host (`session.pin`), so every
//! device sees them.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize, PartialEq, Debug)]
pub struct Prefs {
    /// Workspace ids whose sidebar group is collapsed.
    #[serde(default)]
    pub collapsed: HashSet<String>,
    /// Connect to the paired remote host (whose keys sit in `remote.json` in
    /// the same folder) rather than this computer's own host. Set by pairing
    /// while no local host answers, or by choosing "Connect to …"; a laptop's
    /// normal state. "Use this computer" clears it.
    #[serde(default)]
    pub prefer_remote: bool,
    /// The sidebar's width, once its edge has been dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_w: Option<f32>,
    /// The right panel's width with the terminal in it, once its edge has been dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_w: Option<f32>,
}

/// `~/Library/Application Support/AgentHost/` on macOS,
/// `$XDG_CONFIG_HOME/agenthost/` (or `~/.config/…`) elsewhere;
/// `$AGENTHOST_DESKTOP_DIR` overrides both, for tests.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("AGENTHOST_DESKTOP_DIR") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/AgentHost"));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| home.map(|h| h.join(".config")));
    config.map(|c| c.join("agenthost"))
}

/// The prefs file, `desktop.json` in the config folder.
fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("desktop.json"))
}

pub fn load() -> Prefs {
    path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Write the prefs; failures are quiet (it only means the next launch forgets).
pub fn save(p: &Prefs) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(b) = serde_json::to_vec_pretty(p) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_read_back_and_tolerate_old_files() {
        let p = Prefs { collapsed: ["ws_1".to_string()].into(), prefer_remote: true, sidebar_w: Some(320.0), panel_w: Some(600.0) };
        let back: Prefs = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        let empty: Prefs = serde_json::from_str("{}").unwrap();
        assert!(empty.collapsed.is_empty() && !empty.prefer_remote && empty.sidebar_w.is_none() && empty.panel_w.is_none(), "a file from before these fields is fine");
    }
}
