//! Line icons from the designs (SF Symbols / Adwaita style), as SVG in square view boxes.

use rune::Icon;

macro_rules! icon {
    ($id:ident, $name:literal, $view:literal, $body:literal) => {
        pub static $id: Icon = Icon { name: $name, view: $view, body: $body };
    };
}

icon!(SIDEBAR, "ah-sidebar", 18.0, r#"<rect x="1" y="2.5" width="16" height="13" rx="3" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M6.5 2.5V15.5" stroke="currentColor" stroke-width="1.4"/>"#);
icon!(COMPOSE, "ah-compose", 17.0, r#"<path d="M8 2.5H4.5a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" fill="none"/><path d="M13.2 1.8l2 2L9 10l-2.6.6L7 8z" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" fill="none"/>"#);
icon!(SEARCH, "ah-search", 13.0, r#"<circle cx="5.5" cy="5.5" r="4.2" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M8.6 8.6L12 12" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#);
icon!(BOLT, "ah-bolt", 14.0, r#"<path d="M8.2 0.8L2.5 8h4l-1 5.2L11.5 6h-4z" fill="currentColor"/>"#);
icon!(SPINNER, "ah-spinner", 14.0, r#"<circle cx="7" cy="7" r="5.5" stroke="currentColor" stroke-opacity="0.3" stroke-width="1.6" fill="none"/><path d="M7 1.5a5.5 5.5 0 0 1 5.5 5.5" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" fill="none"/>"#);
icon!(STOP_CIRCLE, "ah-stop-circle", 16.0, r#"<circle cx="8" cy="8" r="6.6" stroke="currentColor" stroke-width="1.4" fill="none"/><rect x="5.6" y="5.6" width="4.8" height="4.8" rx="1" fill="currentColor"/>"#);
icon!(STOP, "ah-stop", 16.0, r#"<rect x="3" y="3" width="10" height="10" rx="2" fill="currentColor"/>"#);
icon!(MORE_H, "ah-more-h", 16.0, r#"<circle cx="3.5" cy="8" r="1.3" fill="currentColor"/><circle cx="8" cy="8" r="1.3" fill="currentColor"/><circle cx="12.5" cy="8" r="1.3" fill="currentColor"/>"#);
icon!(MORE_V, "ah-more-v", 16.0, r#"<circle cx="8" cy="3" r="1.5" fill="currentColor"/><circle cx="8" cy="8" r="1.5" fill="currentColor"/><circle cx="8" cy="13" r="1.5" fill="currentColor"/>"#);
icon!(CHEVRON_R, "ah-chev-r", 10.0, r#"<path d="M3.5 2l3 3-3 3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(CHEVRON_D, "ah-chev-d", 10.0, r#"<path d="M2 3.5l3 3 3-3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(CHEVRON_UP, "ah-chev-up", 14.0, r#"<path d="M3 9l4-4 4 4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(CHEVRON_DOWN, "ah-chev-down", 14.0, r#"<path d="M3 5l4 4 4-4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(UPDOWN, "ah-updown", 10.0, r#"<path d="M2.5 4L5 1.5 7.5 4M2.5 6L5 8.5 7.5 6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(TERMINAL, "ah-terminal", 15.0, r#"<rect x="0.7" y="1.7" width="13.6" height="11.6" rx="2.5" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M3.5 5.5l2 1.8-2 1.8M7 9.3h3.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#);
icon!(DOC, "ah-doc", 14.0, r#"<path d="M3 1h5.5L12 4.5V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1z" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M8.5 1v3.5H12" stroke="currentColor" stroke-width="1.3" fill="none"/>"#);
icon!(GLOBE, "ah-globe", 14.0, r#"<circle cx="7" cy="7" r="5.8" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M1.2 7h11.6M7 1.2c-2 2-2 9.6 0 11.6M7 1.2c2 2 2 9.6 0 11.6" stroke="currentColor" stroke-width="1.2" fill="none"/>"#);
icon!(PLUG_SM, "ah-plug-sm", 14.0, r#"<path d="M5 1.5v3M9 1.5v3M3.2 4.5h7.6v2.4a3.8 3.8 0 0 1-7.6 0zM7 10.7v2.3" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#);
icon!(CHECK, "ah-check", 14.0, r#"<path d="M3 7.3l2.7 2.7L11 4.5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(CROSS, "ah-cross", 14.0, r#"<path d="M4 4l6 6M10 4l-6 6" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" fill="none"/>"#);
icon!(WARN, "ah-warn", 16.0, r##"<path d="M8 1.3l7.3 12.7H0.7z" fill="currentColor"/><rect x="7.25" y="5.5" width="1.5" height="4.5" rx="0.75" fill="#1e1e1e"/><circle cx="8" cy="11.7" r="0.9" fill="#1e1e1e"/>"##);
icon!(CLIP, "ah-clip", 16.0, r#"<path d="M12.5 7.3l-5.6 5.6a3.4 3.4 0 0 1-4.8-4.8l6-6a2.3 2.3 0 0 1 3.2 3.2l-6 6a1.1 1.1 0 0 1-1.6-1.6l5.5-5.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" fill="none"/>"#);
icon!(MIC, "ah-mic", 16.0, r#"<rect x="5.3" y="0.8" width="5.4" height="9" rx="2.7" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M3 7.5a5 5 0 0 0 10 0M8 12.5v2.7" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" fill="none"/>"#);
icon!(SEND, "ah-send", 13.0, r#"<path d="M6.5 11.5V1.5M2.3 5.7L6.5 1.5l4.2 4.2" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(FILE_SM, "ah-file-sm", 14.0, r#"<path d="M3 .8h5L11.8 4.6V12.4a.8.8 0 0 1-.8.8H3a.8.8 0 0 1-.8-.8V1.6A.8.8 0 0 1 3 .8z" stroke="currentColor" stroke-width="1.2" fill="none"/>"#);
icon!(XMARK_CIRCLE, "ah-xc", 14.0, r##"<circle cx="7" cy="7" r="6" fill="currentColor"/><path d="M4.8 4.8l4.4 4.4M9.2 4.8l-4.4 4.4" stroke="#1e1e1e" stroke-width="1.4" stroke-linecap="round"/>"##);
icon!(CLOSE, "ah-close", 10.0, r#"<path d="M2 2l6 6M8 2L2 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#);
icon!(MINIMIZE, "ah-min", 10.0, r#"<path d="M2 5h6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#);
icon!(FOLDER, "ah-folder", 15.0, r#"<path d="M1 3.5a1.5 1.5 0 0 1 1.5-1.5h3l1.5 1.6h5.5A1.5 1.5 0 0 1 14 5.1v6.4a1.5 1.5 0 0 1-1.5 1.5h-10A1.5 1.5 0 0 1 1 11.5z" stroke="currentColor" stroke-width="1.3" fill="none"/>"#);
icon!(PLUS, "ah-plus", 16.0, r#"<path d="M8 2v12M2 8h12" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>"#);
icon!(PLUS_SMALL, "ah-plus-sm", 10.0, r#"<path d="M5 1v8M1 5h8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#);
icon!(MINUS_SMALL, "ah-minus-sm", 10.0, r#"<path d="M1 5h8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#);
icon!(MENU, "ah-menu", 16.0, r#"<path d="M2 3h12M2 8h12M2 13h12" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>"#);
icon!(PHONE, "ah-phone", 22.0, r#"<rect x="6.5" y="2.5" width="9" height="17" rx="2.2" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M9.5 16.8h3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#);
// Settings tabs (22 pt).
icon!(TAB_GENERAL, "ah-tab-general", 22.0, r#"<circle cx="11" cy="11" r="3" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M11 2v2.5M11 17.5V20M2 11h2.5M17.5 11H20M4.6 4.6l1.8 1.8M15.6 15.6l1.8 1.8M4.6 17.4l1.8-1.8M15.6 6.4l1.8-1.8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#);
icon!(TAB_AGENTS, "ah-tab-agents", 22.0, r#"<rect x="3" y="6" width="16" height="12" rx="3.5" stroke="currentColor" stroke-width="1.4" fill="none"/><circle cx="8.5" cy="12" r="1.3" fill="currentColor"/><circle cx="13.5" cy="12" r="1.3" fill="currentColor"/><path d="M11 6V3M9 3h4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#);
icon!(TAB_WORKSPACES, "ah-tab-ws", 22.0, r#"<path d="M2.5 6a2 2 0 0 1 2-2h4l2 2.2h7a2 2 0 0 1 2 2V16a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" stroke="currentColor" stroke-width="1.4" fill="none"/>"#);
icon!(TAB_CONNECTORS, "ah-tab-conn", 22.0, r#"<path d="M8 3v4M14 3v4M5.5 7h11v3.5a5.5 5.5 0 0 1-11 0zM11 16v3.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" fill="none"/>"#);
icon!(TAB_DEVICES, "ah-tab-dev", 22.0, r#"<rect x="6.5" y="2.5" width="9" height="17" rx="2.2" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M9.5 16.8h3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#);
icon!(GITHUB, "ah-github", 16.0, r#"<path d="M8 0.5a7.5 7.5 0 0 0-2.4 14.6c.4.1.5-.2.5-.4v-1.3c-2.1.5-2.5-1-2.5-1-.3-.9-.8-1.1-.8-1.1-.7-.5 0-.5 0-.5.8.1 1.2.8 1.2.8.7 1.2 1.8.8 2.2.6.1-.5.3-.8.5-1-1.7-.2-3.4-.8-3.4-3.7 0-.8.3-1.5.8-2-.1-.2-.3-1 .1-2 0 0 .6-.2 2.1.8a7 7 0 0 1 3.8 0c1.4-1 2.1-.8 2.1-.8.4 1 .2 1.8.1 2 .5.5.8 1.2.8 2 0 2.9-1.8 3.5-3.4 3.7.3.2.5.7.5 1.4v2.1c0 .2.1.5.6.4A7.5 7.5 0 0 0 8 .5z" fill="currentColor"/>"#);
icon!(TAB_ARCHIVED, "ah-tab-archived", 22.0, r#"<rect x="2.5" y="4" width="17" height="4.5" rx="1.2" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M4 8.5V17a1.5 1.5 0 0 0 1.5 1.5h11A1.5 1.5 0 0 0 18 17V8.5M9 12h4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" fill="none"/>"#);
// A git branch (SF Symbols' arrow.triangle.branch / Adwaita's vcs-branch).
icon!(BRANCH, "ah-branch", 12.0, r#"<circle cx="3.2" cy="2.6" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="3.2" cy="9.4" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="8.8" cy="3.8" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M3.2 4.1v3.8M8.8 5.3c0 2.2-2.4 2.4-4.4 3" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" fill="none"/>"#);
// The trailing inspector pane (sidebar.right).
icon!(INSPECTOR, "ah-inspector", 18.0, r#"<rect x="1" y="2.5" width="16" height="13" rx="3" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M11.5 2.5V15.5" stroke="currentColor" stroke-width="1.4"/>"#);
// A speech bubble for a side question.
icon!(BUBBLE, "ah-bubble", 14.0, r#"<path d="M2.5 2h9a1.5 1.5 0 0 1 1.5 1.5v5A1.5 1.5 0 0 1 11.5 10H6.5L3.5 12.5V10h-1A1.5 1.5 0 0 1 1 8.5v-5A1.5 1.5 0 0 1 2.5 2z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" fill="none"/>"#);
// Toasts: information and failure.
icon!(INFO, "ah-info", 16.0, r##"<circle cx="8" cy="8" r="7" fill="currentColor"/><rect x="7.2" y="7" width="1.6" height="5" rx="0.8" fill="#1e1e1e"/><circle cx="8" cy="4.7" r="1" fill="#1e1e1e"/>"##);
icon!(ERROR, "ah-error", 16.0, r##"<circle cx="8" cy="8" r="7" fill="currentColor"/><rect x="7.2" y="3.8" width="1.6" height="5.4" rx="0.8" fill="#1e1e1e"/><circle cx="8" cy="11.6" r="1" fill="#1e1e1e"/>"##);
// The model popover's arrow, pointing down at its chip: the popover's own fill with its
// border on the two slanted edges, in the bottom half of the box (macOS and GNOME colours).
icon!(POPOVER_ARROW_MAC, "ah-pop-arrow-mac", 20.0, r##"<path d="M0 10.6L10 20L20 10.6z" fill="#2f2f31"/><path d="M0.4 10.6L10 19.6L19.6 10.6" stroke="#ffffff" stroke-opacity="0.13" stroke-width="0.6" fill="none"/>"##);
icon!(POPOVER_ARROW_GNOME, "ah-pop-arrow-gnome", 16.0, r##"<path d="M0 8.6L8 16L16 8.6z" fill="#36363a"/><path d="M0.5 8.6L8 15.5L15.5 8.6" stroke="#ffffff" stroke-opacity="0.12" stroke-width="1" fill="none"/>"##);
// Send's stop while the agent works.
icon!(STOP_SM, "ah-stop-sm", 12.0, r#"<rect x="2" y="2" width="8" height="8" rx="1.6" fill="currentColor"/>"#);
// A subagent: a four-point sparkle.
icon!(SPARKLE, "ah-sparkle", 14.0, r#"<path d="M7 1.2C7.5 4.4 9.1 6.2 12.8 7 9.1 7.8 7.5 9.6 7 12.8 6.5 9.6 4.9 7.8 1.2 7 4.9 6.2 6.5 4.4 7 1.2z" fill="currentColor"/>"#);
icon!(STEPS, "ah-steps", 14.0, r#"<path d="M2.2 3.6l1.1 1.1 2-2.1M2.2 9.6l1.1 1.1 2-2.1M7.5 3.7h4.8M7.5 9.7h4.8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
// Automations: a clock (scheduled, and the sidebar entry), a webhook's three linked nodes,
// and a play triangle (by hand, Run Now).
icon!(CLOCK, "ah-clock", 14.0, r#"<circle cx="7" cy="7" r="5.8" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M7 3.8V7l2.2 1.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(HOOK, "ah-hook", 14.0, r#"<circle cx="7" cy="3.2" r="1.8" stroke="currentColor" stroke-width="1.3" fill="none"/><circle cx="3" cy="10.6" r="1.8" stroke="currentColor" stroke-width="1.3" fill="none"/><circle cx="11" cy="10.6" r="1.8" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M6.1 4.8L3.9 9M7.9 4.8l2.2 4.2M4.8 10.6h4.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#);
icon!(PLAY, "ah-play", 14.0, r#"<path d="M4 2.6v8.8a.6.6 0 0 0 .9.5l7-4.4a.6.6 0 0 0 0-1L4.9 2.1a.6.6 0 0 0-.9.5z" fill="currentColor"/>"#);

// The compact sidebar: a pull request the session opened, pin and unpin, archive, and the
// Workspaces heading's Add Folder.
icon!(PULL_REQUEST, "ah-pr", 12.0, r#"<circle cx="3" cy="2.6" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="3" cy="9.4" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="9" cy="9.4" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M3 4v4M9 8V5.2a1.6 1.6 0 0 0-1.6-1.6H5.4M6.6 2.4L5.4 3.6l1.2 1.2" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#);
icon!(PIN, "ah-pin", 14.0, r#"<path d="M5 1.6h4l-.5 3.5 2.4 2.3v1.1H3.1V7.4l2.4-2.3z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" fill="none"/><path d="M7 8.5v3.9" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#);
icon!(UNPIN, "ah-unpin", 14.0, r#"<path d="M5 1.6h4l-.5 3.5 2.4 2.3v1.1H3.1V7.4l2.4-2.3z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" fill="none"/><path d="M7 8.5v3.9M1.8 1.8l10.4 10.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#);
icon!(ARCHIVE, "ah-archive", 14.0, r#"<rect x="1.5" y="2.3" width="11" height="3.2" rx="0.9" stroke="currentColor" stroke-width="1.3" fill="none"/><path d="M2.6 5.5v5.4a1.1 1.1 0 0 0 1.1 1.1h6.6a1.1 1.1 0 0 0 1.1-1.1V5.5M5.6 8h2.8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#);
icon!(FOLDER_PLUS, "ah-folder-plus", 15.0, r#"<path d="M1 3.5a1.4 1.4 0 0 1 1.4-1.4h3l1.4 1.5h5.2a1.4 1.4 0 0 1 1.4 1.4v6a1.4 1.4 0 0 1-1.4 1.4H2.4A1.4 1.4 0 0 1 1 11z" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M7.2 5.9v4M5.2 7.9h4" stroke="currentColor" stroke-width="1.25" stroke-linecap="round"/>"#);

// The agents' own marks (assets/agents/, from LobeHub's icon set), in their element's colour:
// the badge's ink on its brand tile, or the model chip's text colour.
macro_rules! agent_logo {
    ($id:ident, $name:literal, $file:literal) => {
        pub static $id: Icon = Icon { name: $name, view: 24.0, body: include_str!(concat!("../../assets/agents/", $file)) };
    };
}

agent_logo!(LOGO_CLAUDE, "ah-logo-claude", "claude.svg");
agent_logo!(LOGO_CODEX, "ah-logo-codex", "codex.svg");
agent_logo!(LOGO_ANTIGRAVITY, "ah-logo-antigravity", "antigravity.svg");
agent_logo!(LOGO_OPENCODE, "ah-logo-opencode", "opencode.svg");
agent_logo!(LOGO_CURSOR, "ah-logo-cursor", "cursor.svg");

/// An agent's mark; None for one without (the test agent), which keeps its letter.
pub fn agent_logo(agent: agenthost_proto::AgentKind) -> Option<&'static Icon> {
    use agenthost_proto::AgentKind as A;
    match agent {
        A::Claude => Some(&LOGO_CLAUDE),
        A::Codex => Some(&LOGO_CODEX),
        A::Antigravity => Some(&LOGO_ANTIGRAVITY),
        A::Opencode => Some(&LOGO_OPENCODE),
        A::Cursor => Some(&LOGO_CURSOR),
        A::Fake => None,
    }
}

#[cfg(test)]
mod tests {
    use agenthost_proto::AgentKind;

    /// Every real agent's mark parses and draws a shape, not nothing and not a filled square.
    /// `AGENT_LOGOS_ASCII=1` prints them, to look at.
    #[test]
    fn agent_logos_draw() {
        for agent in AgentKind::REAL {
            let logo = super::agent_logo(agent).expect("every real agent has a mark");
            let px = 32;
            let cov = rune::text::icon_coverage(logo, px);
            let inked = cov.iter().filter(|&&c| c > 127).count() as f32 / cov.len() as f32;
            if std::env::var_os("AGENT_LOGOS_ASCII").is_some() {
                println!("{}:", logo.name);
                for r in cov.chunks(px as usize) {
                    println!("{}", r.iter().map(|&c| if c > 170 { '#' } else if c > 60 { '+' } else { '.' }).collect::<String>());
                }
            }
            assert!((0.12..0.85).contains(&inked), "{} covers {inked}", logo.name);
        }
    }

    /// The iOS asset catalog carries copies of the same marks.
    #[test]
    fn ios_has_the_same_logos() {
        macro_rules! same {
            ($file:literal, $set:literal) => {
                assert_eq!(
                    include_str!(concat!("../../assets/agents/", $file)),
                    include_str!(concat!("../../ios/AgentHost/Assets.xcassets/", $set, ".imageset/", $file)),
                    "copy assets/agents/{} into the {} image set",
                    $file,
                    $set
                );
            };
        }
        same!("claude.svg", "AgentClaude");
        same!("codex.svg", "AgentCodex");
        same!("antigravity.svg", "AgentAntigravity");
        same!("opencode.svg", "AgentOpenCode");
        same!("cursor.svg", "AgentCursor");
    }
}
