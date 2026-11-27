//! The browser: a Chromium the host runs for each workspace, which agents
//! drive and you watch and use in the desktop's right panel (protocol 12).
//!
//! Each workspace has its own browser profile, kept on disk, so you sign in
//! to a site once per workspace and every session there finds you signed in.
//! The host starts the browser when something needs it and stops it when
//! nothing has for a while; the profile stays.
//!
//! The browser runs without a window. A client sees a tab by attaching to
//! it: the host sends its picture as JPEG frames (`browser.frame`), at most
//! [`BROWSER_FRAMES_AHEAD`] past the last one the client acknowledged
//! (`browser.ack`), and newer frames replace ones not yet sent. A client
//! clicks and types with `browser.input`.
//!
//! The browser's traffic goes through the host's egress proxy with the
//! workspace's network settings, as a session's does: it reaches the hosts
//! the workspace allows and nothing on this computer or its network.
//!
//! A browser signed in to your accounts can act as you: every `browser.*`
//! method needs the admin capability, which only the local socket has.

use serde::{Deserialize, Serialize};

/// Frames the host sends past a client's last acknowledgement.
pub const BROWSER_FRAMES_AHEAD: u64 = 2;
/// Most tabs one workspace's browser keeps open; opening another closes the
/// least recently used tab an agent opened.
pub const MAX_BROWSER_TABS: usize = 12;
/// A tab's page size (CSS pixels) when nobody says otherwise.
pub const BROWSER_DEFAULT_SIZE: (u32, u32) = (1280, 800);
/// A tab's page size is kept within these.
pub const BROWSER_MIN_SIZE: u32 = 200;
pub const BROWSER_MAX_SIZE: u32 = 4096;
/// Most hosts a browser remembers being refused.
pub const BROWSER_BLOCKED_KEPT: usize = 8;

/// Whether this host can run a browser, and with what.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserSupport {
    pub available: bool,
    /// The browser it runs ("/usr/bin/chromium"), or why there is none.
    pub detail: String,
}

/// One workspace's browser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserInfo {
    pub workspace_id: String,
    /// The browser is running (it starts when a tab opens).
    pub running: bool,
    /// Oldest first.
    pub tabs: Vec<BrowserTab>,
    /// Sites the browser wanted that the workspace's network rules don't include, newest
    /// first (`name`, or `name:port` off the web's own ports): the app asks whether to allow
    /// each. One leaves the list when the rules come to include it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,
    /// Why the browser could not start or stopped unexpectedly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserTab {
    pub tab_id: String,
    pub url: String,
    pub title: String,
    /// The page is loading.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loading: bool,
    /// The session whose agent opened it (or whose page opened it, as a
    /// sign-in popup); none for a tab you opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The page's size in CSS pixels.
    pub width: u32,
    pub height: u32,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// `browser.list` result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserList {
    pub support: BrowserSupport,
    /// The workspaces whose browser runs or has tabs.
    pub browsers: Vec<BrowserInfo>,
}

/// `browser.open`: a new tab in a workspace's browser, started if need be.
/// Returns the [`BrowserTab`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserOpenParams {
    pub workspace_id: String,
    /// http(s) only; a blank page when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserTabRef {
    pub tab_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserWorkspaceRef {
    pub workspace_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    Back,
    Forward,
    Reload,
    Stop,
}

/// `browser.navigate`: go to `url` (http or https; a bare host gets
/// https://), or do `action`. Exactly one of the two.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserNavigateParams {
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<BrowserAction>,
}

/// `browser.resize`: the page's size in CSS pixels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserResizeParams {
    pub tab_id: String,
    pub width: u32,
    pub height: u32,
}

/// `browser.attach`: start getting a tab's picture on this connection, at
/// the page's own size (one picture pixel per CSS pixel).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserAttachParams {
    pub tab_id: String,
}

/// `browser.ack`: this connection has drawn the tab's frames up to `frame_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserAckParams {
    pub tab_id: String,
    pub frame_id: u64,
}

/// Which modifier keys are held, as the page sees them.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserMods {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ctrl: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alt: bool,
    /// Command on macOS, the Windows or Super key elsewhere.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPointer {
    Down,
    Up,
    Move,
}

/// One thing you did to a tab. Positions are CSS pixels from the page's
/// top left.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserInput {
    Mouse {
        phase: BrowserPointer,
        x: f32,
        y: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<BrowserButton>,
        /// 1 for a click, 2 for a double click.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clicks: Option<u32>,
        #[serde(default)]
        mods: BrowserMods,
    },
    Wheel {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        #[serde(default)]
        mods: BrowserMods,
    },
    /// A key pressed and let go: `key` is the web's name for it ("Enter",
    /// "ArrowLeft", "a"), `text` what it types, if anything.
    Key {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default)]
        mods: BrowserMods,
    },
    /// Text put in as a paste is.
    Text { text: String },
}

/// `browser.input`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserInputParams {
    pub tab_id: String,
    pub input: BrowserInput,
}

/// `browser.selection` result: the text selected in the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserSelection {
    pub text: String,
}

/// `browser.frame` notification: the latest picture of a tab this
/// connection attached to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserFrame {
    pub tab_id: String,
    /// Counts up per attachment; acknowledge it with `browser.ack`.
    pub frame_id: u64,
    /// Base64 JPEG.
    pub data: String,
    /// The page's size in CSS pixels, which the picture shows all of.
    pub page_width: u32,
    pub page_height: u32,
}

/// `browser.changed` notification: a workspace's browser started or
/// stopped, or a tab opened, closed, loaded or changed address or title.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserChanged {
    pub browser: BrowserInfo,
}

/// Turn what someone typed in an address bar into an http(s) URL, or say
/// why not. Agents and the panel go through this, so the browser never opens
/// files, `chrome://` pages or scripts.
pub fn browser_url(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("no address".into());
    }
    if s == "about:blank" {
        return Ok(s.into());
    }
    let lower = s.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        s.to_string()
    } else {
        // "name:rest" is a scheme unless rest starts with a port ("localhost:3000/x").
        if let Some((scheme, rest)) = lower.split_once(':') {
            let port = rest.split(['/', '?', '#']).next().unwrap_or("");
            let is_port = !port.is_empty() && port.chars().all(|c| c.is_ascii_digit());
            let schemeish = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
            if schemeish && !is_port {
                return Err(format!("only web addresses (http, https) open in the browser, not {scheme}:"));
            }
        }
        let host = lower.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
        let local = host == "localhost" || host.ends_with(".localhost") || host.starts_with("127.");
        format!("{}://{s}", if local { "http" } else { "https" })
    };
    if with_scheme.contains(char::is_whitespace) {
        return Err("an address has no spaces".into());
    }
    let rest = with_scheme.split_once("://").map(|(_, r)| r).unwrap_or("");
    if rest.is_empty() || rest.starts_with('/') {
        return Err("the address has no host".into());
    }
    Ok(with_scheme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_become_web_urls() {
        assert_eq!(browser_url("https://example.com/a").unwrap(), "https://example.com/a");
        assert_eq!(browser_url("  example.com ").unwrap(), "https://example.com");
        assert_eq!(browser_url("example.com:8443/x").unwrap(), "https://example.com:8443/x");
        assert_eq!(browser_url("localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(browser_url("app.localhost:5173/login").unwrap(), "http://app.localhost:5173/login");
        assert_eq!(browser_url("HTTP://Example.com").unwrap(), "HTTP://Example.com");
        assert_eq!(browser_url("about:blank").unwrap(), "about:blank");
    }

    #[test]
    fn other_schemes_are_refused() {
        for bad in ["file:///etc/passwd", "chrome://settings", "javascript:alert(1)", "view-source:https://a.com", "data:text/html,hi", "devtools://x", "", "https://", "http:///x", "a b.com"] {
            assert!(browser_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn input_reads_as_tagged_json() {
        let i: BrowserInput = serde_json::from_str(r#"{"type":"mouse","phase":"down","x":3,"y":4,"button":"left","clicks":1}"#).unwrap();
        assert_eq!(i, BrowserInput::Mouse { phase: BrowserPointer::Down, x: 3.0, y: 4.0, button: Some(BrowserButton::Left), clicks: Some(1), mods: BrowserMods::default() });
        let k = serde_json::to_value(BrowserInput::Key { key: "Enter".into(), text: Some("\r".into()), mods: BrowserMods { shift: true, ..Default::default() } }).unwrap();
        assert_eq!(k, serde_json::json!({"type": "key", "key": "Enter", "text": "\r", "mods": {"shift": true}}));
    }
}
