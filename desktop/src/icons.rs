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
