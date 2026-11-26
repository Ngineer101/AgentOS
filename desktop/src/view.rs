//! The main window: the sidebar (sessions by workspace, search, host line), the toolbar or
//! header bar, banners and the open session; sheets, Settings and menus over it.

use crate::look::MEDIUM;
use crate::commands::{label as keys, run, Cmd};
use crate::icons as ic;
use crate::look::{Btn, Look, Theme};
use crate::model::{App, Conn, Menu, Screen, Status, Tab};
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, Cx, El, ScrollSpec, TextStyle, Tx};

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let mut t = Theme::new(s.look);
    if let Some(w) = s.sidebar_w {
        t.sidebar_w = w;
    }
    let k = t.kit();
    let mut body = row().key("body").w_full().h_full().min_h(0.0).fill(t.window);
    if s.sidebar_open {
        body = body.child(sidebar(s, &t));
    }
    body = body.child(main_pane(s, &t, &k, cx));
    // One trailing pane: a subagent's, else the right panel on its open tab.
    match (&s.screen, &s.agent_open) {
        (Screen::Session(id), Some((_, call))) if s.agent_visible() => body = body.child(crate::agents::pane(s, id, call, &t, &k)),
        _ => {
            if let Some(tab) = s.panel_shown() {
                body = body.child(crate::panel::pane(s, tab, &t, &k));
            }
        }
    }
    let mut root = col().key("root").w_full().h_full().fill(t.window).child(body);
    if s.menu.is_some() && s.menu != Some(Menu::Login) && !matches!(s.menu, Some(Menu::Pick(_))) {
        root = root.child(menu_layer(s, &t));
    }
    if s.new_open {
        root = root.child(crate::sheet::new_session(s, &t));
    }
    if let Some(tab) = s.settings {
        root = root.child(crate::settings::view(s, tab, &t));
    }
    if s.shortcuts_open {
        root = root.child(crate::sheet::shortcuts(s, &t));
    }
    if let Some(id) = &s.confirm_archive {
        root = root.child(crate::sheet::confirm_archive(s, id, &t));
    }
    if s.auto_open {
        root = root.child(crate::automations::editor(s, &t));
    }
    if let Some(alert) = crate::automations::confirm(s, &t) {
        root = root.child(alert);
    }
    if let Some(alert) = crate::notes::confirm(s, &t) {
        root = root.child(alert);
    }
    if let Some(v) = crate::session::image_viewer(s, &t) {
        root = root.child(v);
    }
    // Over everything, sheets too: a failure in Settings shows where you are.
    if let Some(toasts) = crate::toast::layer(s, &t) {
        root = root.child(toasts);
    }
    root
}

// ---------------------------------------------------------------------------
// Small shared pieces
// ---------------------------------------------------------------------------

pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

pub fn human_size(b: u64) -> String {
    match b {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} B"),
    }
}

pub(crate) fn is_web_link(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:"].iter().any(|p| lower.starts_with(p))
}

/// "3m", "2h", "1d", "2w", "3mo", "1y" (macOS) or "3 min", "2 h", "1 d", "2 wk", "3 mo",
/// "1 yr" (GNOME).
pub fn short_age(ts: i64, look: Look) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let s = ((now - ts) / 1000).max(0);
    const DAY: i64 = 86400;
    let (n, unit_mac, unit_gnome) = match s {
        s if s < 60 => return "now".into(),
        s if s < 3600 => (s / 60, "m", " min"),
        s if s < DAY => (s / 3600, "h", " h"),
        s if s < 14 * DAY => (s / DAY, "d", " d"),
        s if s < 30 * DAY => (s / (7 * DAY), "w", " wk"),
        s if s < 365 * DAY => (s / (30 * DAY), "mo", " mo"),
        s => (s / (365 * DAY), "y", " yr"),
    };
    format!("{n}{}", if look.mac() { unit_mac } else { unit_gnome })
}

/// A time either side of `now` (both ms): "in 3h", "2h ago", "now" (macOS), or "in 3 h",
/// "2 h ago" (GNOME). Within a day, "in 4h 10m": the next run's minute matters.
pub fn when_label(ts: i64, now: i64, look: Look) -> String {
    let d = (ts - now) / 1000;
    let s = d.abs();
    let (m, h, day) = if look.mac() { ("m", "h", "d") } else { (" min", " h", " d") };
    let span = match s {
        s if s < 60 => return "now".into(),
        s if s < 3600 => format!("{}{m}", s / 60),
        s if s < 86400 && d > 0 && (s % 3600) / 60 > 0 && s < 6 * 3600 => format!("{}{h} {}{m}", s / 3600, (s % 3600) / 60),
        s if s < 86400 => format!("{}{h}", s / 3600),
        s => format!("{}{day}", s / 86400),
    };
    if d > 0 { format!("in {span}") } else { format!("{span} ago") }
}

/// Now, in ms since the epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A status's colour in this look.
pub fn status_color(t: &Theme, st: Status) -> Color {
    match st {
        Status::Working => t.text2,
        Status::NeedsYou => t.warn,
        Status::Failed => t.danger,
        Status::YourTurn => t.accent,
        Status::Ended => if t.mac() { t.text3 } else { Color::WHITE.alpha(0.3) },
    }
}

/// The header's branch: the name in full (middle-truncated when the header is narrow), a
/// button that copies it.
pub fn branch_button(t: &Theme, branch: &str, color: Color, chip: bool) -> El {
    let mac = t.mac();
    let size = if mac { 11.0 } else { 12.0 };
    let b = branch.to_string();
    let r = row()
        .key("header-branch")
        .gap(4.0)
        .center_y()
        .min_w(0.0)
        .shrink()
        .h(if mac { 18.0 } else { 22.0 })
        .px(if mac { 4.0 } else { 8.0 })
        .radius(if mac { 9.0 } else { 11.0 })
        .role("button")
        .label(format!("Branch {branch}: copy its name"))
        .help(format!("{branch}\nClick to copy the branch name"))
        .focusable(true)
        .hover(Color::WHITE.alpha(if mac { 0.08 } else { 0.12 }))
        .press(Color::WHITE.alpha(if mac { 0.14 } else { 0.18 }))
        .on_press_tx(move |s: &mut App, tx: &mut Tx| crate::commands::copy_branch(s, &b, tx))
        .child(icon(&ic::BRANCH, size).color(color))
        .child(text(branch.to_string()).font(TextStyle::new(size, 400.0).mono()).color(color).truncate_middle().shrink());
    if chip {
        r.fill(Color::WHITE.alpha(0.08))
    } else {
        r
    }
}

/// Renaming the session in the header: Return renames (blank clears the title), Escape
/// cancels. A refusal is a toast and the field stays.
fn title_field(s: &App, x: &Session, t: &Theme, font: TextStyle) -> El {
    let mac = t.mac();
    let input = rune::text_input(&s.title_edit, font)
        .single_line()
        .key("title-field")
        .label("Session title")
        .grow(1.0)
        .min_w(0.0)
        .color(t.text)
        .placeholder(s.untitled_name(x), t.text3)
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.title_edit)
        .on_submit(|s: &mut App, _: &mut Tx| s.save_session_rename())
        .on_input_key(|s: &mut App, ev: &rune::KeyEvent, _: &mut Tx| {
            if ev.key == rune::Key::Escape {
                s.cancel_session_rename();
                return true;
            }
            false
        });
    let mut f = row()
        .key("title-rename")
        .h(if mac { 20.0 } else { 28.0 })
        .w(if mac { 340.0 } else { 320.0 })
        .px(if mac { 6.0 } else { 8.0 })
        // The text stays where the title was.
        .ml(if mac { -6.0 } else { 0.0 })
        .center_y()
        .radius(t.btn_radius)
        .fill(t.field)
        .help("Return to rename, Escape to cancel. Leave it empty to use the workspace's name.")
        .box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() })
        .child(input);
    if mac {
        f = f.border(0.5, t.field_border);
    }
    f
}

/// The status vocabulary: spinner = working, orange = needs you, red = failed, accent dot =
/// your turn (green while unread: see [`unread_glyph`]), grey = ended. `on_accent` draws it
/// on a selected (accent) row.
pub fn status_glyph(t: &Theme, st: Status, on_accent: bool) -> El {
    let size = if t.mac() { 8.0 } else { 9.0 };
    let c = if on_accent { Color::WHITE } else { status_color(t, st) };
    let e = match st {
        Status::Working => icon(&ic::SPINNER, if t.mac() { 14.0 } else { 16.0 }).color(if on_accent { Color::WHITE } else if t.mac() { t.text2 } else { t.text }).looping(rune::LoopSpec::rotation(0.9)),
        Status::Ended if on_accent => t.dot(Color::WHITE.alpha(0.6), size),
        Status::YourTurn if !on_accent => t.dot(t.your_turn, size),
        _ => t.dot(c, size),
    };
    row().size(14.0, 14.0).center().child(e)
}

/// A session's glyph while its agent's finished turn is unread: a green dot.
pub fn unread_glyph(t: &Theme) -> El {
    row().size(14.0, 14.0).center().child(t.dot(t.unread, if t.mac() { 8.0 } else { 9.0 }))
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn sidebar(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let toggle = t.icon_button(&ic::SIDEBAR, if mac { 18.0 } else { 17.0 }, &format!("Hide Sidebar ({})", keys(s.look, Cmd::ToggleSidebar)), |s, tx| run(s, Cmd::ToggleSidebar, tx)).key("toggle-sidebar");
    let mut search_btn = t
        .icon_button(&ic::SEARCH, if mac { 15.0 } else { 16.0 }, &format!("Search Sessions ({})", keys(s.look, Cmd::Search)), |s, _| if s.search_open { s.search.replace_text(""); s.close_search() } else { s.open_search() })
        .key("sidebar-search")
        .checked(s.search_open);
    if s.search_open {
        search_btn = search_btn.fill(t.control);
    }
    // Both looks: the sidebar's buttons sit at its trailing edge, as macOS has them beside
    // the traffic lights. GNOME keeps its main menu (F10) there too, and the app's mark and
    // name at the leading edge, where macOS has the traffic lights.
    let mut header = row().key("sidebar-top").w_full().h(t.toolbar_h).gap(if mac { 4.0 } else { 2.0 }).center_y().window_drag();
    header = if mac {
        header.pl(20.0).pr(12.0).child(el().w(54.0)).child(el().grow(1.0)).child(toggle).child(search_btn)
    } else {
        let brand = row()
            .key("sidebar-brand")
            .gap(8.0)
            .pl(8.0)
            .center_y()
            .grow(1.0)
            .min_w(0.0)
            .child(el().size(22.0, 22.0).pixels(crate::brand::logo()))
            .child(text("AgentOS").font(TextStyle::new(t.fs, MEDIUM)).color(t.text).truncate().min_w(0.0));
        header
            .px(6.0)
            .child(brand)
            .child(toggle)
            .child(search_btn)
            .child(t.icon_button(&ic::MENU, 16.0, &format!("Main Menu ({})", keys(s.look, Cmd::MainMenu)), |s, tx| run(s, Cmd::MainMenu, tx)).key("main-menu"))
    };

    // Under the header: the search field while searching, else New Session and Automations.
    let top = if s.search_open {
        search_field(s, t)
    } else {
        let new = nav_row(t, "nav-new", &ic::COMPOSE, "New Session", false, Some(text(keys(s.look, Cmd::NewSession)).font(TextStyle::new(if mac { 11.0 } else { 12.0 }, 400.0)).color(if mac { t.text3 } else { t.text2 })), |s, tx| run(s, Cmd::NewSession, tx));
        col().key("sidebar-nav").w_full().px(if mac { 10.0 } else { 6.0 }).pb(2.0).gap(if mac { 1.0 } else { 2.0 }).child(new).child(automations_nav(s, t)).children(notes_nav(s, t))
    };

    // Pinned, then the workspaces' groups.
    let numbered = s.numbered();
    let q = s.search.text.trim().to_lowercase();
    let mut list = col().key("session-list").w_full().px(if mac { 10.0 } else { 6.0 }).pb(10.0).gap(if mac { 1.0 } else { 2.0 }).role("list").focus_group();
    let pinned = s.pinned_sessions();
    if !pinned.is_empty() {
        list = list.child(heading(t, "Pinned", None));
        for x in &pinned {
            list = list.child(session_row(s, t, x, jump(&numbered, x), &q));
        }
    }
    let groups = s.sidebar_groups();
    if !groups.is_empty() || pinned.is_empty() {
        let add = row()
            .key("add-folder")
            .size(if mac { 22.0 } else { 28.0 }, if mac { 22.0 } else { 28.0 })
            .center()
            .radius(t.btn_radius)
            .role("button")
            .label("Add Folder…")
            .help("Add Folder…")
            .focusable(true)
            .hover(Color::WHITE.alpha(0.08))
            .on_press_tx(|_: &mut App, tx: &mut Tx| crate::settings::pick_workspace_folder(tx))
            .child(icon(&ic::FOLDER_PLUS, if mac { 15.0 } else { 16.0 }).color(if mac { t.text3 } else { t.text2 }));
        list = list.child(heading(t, "Workspaces", Some(add)));
    }
    for (gi, (w, sessions)) in groups.iter().enumerate() {
        let (rows, hidden) = s.group_rows(&w.workspace_id, sessions);
        list = list.child(group_row(s, t, w, sessions, gi > 0));
        for x in &rows {
            list = list.child(session_row(s, t, x, jump(&numbered, x), &q));
        }
        if hidden > 0 || (s.show_all.contains(&w.workspace_id) && sessions.len() > crate::model::GROUP_SHOWN && !s.group_collapsed(&w.workspace_id) && q.is_empty()) {
            list = list.child(more_row(t, &w.workspace_id, hidden));
        }
    }
    if groups.is_empty() && pinned.is_empty() {
        let msg = if !s.search.text.is_empty() {
            "No sessions match.".to_string()
        } else {
            format!("No sessions yet. {} starts one.", keys(s.look, Cmd::NewSession))
        };
        list = list.child(t.small(msg).wrap().px(10.0).pt(8.0));
    }

    // The host line.
    let (dot, state) = match &s.conn {
        Conn::Connected => (t.ok, "Connected"),
        Conn::Connecting => (t.warn, "Connecting…"),
        Conn::StartingHost => (t.warn, "Starting…"),
        Conn::Down(_) => (t.danger, "Offline"),
    };
    let name = if s.hostname.is_empty() { s.info.as_ref().map(|i| i.host_id.clone()).unwrap_or_else(|| "This computer".into()) } else { s.hostname.clone() };
    let fsz = if mac { 12.0 } else { 13.0 };
    let mut host = row()
        .key("host-line")
        .w_full()
        .gap(8.0)
        .center_y()
        .role("button")
        .label(format!("Host {name}: {state}"))
        .help("Host status and checks")
        .hover(Color::WHITE.alpha(0.04))
        .on_press(|s: &mut App| s.open_settings(Tab::General))
        .child(t.dot(dot, if mac { 7.0 } else { 8.0 }));
    host = if mac {
        host.pt(12.0)
            .pb(14.0)
            .px(20.0)
            .border_sides([0.5, 0.0, 0.0, 0.0], t.sep)
            .child(text(name).font(TextStyle::new(fsz, 400.0)).color(t.text2).truncate().shrink())
            .child(el().grow(1.0))
            .child(text(state.to_string()).font(TextStyle::new(fsz, 400.0)).color(t.text3))
    } else {
        host.py(12.0).px(18.0).child(text(format!("{name} · {state}")).font(TextStyle::new(fsz, 400.0)).color(t.text2).truncate().shrink())
    };

    let mut sb = col()
        .key("sidebar")
        .w(t.sidebar_w)
        .h_full()
        .min_h(0.0)
        .child(header)
        .child(top)
        .child(col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
        .child(host);
    sb = if mac { sb.gradient(t.sidebar_top, t.sidebar_bottom).border_sides([0.0, 0.5, 0.0, 0.0], Color::BLACK.alpha(0.6)) } else { sb.fill(t.sidebar_top) };
    sb.child(sidebar_grip(t))
}

/// The sidebar's trailing edge: drag it to resize the sidebar (kept between launches),
/// double-press it for the look's own width.
fn sidebar_grip(t: &Theme) -> El {
    use crate::model::{SIDEBAR_MAX_W, SIDEBAR_MIN_W};
    const GRIP: f32 = 6.0;
    let w = t.sidebar_w;
    el().key("sidebar-grip")
        .role("separator")
        .label("Resize Sidebar")
        .range_value(f64::from(w), f64::from(SIDEBAR_MIN_W), f64::from(SIDEBAR_MAX_W))
        .range_step(16.0)
        .pin(None, Some(0.0), Some(-GRIP / 2.0), Some(0.0))
        .w(GRIP)
        .cursor(rune::Cursor::ResizeColumn)
        .fill(Color::TRANSPARENT)
        .raise()
        .on_drag(move |s: &mut App, d: &mut rune::Drag, _: &mut Tx| match d.phase {
            rune::Phase::Down if d.clicks == 2 => s.sidebar_w = None,
            rune::Phase::Down => d.data = w,
            rune::Phase::Move if d.clicks < 2 => s.resize_sidebar(d.data + d.dx, false),
            rune::Phase::Up if d.cancelled() && d.clicks < 2 => s.resize_sidebar(d.data, true),
            rune::Phase::Up if d.clicks < 2 => s.resize_sidebar(d.data + d.dx, true),
            _ => {}
        })
        .on_step(move |s: &mut App, delta: i32, _: &mut Tx| s.resize_sidebar(w + delta as f32 * 16.0, true))
}

/// A session's place among the numbered ones (⌘1…⌘9), if it has one.
fn jump(numbered: &[String], x: &Session) -> Option<usize> {
    numbered.iter().position(|id| *id == x.session_id).map(|i| i + 1)
}

/// The sidebar's search field, under the header; Escape clears it, then closes it.
fn search_field(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let field = rune::text_input(&s.search, TextStyle::new(t.fs, 400.0))
        .single_line()
        .key("search-field")
        .label("Search sessions")
        .grow(1.0)
        .min_w(0.0)
        .color(t.text)
        .placeholder(if mac { "Search" } else { "Search sessions" }, if mac { t.text3 } else { t.text2 })
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.search)
        .on_input_key(|s: &mut App, ev: &rune::KeyEvent, _: &mut Tx| {
            if ev.key == rune::Key::Escape && ev.mods == rune::Mods::default() {
                s.close_search();
                return true;
            }
            false
        });
    let mut search = row()
        .key("search-box")
        .h(if mac { 28.0 } else { 34.0 })
        .mx(if mac { 12.0 } else { 10.0 })
        .mb(if mac { 8.0 } else { 6.0 })
        .px(if mac { 8.0 } else { 10.0 })
        .gap(if mac { 6.0 } else { 8.0 })
        .center_y()
        .radius(t.btn_radius)
        .fill(Color::WHITE.alpha(if mac { 0.07 } else { 0.08 }))
        .child(icon(&ic::SEARCH, if mac { 13.0 } else { 15.0 }).color(if mac { t.text3 } else { t.text2 }))
        .child(field);
    if !s.search.text.is_empty() {
        search = search.child(
            row()
                .key("search-clear")
                .role("button")
                .label("Clear search")
                .on_press(|s: &mut App| s.search.replace_text(""))
                .child(icon(&ic::XMARK_CIRCLE, 13.0).color(t.text3)),
        );
    }
    if s.search.focused {
        search = search.box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() });
    }
    search
}

/// One of the sidebar's destinations above the sessions (New Session, Automations).
fn nav_row(t: &Theme, key: &str, glyph: &'static rune::Icon, label: &str, selected: bool, trailing: Option<El>, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    let mac = t.mac();
    let mut r = row()
        .key(key.to_string())
        .w_full()
        .h(if mac { 28.0 } else { 34.0 })
        .gap(if mac { 8.0 } else { 10.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .center_y()
        .radius(t.row_radius)
        .role("button")
        .label(label.to_string())
        .focusable(true)
        .on_press_tx(f)
        .child(row().size(16.0, 16.0).center().child(icon(glyph, if mac { 15.0 } else { 16.0 }).color(if selected || !mac { t.text } else { t.text2 })))
        .child(text(label.to_string()).font(TextStyle { weight: if selected && !mac { MEDIUM } else { 400.0 }, ..t.font() }).color(if selected && mac { Color::WHITE } else { t.text }).grow(1.0));
    if let Some(e) = trailing {
        r = r.child(e);
    }
    if selected {
        r.fill(t.row_selected)
    } else {
        r.hover(t.row_hover)
    }
}

/// A small heading over a part of the list ("Pinned", "Workspaces"), with an optional button.
fn heading(t: &Theme, title: &str, button: Option<El>) -> El {
    let mac = t.mac();
    let mut r = row()
        .w_full()
        .h(if mac { 30.0 } else { 38.0 })
        .pl(10.0)
        .pr(if mac { 2.0 } else { 0.0 })
        .pt(8.0)
        .center_y()
        .child(text(title.to_string()).font(if mac { TextStyle::new(11.0, MEDIUM) } else { TextStyle::new(12.5, MEDIUM) }).color(if mac { t.text3 } else { t.text2 }).grow(1.0));
    if let Some(b) = button {
        r = r.child(b);
    }
    r
}

/// A workspace's row: the chevron collapses it, "+" starts a session in it. Collapsed, it
/// shows how many sessions it holds and an orange dot if one needs you.
fn group_row(s: &App, t: &Theme, w: &Workspace, sessions: &[&Session], gap_above: bool) -> El {
    let mac = t.mac();
    let quick = w.kind == WorkspaceKind::Scratch;
    let title = if quick { if mac { "Quick tasks".to_string() } else { "Quick Tasks".to_string() } } else { w.name.clone() };
    let collapsed = s.group_collapsed(&w.workspace_id);
    let searching = !s.search.text.trim().is_empty();
    let ink = t.text2;
    let wsid = w.workspace_id.clone();
    let mut r = row()
        .key(format!("group-{}", w.workspace_id))
        .w_full()
        .h(if mac { 26.0 } else { 32.0 })
        .pl(if mac { 10.0 } else { 12.0 })
        .pr(if mac { 4.0 } else { 3.0 })
        .gap(if mac { 8.0 } else { 10.0 })
        .center_y()
        .radius(t.row_radius)
        .role("button")
        .label(format!("{title}, {} session{}, {}", sessions.len(), if sessions.len() == 1 { "" } else { "s" }, if collapsed { "collapsed" } else { "expanded" }))
        .focusable(true)
        .hover(t.row_hover)
        .child(row().size(if mac { 14.0 } else { 16.0 }, 14.0).center().child(icon(if collapsed { &ic::CHEVRON_R } else { &ic::CHEVRON_D }, if mac { 10.0 } else { 12.0 }).color(ink)))
        .child(text(title.clone()).font(TextStyle::new(t.fs, MEDIUM)).color(ink).truncate().grow(1.0).min_w(0.0))
        .on_secondary_press({
            let wsid = wsid.clone();
            move |s: &mut App, p: rune::PointerPress, tx: &mut Tx| section_menu(s, &wsid, p, tx)
        });
    if gap_above {
        r = r.mt(6.0);
    }
    if !searching {
        let id = wsid.clone();
        r = r.on_press(move |s: &mut App| s.toggle_group(&id));
    }
    if collapsed {
        if sessions.iter().any(|x| Status::of(x.state) == Status::NeedsYou) {
            r = r.child(t.dot(t.warn, if mac { 7.0 } else { 8.0 }));
        }
        r = r.child(text(sessions.len().to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.0 }, 400.0)).color(if mac { t.text3 } else { t.text2 }));
    }
    let id = wsid.clone();
    let size = if mac { 20.0 } else { 26.0 };
    r.child(
        row()
            .key(format!("group-new-{}", w.workspace_id))
            .size(size, size)
            .center()
            .radius(t.btn_radius)
            .role("button")
            .label(format!("New Session in {title}"))
            .help(format!("New Session in {title}"))
            .hover(Color::WHITE.alpha(0.10))
            .press(Color::WHITE.alpha(0.16))
            .on_press(move |s: &mut App| s.open_new_session_in(&id))
            .child(icon(&ic::PLUS_SMALL, if mac { 11.0 } else { 12.0 }).color(if mac { t.text2 } else { t.text })),
    )
}

/// "Show 6 more" under a group's newest sessions, or "Show less" once they all show.
fn more_row(t: &Theme, ws: &str, hidden: usize) -> El {
    let mac = t.mac();
    let label = match (hidden, mac) {
        (0, true) => "Show less".to_string(),
        (0, false) => "Show Less".to_string(),
        (n, true) => format!("Show {n} more"),
        (n, false) => format!("Show {n} More"),
    };
    let id = ws.to_string();
    row()
        .key(format!("more-{ws}"))
        .w_full()
        .h(if mac { 26.0 } else { 32.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .gap(if mac { 8.0 } else { 10.0 })
        .center_y()
        .radius(t.row_radius)
        .role("button")
        .label(label.clone())
        .focusable(true)
        .hover(t.row_hover)
        .on_press(move |s: &mut App| {
            if hidden > 0 {
                s.show_all.insert(id.clone());
            } else {
                s.show_all.remove(&id);
            }
        })
        .child(el().w(if mac { 14.0 } else { 16.0 }))
        .child(text(label).font(t.font()).color(if mac { t.text3 } else { t.accent_text }))
}

/// A workspace row's context menu: native on macOS, the app's own elsewhere.
fn section_menu(s: &mut App, ws: &str, p: rune::PointerPress, tx: &mut Tx) {
    if s.look.mac() && rune::popup_menu_supported() {
        let ws = ws.to_string();
        let mut menu = rune::PopupMenu::new().item(rune::PopupItem::new("new", "New Session Here"));
        if s.host_has_notes() {
            menu = menu.item(rune::PopupItem::new("note", "New Note Here"));
        }
        let menu = menu.separator().item(rune::PopupItem::new("archive-ended", "Archive Ended Sessions"));
        tx.popup_menu(menu, p, move |s: &mut App, picked, _| match picked {
            Some("new") => s.open_new_session_in(&ws),
            Some("note") => s.new_note_in(&ws),
            Some("archive-ended") => s.archive_ended(&ws),
            _ => {}
        });
    } else {
        s.menu = Some(Menu::Section(ws.to_string(), p.x, p.y));
    }
}

/// The sidebar's way to the Automations screen: how many are on, or a spinner while one runs.
fn automations_nav(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let selected = s.screen == Screen::Automations;
    let running = s.automations_running();
    let on = s.automations.iter().filter(|a| a.enabled).count();
    let faint = if selected && mac { Color::WHITE.alpha(0.75) } else if mac { t.text3 } else { t.text2 };
    let trailing = if running > 0 {
        Some(icon(&ic::SPINNER, 12.0).color(faint).looping(rune::LoopSpec::rotation(0.9)))
    } else if on > 0 {
        Some(text(on.to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.0 }, 400.0)).color(faint))
    } else {
        None
    };
    let label = if running > 0 { format!("Automations, {running} running") } else { "Automations".to_string() };
    nav_row(t, "nav-automations", &ic::CLOCK, "Automations", selected, trailing, |s, _| s.open_automations()).label(label)
}

/// The sidebar's way to the Notes screen, with how many are open (hosts with notes only).
fn notes_nav(s: &App, t: &Theme) -> Option<El> {
    if !s.host_has_notes() {
        return None;
    }
    let mac = t.mac();
    let selected = s.screen == Screen::Notes;
    let open = s.notes.iter().filter(|n| n.done_at.is_none()).count();
    let faint = if selected && mac { Color::WHITE.alpha(0.75) } else if mac { t.text3 } else { t.text2 };
    let trailing = (open > 0).then(|| text(open.to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.0 }, 400.0)).color(faint));
    let label = if open > 0 { format!("Notes, {open} open") } else { "Notes".to_string() };
    Some(nav_row(t, "nav-notes", &ic::NOTE, "Notes", selected, trailing, |s, _| s.open_notes()).label(label))
}

/// A session's exception line: only when it needs you, failed, or has agents running.
fn exception_line(s: &App, t: &Theme, x: &Session, status: Status) -> Option<(String, Color)> {
    match status {
        Status::NeedsYou => Some((status.word().to_string(), t.warn)),
        Status::Failed => Some((x.state_reason.as_deref().map(|r| format!("Failed · {r}")).unwrap_or_else(|| "Failed".into()), t.danger)),
        Status::Working => crate::model::running_label(s.running_work(&x.session_id).0, 0).map(|l| (l, t.text2)),
        _ => None,
    }
}

/// Byte ranges of `q` (lowercase) in `title`, bolded in the row while searching.
fn match_spans(title: &str, q: &str, weight: f32) -> Vec<rune::Span> {
    let lower = title.to_lowercase();
    if q.is_empty() || lower.len() != title.len() {
        return vec![];
    }
    lower.match_indices(q).map(|(i, m)| rune::Span::new(i, i + m.len()).weight(weight)).collect()
}

/// A session in the sidebar: status, title, then its pull request and automation icons and
/// its age. Hovered, Pin and Archive take their place; with the jump keys' modifier held
/// the age gives way to ⌘1…9. Everything else (login, branch, PR number) is in its tooltip.
fn session_row(s: &App, t: &Theme, x: &Session, n: Option<usize>, q: &str) -> El {
    let mac = t.mac();
    let selected = s.screen == Screen::Session(x.session_id.clone());
    let ended = x.state.is_terminal();
    let title = s.session_title(x);
    let status = Status::of(x.state);
    let unread = s.unread(x);
    let profile = x.profile_id.as_ref().and_then(|id| s.info.as_ref()?.agents.iter().find(|a| a.profile_id.as_ref() == Some(id))?.profile_name.clone());
    let age = short_age(x.updated_at, s.look);
    let line = exception_line(s, t, x, status);
    let run = x.automation_id.is_some();
    let pr = x.pull_request.as_ref();
    let ws = s.workspace(&x.workspace_id).map(|w| if w.kind == WorkspaceKind::Scratch { "Quick tasks".to_string() } else { w.name.clone() });

    // What the second line used to say, for the tooltip and the screen reader.
    let word = if unread { format!("{}, unread", status.word()) } else { status.word().to_string() };
    let mut facts = vec![line.as_ref().map(|l| l.0.clone()).unwrap_or(word), profile.unwrap_or_else(|| x.agent.display_name().to_string())];
    facts.extend(ws.clone());
    facts.push(age.clone());
    let mut tip = facts.join(" · ");
    let mut extra = vec![];
    if let Some(p) = pr {
        extra.push(match p.state {
            Some(PullRequestState::Open) => format!("Pull request #{}, open", p.number),
            Some(PullRequestState::Merged) => format!("Pull request #{}, merged", p.number),
            Some(PullRequestState::Closed) => format!("Pull request #{}, closed", p.number),
            None => format!("Pull request #{}", p.number),
        });
    }
    if let Some(w) = &x.worktree {
        extra.push(w.branch.clone());
    }
    if run {
        extra.push("An automation's run".to_string());
    }
    if !extra.is_empty() {
        tip = format!("{tip}\n{}", extra.join(" · "));
    }
    let spoken = format!("{title}, {}", tip.replace('\n', ", "));

    let (title_c, sub_faint) = if selected && mac {
        (Color::WHITE, t.text2)
    } else if ended {
        (t.text2, t.text3)
    } else {
        (t.text, if mac { t.text3 } else { t.text2 })
    };
    let id = x.session_id.clone();
    let hover_id = x.session_id.clone();
    let two = line.is_some();
    let mut r = row()
        .key(x.session_id.clone())
        .w_full()
        .gap(if mac { 8.0 } else { 10.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .radius(t.row_radius)
        .role("listitem")
        .label(spoken)
        .help(tip)
        .focusable(true)
        .on_press(move |s: &mut App| {
            s.close_overlays();
            s.open_session(&id)
        })
        .on_secondary_press({
            let id = x.session_id.clone();
            move |s: &mut App, p: rune::PointerPress, tx: &mut Tx| row_menu(s, &id, p, tx)
        })
        .on_hover_change(move |s: &mut App, inside: bool, _: &mut Tx| {
            if inside {
                s.hover_row = Some(hover_id.clone());
            } else if s.hover_row.as_deref() == Some(hover_id.as_str()) {
                s.hover_row = None;
            }
        });
    r = if two { r.py(if mac { 6.0 } else { 7.0 }).items_start() } else { r.h(if mac { 28.0 } else { 34.0 }).center_y() };
    let glyph = if unread { unread_glyph(t) } else { status_glyph(t, status, false) };
    r = r.child(if two { row().h(if mac { 16.0 } else { 20.0 }).center_y().child(glyph) } else { glyph });

    let weight = if selected && !mac { MEDIUM } else { 400.0 };
    let name = text(title).font(TextStyle { weight, ..t.font() }).color(title_c).truncate().min_w(0.0).spans(match_spans(&s.session_title(x), q, MEDIUM));
    r = r.child(match &line {
        Some((l, c)) => col().grow(1.0).min_w(0.0).gap(1.0).child(name).child(text(l.clone()).font(t.font_small()).color(*c).truncate()),
        None => name.grow(1.0),
    });

    // The trailing column.
    let small = TextStyle::new(if mac { 11.0 } else { 12.0 }, 400.0);
    let mut end = row().gap(if mac { 6.0 } else { 8.0 }).center_y();
    if two {
        end = end.mt(if mac { 1.0 } else { 2.0 });
    }
    let hovered = s.hover_row.as_deref() == Some(x.session_id.as_str()) && x.archived_at.is_none();
    if hovered {
        let size = if mac { 20.0 } else { 26.0 };
        let button = |key: String, glyph: &'static rune::Icon, label: &str, f: Box<dyn Fn(&mut App)>| {
            row()
                .key(key)
                .size(size, size)
                .center()
                .radius(t.btn_radius)
                .role("button")
                .label(label.to_string())
                .help(label.to_string())
                .hover(Color::WHITE.alpha(0.10))
                .press(Color::WHITE.alpha(0.16))
                .on_press(move |s: &mut App| f(s))
                .child(icon(glyph, if mac { 13.0 } else { 14.0 }).color(if mac { t.text2 } else { t.text }))
        };
        if s.has_pins() {
            let pinned = x.pinned_at.is_some();
            let id = x.session_id.clone();
            end = end.child(button(format!("pin-{}", x.session_id), if pinned { &ic::UNPIN } else { &ic::PIN }, if pinned { "Unpin" } else { "Pin" }, Box::new(move |s| s.set_pinned(&id, !pinned))));
        }
        let id = x.session_id.clone();
        end = end.child(button(format!("archive-{}", x.session_id), &ic::ARCHIVE, if ended { "Archive" } else { "Archive…" }, Box::new(move |s| s.archive(&id))));
        end = end.mr(if mac { -6.0 } else { -8.0 });
    } else {
        if let Some(p) = pr {
            end = end.child(icon(&ic::PULL_REQUEST, if mac { 12.0 } else { 14.0 }).color(pr_color(t, p.state)));
        }
        if run {
            end = end.child(icon(&ic::CLOCK, if mac { 12.0 } else { 14.0 }).color(sub_faint));
        }
        match n.filter(|_| s.jump_held) {
            Some(n) => end = end.child(text(keys(s.look, Cmd::Jump(n as u8))).font(TextStyle::new(if mac { 11.0 } else { 12.0 }, MEDIUM)).color(t.text)),
            None => end = end.child(text(age).font(small).color(sub_faint)),
        }
    }
    r = r.child(end);
    if selected {
        r.fill(t.row_selected)
    } else {
        r.hover(t.row_hover)
    }
}

/// A pull request's mark: green while open, purple once merged, red when closed without
/// merging; grey until the host has looked (older hosts never do).
pub fn pr_color(t: &Theme, state: Option<PullRequestState>) -> Color {
    match state {
        Some(PullRequestState::Open) => t.ok,
        Some(PullRequestState::Merged) => t.pr,
        Some(PullRequestState::Closed) => t.danger,
        None => t.text2,
    }
}

// ---------------------------------------------------------------------------
// Main pane: toolbar, banners, content
// ---------------------------------------------------------------------------

fn main_pane(s: &App, t: &Theme, k: &rune_kit::Kit, cx: &mut Cx<App>) -> El {
    let _ = cx;
    let content = match &s.screen {
        Screen::Session(id) if s.session(id).is_some() => crate::session::screen(s, id, t, k),
        Screen::Automations => crate::automations::screen(s, t),
        Screen::Notes => crate::notes::screen(s, t),
        _ => home(s, t),
    };
    let mut c = col().key("main").grow(1.0).min_w(0.0).h_full().min_h(0.0).fill(t.content).child(toolbar(s, t));
    if !s.sidebar_open || !t.mac() {
        c = c.border_sides([0.0, 0.0, 0.0, if t.mac() { 0.0 } else { 1.0 }], Color::BLACK.alpha(0.35));
    }
    c.child(status_strips(s, t)).child(content)
}

fn toolbar(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let sess = s.current();
    let (title, subtitle) = match sess {
        // The session's own title (else its workspace's name), over who runs it, where, and
        // on which branch: the same live or ended.
        Some(x) => (s.session_title(x), s.session_context(x)),
        None if s.screen == Screen::Automations => ("Automations".to_string(), crate::automations::summary(s)),
        None if s.screen == Screen::Notes => ("Notes".to_string(), crate::notes::summary(s)),
        None => {
            let live = s.sessions.iter().filter(|x| !x.state.is_terminal()).count();
            ("AgentOS".to_string(), if live == 0 { "No live sessions".to_string() } else { format!("{live} live session{}", if live == 1 { "" } else { "s" }) })
        }
    };
    let branch = sess.and_then(|x| x.worktree.as_ref()).map(|w| w.branch.clone());
    // macOS runs the branch on with a dot; GNOME's capsule stands apart.
    let subtitle = if branch.is_some() && mac { format!("{subtitle} ·") } else { subtitle };
    let sub_text = text(subtitle).font(TextStyle::new(t.fs_small, 400.0).lh((t.fs_small * 1.2).round())).color(t.text2).truncate();
    // GNOME has no capsule: the status leads the header's subtitle.
    // "Your turn" is the resting state: the header says nothing then.
    let sub_line = match sess.filter(|x| !mac && x.archived_at.is_none() && Status::of(x.state) != Status::YourTurn) {
        Some(x) => {
            let st = Status::of(x.state);
            let word = text(format!("{} · ", st.word())).font(TextStyle::new(t.fs_small, MEDIUM).lh((t.fs_small * 1.2).round())).color(if st == Status::Working { t.text2 } else { status_color(t, st) });
            row().key("header-status").gap(6.0).center_y().min_w(0.0).child(status_glyph(t, st, false).size(12.0, 12.0)).child(word).child(sub_text.shrink())
        }
        None => row().min_w(0.0).child(sub_text.shrink()),
    };
    let sub_line = match &branch {
        Some(b) => sub_line.gap(if mac { 1.0 } else { 8.0 }).center_y().child(branch_button(t, b, t.text2, !mac)),
        None => sub_line,
    };
    let sub_line = if mac { sub_line } else { sub_line.max_w(10_000.0).w_full().justify_center() };
    let title_font = TextStyle::new(t.fs, MEDIUM).lh((t.fs * 1.2).round());
    let renaming = sess.filter(|x| s.session_rename.as_deref() == Some(x.session_id.as_str()));
    let title_el = match renaming {
        Some(x) => title_field(s, x, t, title_font),
        None => {
            let mut e = text(title).key("session-title").font(title_font).color(t.text).truncate();
            if sess.is_some_and(|x| x.archived_at.is_none()) {
                e = e.help("Double-click to rename").on_double_press(|s: &mut App| s.start_session_rename());
            }
            e
        }
    };
    let titles = col().key("title").min_w(0.0).shrink().gap(if renaming.is_some() && !mac { 2.0 } else { 0.0 }).child(title_el).child(sub_line);
    // Stopping a turn is the composer's stop button and its keys, not the toolbar's.
    let live = sess.is_some_and(|x| !x.state.is_terminal());
    let more = t
        .icon_button(if mac { &ic::MORE_H } else { &ic::MORE_V }, 16.0, "Session Menu", |s: &mut App, tx: &mut Tx| session_menu(s, tx))
        .key("session-menu")
        .opacity(if live || sess.is_some() { 1.0 } else { 0.35 });
    // The side-question pane's toggle, once the session has side questions.
    let side = sess.filter(|x| s.has_side(&x.session_id)).map(|_| {
        let shown = s.side_visible();
        let label = format!("{} Side Questions ({})", if shown { "Hide" } else { "Show" }, keys(s.look, Cmd::ToggleSide));
        let b = t.icon_button(&ic::INSPECTOR, if mac { 18.0 } else { 17.0 }, &label, |s, tx| run(s, Cmd::ToggleSide, tx)).key("toggle-side").role("button").checked(shown);
        if shown { b.fill(Color::WHITE.alpha(if mac { 0.10 } else { 0.12 })) } else { b }
    });

    // The terminal's toggle, on every screen, where this connection may have one.
    let terminal = s.terminal_allowed().then(|| crate::terminal::toggle_button(s, t));
    // The browser's, likewise.
    let browser = s.browser_allowed().then(|| crate::browser::toggle_button(s, t));
    // The open session's changes (and, a tab over, its files).
    let changes = (sess.is_some() && s.changes_available()).then(|| crate::files::toggle_button(s, t));

    if mac {
        let mut bar = row()
            .key("toolbar")
            .w_full()
            .h(t.toolbar_h)
            .pl(20.0)
            .pr(14.0)
            .gap(10.0)
            .center_y()
            .fill(t.toolbar)
            .border_sides([0.0, 0.0, 0.5, 0.0], Color::BLACK.alpha(0.6))
            .window_drag();
        if !s.sidebar_open {
            bar = bar
                .pl(92.0)
                .child(t.icon_button(&ic::SIDEBAR, 18.0, &format!("Show Sidebar ({})", keys(s.look, Cmd::ToggleSidebar)), |s, tx| run(s, Cmd::ToggleSidebar, tx)))
                .child(t.icon_button(&ic::COMPOSE, 17.0, &format!("New Session ({})", keys(s.look, Cmd::NewSession)), |s, tx| run(s, Cmd::NewSession, tx)));
        }
        bar = bar.child(titles).child(el().grow(1.0));
        if let Some(c) = capsule(s, t) {
            bar = bar.child(c);
        }
        let mut group = row().gap(2.0).ml(6.0);
        if sess.is_some() {
            group = group.child(more.w(32.0));
        }
        if let Some(b) = side {
            group = group.child(b.w(32.0));
        }
        if let Some(b) = changes {
            group = group.child(b.w(32.0));
        }
        if let Some(b) = browser {
            group = group.child(b.w(32.0));
        }
        if let Some(b) = terminal {
            group = group.child(b.w(32.0));
        }
        bar.child(group)
    } else {
        let mut bar = row()
            .key("toolbar")
            .w_full()
            .h(t.toolbar_h)
            .px(6.0)
            .gap(4.0)
            .center_y()
            .fill(t.toolbar)
            .box_shadow(rune::Shadow { y: 1.0, color: Color::BLACK.alpha(0.3), ..Default::default() })
            .window_drag();
        // While the sidebar shows, its own header has the toggle (as on macOS).
        bar = if s.sidebar_open {
            bar.child(el().w(34.0))
        } else {
            bar.child(t.icon_button(&ic::SIDEBAR, 17.0, &format!("Show Sidebar ({})", keys(s.look, Cmd::ToggleSidebar)), |s, tx| run(s, Cmd::ToggleSidebar, tx)).key("show-sidebar"))
        };
        bar = bar.child(col().grow(1.0).min_w(0.0).items_center().child(titles.w_full().items_center()));
        if sess.is_some() {
            bar = bar.child(more);
            if let Some(b) = side {
                bar = bar.child(b);
            }
            if let Some(b) = changes {
                bar = bar.child(b);
            }
        } else {
            // Keeps the title centred as the session's two buttons would.
            let others = usize::from(terminal.is_some()) + usize::from(browser.is_some());
            bar = bar.child(el().w((72.0 - 36.0 * others as f32).max(0.0)));
        }
        if let Some(b) = browser {
            bar = bar.child(b);
        }
        if let Some(b) = terminal {
            bar = bar.child(b);
        }
        // Linux keeps the window manager's own frame; elsewhere (a preview) draw Adwaita's.
        if !cfg!(target_os = "linux") {
            let circle = |i: &'static rune::Icon, label: &str, f: fn(&mut App, &mut Tx)| {
                row().size(24.0, 24.0).radius(12.0).center().fill(Color::WHITE.alpha(0.1)).role("button").label(label.to_string()).hover(Color::WHITE.alpha(0.06)).on_press_tx(f).child(icon(i, 10.0).color(t.text))
            };
            bar = bar.child(
                row()
                    .gap(6.0)
                    .px(6.0)
                    .child(circle(&ic::MINIMIZE, "Minimize", |_, tx| tx.minimize()))
                    .child(circle(&ic::CLOSE, "Close", |_, tx| tx.close_window())),
            );
        }
        bar
    }
}

/// The macOS toolbar's status capsule: this session needs you, or another one does.
/// The macOS toolbar's status capsule, and one for approvals waiting in other sessions.
fn capsule(s: &App, t: &Theme) -> Option<El> {
    let pill = |key: &str, label: String, fg: Color, bg: Color, glyph: El| {
        row().key(key.to_string()).h(22.0).px(9.0).gap(6.0).center_y().radius(11.0).fill(bg).child(glyph).child(text(label).font(TextStyle::new(11.0, MEDIUM)).color(fg))
    };
    let mut r = row().gap(6.0).center_y();
    let mut any = false;
    let sess = s.current();
    if let Some(x) = sess.filter(|x| x.archived_at.is_none() && Status::of(x.state) != Status::YourTurn) {
        let st = Status::of(x.state);
        let c = status_color(t, st);
        let glyph = if st == Status::Working { icon(&ic::SPINNER, 10.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)) } else { t.dot(c, 6.0) };
        let (fg, bg) = match st {
            Status::Working | Status::Ended => (t.text2, Color::WHITE.alpha(0.08)),
            _ => (c, c.alpha(0.16)),
        };
        let mut p = pill("status-capsule", st.word().to_string(), fg, bg, glyph);
        if st == Status::NeedsYou {
            p = p.role("button").label("Needs you: show the request").on_press_tx(|_: &mut App, tx: &mut Tx| tx.scroll_into_view("approval-card"));
        }
        r = r.child(p);
        any = true;
    }
    let elsewhere = s.approvals.iter().filter(|a| sess.is_none_or(|x| x.session_id != a.session_id)).count();
    if elsewhere > 0 {
        r = r.child(
            pill("waiting-capsule", format!("{elsewhere} waiting · {}", keys(s.look, Cmd::NextApproval)), t.warn, t.warn.alpha(0.16), t.dot(t.warn, 6.0))
                .role("button")
                .label("Go to the next waiting approval")
                .on_press_tx(|s: &mut App, tx: &mut Tx| run(s, Cmd::NextApproval, tx)),
        );
        any = true;
    }
    any.then_some(r)
}

/// A sidebar row's context menu: native on macOS, the app's own elsewhere.
fn row_menu(s: &mut App, id: &str, p: rune::PointerPress, tx: &mut Tx) {
    let Some(x) = s.session(id).cloned() else { return };
    if s.look.mac() && rune::popup_menu_supported() {
        let mut archive = rune::PopupItem::new("archive", if x.state.is_terminal() { "Archive Session" } else { "Archive Session…" });
        if let Ok(k) = "ctrl+cmd+a".parse::<rune::Shortcut>() {
            archive = archive.key_equivalent(k);
        }
        let mut menu = rune::PopupMenu::new().item(rune::PopupItem::new("open", "Open"));
        if s.has_pins() {
            menu = menu.item(rune::PopupItem::new("pin", if x.pinned_at.is_some() { "Unpin" } else { "Pin" }));
        }
        let menu = menu
            .separator()
            .item(archive)
            .item(rune::PopupItem::new("archive-ended", "Archive Ended Sessions in This Workspace"));
        let (id, ws) = (id.to_string(), x.workspace_id.clone());
        let pinned = x.pinned_at.is_some();
        tx.popup_menu(menu, p, move |s: &mut App, picked, _| match picked {
            Some("open") => s.open_session(&id),
            Some("pin") => s.set_pinned(&id, !pinned),
            Some("archive") => s.archive(&id),
            Some("archive-ended") => s.archive_ended(&ws),
            _ => {}
        });
    } else {
        s.menu = Some(Menu::Row(id.to_string(), p.x, p.y));
    }
}

/// The toolbar's "…" menu: native on macOS, the app's own elsewhere.
fn session_menu(s: &mut App, tx: &mut Tx) {
    if s.current().is_none() {
        return;
    }
    if s.look.mac() && rune::popup_menu_supported() {
        let live = s.current().is_some_and(|x| !x.state.is_terminal());
        let key = |k: &str| k.parse::<rune::Shortcut>().ok();
        let mut folder = rune::PopupItem::new("folder", "Show Session Folder in Finder");
        if let Some(k) = key("alt+cmd+r") {
            folder = folder.key_equivalent(k);
        }
        let archived = s.current().is_some_and(|x| x.archived_at.is_some());
        let mut archive = rune::PopupItem::new("archive", if live { "Archive Session…" } else { "Archive Session" }).enabled(!archived);
        if let Some(k) = key("ctrl+cmd+a") {
            archive = archive.key_equivalent(k);
        }
        let branch = s.current().is_some_and(|x| x.worktree.is_some());
        let mut menu = rune::PopupMenu::new().item(rune::PopupItem::new("rename", "Rename Session…").enabled(!archived));
        if branch {
            menu = menu.item(rune::PopupItem::new("copy-branch", "Copy Branch Name"));
        }
        let menu = menu.separator().item(folder).separator().item(archive);
        tx.popup_menu(menu, "session-menu", |s: &mut App, picked, tx| match picked {
            Some("rename") => run(s, Cmd::RenameSession, tx),
            Some("copy-branch") => run(s, Cmd::CopyBranch, tx),
            Some("folder") => run(s, Cmd::ShowFolder, tx),
            Some("archive") => run(s, Cmd::Archive, tx),
            _ => {}
        });
    } else {
        s.menu = if s.menu == Some(Menu::Session) { None } else { Some(Menu::Session) };
    }
}

/// States that last, not messages: the host connection, and an archived session being read.
/// (Messages, errors and approvals waiting elsewhere are toasts: see `toast.rs`.)
fn status_strips(s: &App, t: &Theme) -> El {
    let mut c = col().key("banners").w_full();
    let strip = |msg: El, bg: Color| row().w_full().py(8.0).px(16.0).gap(16.0).center_y().justify_center().fill(bg).child(msg);
    let msg = |m: String| text(m).font(TextStyle::new(if t.mac() { 12.0 } else { 14.0 }, 400.0)).color(t.text).wrap().shrink();
    match &s.conn {
        Conn::Connecting => c = c.child(strip(msg("Connecting to the host…".into()), Color::WHITE.alpha(0.05))),
        Conn::StartingHost => c = c.child(strip(msg("No host was running, so the app started one…".into()), Color::WHITE.alpha(0.05))),
        Conn::Down(e) => c = c.child(strip(msg(format!("Not connected to the host: {e}. Retrying.")), t.danger.alpha(0.16)).key("offline-banner")),
        Conn::Connected => {}
    }
    // An archived session opened from Settings: read-only.
    if let Some(x) = s.current().filter(|x| x.archived_at.is_some()) {
        let id = x.session_id.clone();
        let when = x.archived_at.map(|ts| format!(" {} ago", short_age(ts, s.look))).filter(|w| w != " now ago").unwrap_or_default();
        c = c.child(
            strip(msg(format!("Archived{when} · Read only")), Color::WHITE.alpha(0.06))
                .key("archived-banner")
                .child(t.small_button("Unarchive", move |s| s.unarchive(&id)).key("unarchive")),
        );
    }
    c
}

/// No session open: what to do next (or why nothing works).
fn home(s: &App, t: &Theme) -> El {
    let mut c = col().key("home").w_full().grow(1.0).min_h(0.0).center().gap(10.0).px(24.0);
    c = c.child(el().w(64.0).h(64.0).pixels(crate::brand::logo()));
    if let Conn::Down(e) = &s.conn {
        let mut d = c
            .child(text("Not connected to the host").font(TextStyle::new(15.0, MEDIUM)).color(t.text))
            .child(t.small(format!("{e}")).wrap().align(rune::Align::Center).max_w(460.0));
        if s.link_remote {
            // A paired connection that dropped: the host is asleep, or the
            // relay is unreachable; only time (or its owner) fixes that.
            d = d.child(
                t.small("The host may be asleep or the relay unreachable. The app keeps retrying.")
                    .wrap()
                    .align(rune::Align::Center)
                    .max_w(460.0),
            );
            d = d.child(
                row().gap(8.0).pt(6.0).child(
                    t.button("Pair with Another Host…", Btn::Default, None, |s: &mut App| {
                        s.open_settings(Tab::General);
                        s.start_remote_pair();
                    })
                    .key("pair-other"),
                ),
            );
        } else {
            d = d.child(
                t.small("The app keeps retrying. Is agenthostd running?").wrap().align(rune::Align::Center).max_w(460.0),
            );
            let mut buttons = row().gap(8.0).pt(6.0);
            if let Some(name) = s.remote_host.clone() {
                let label = format!("Connect to {name}");
                buttons = buttons.child(t.button(&label, Btn::Primary, None, move |s: &mut App| s.connect_remote()).key("connect-remote"));
            }
            buttons = buttons.child(
                t.button("Pair with a Remote Host…", Btn::Default, None, |s: &mut App| {
                    s.open_settings(Tab::General);
                    s.start_remote_pair();
                })
                .key("pair-remote"),
            );
            d = d.child(buttons);
        }
        return d;
    }
    c.child(text("No Session Selected").font(TextStyle::new(15.0, MEDIUM)).color(t.text))
        .child(t.small(format!("Start a session with {}, or a quick task with {}.", keys(s.look, Cmd::NewSession), keys(s.look, Cmd::QuickTask))).wrap().align(rune::Align::Center))
        .child(
            row()
                .gap(8.0)
                .pt(6.0)
                .child(t.button_tx("Quick Task", Btn::Default, Some(&keys(s.look, Cmd::QuickTask)), |s, tx| run(s, Cmd::QuickTask, tx)).key("home-quick"))
                .child(t.button_tx("New Session", Btn::Primary, Some(&keys(s.look, Cmd::NewSession)), |s, tx| run(s, Cmd::NewSession, tx)).key("home-new")),
        )
}

// ---------------------------------------------------------------------------
// In-app menus
// ---------------------------------------------------------------------------

pub struct Item {
    pub title: String,
    pub keys: String,
    pub enabled: bool,
    pub danger: bool,
    pub checked: bool,
    pub action: Box<dyn Fn(&mut App, &mut Tx)>,
}

impl Item {
    pub fn new(title: &str, keys: String, action: impl Fn(&mut App, &mut Tx) + 'static) -> Item {
        Item { title: title.into(), keys, enabled: true, danger: false, checked: false, action: Box::new(action) }
    }
    pub fn enabled(mut self, on: bool) -> Item {
        self.enabled = on;
        self
    }
    pub fn checked(mut self, on: bool) -> Item {
        self.checked = on;
        self
    }
}

/// A pop-up menu's box: items (None = separator).
pub fn menu_box(t: &Theme, items: Vec<Option<Item>>, width: f32) -> El {
    let mac = t.mac();
    let mut b = col()
        .key("menu")
        .w(width)
        .padding(if mac { 5.0 } else { 6.0 })
        .gap(if mac { 0.0 } else { 2.0 })
        .radius(t.box_radius)
        .fill(if mac { rune::color::hex("#2c2c2e").unwrap_or(Color::BLACK) } else { rune::color::hex("#36363a").unwrap_or(Color::BLACK) })
        .border(if mac { 0.5 } else { 1.0 }, Color::WHITE.alpha(0.12))
        .shadow(10.0, 30.0, Color::BLACK.alpha(0.5))
        .role("menu")
        .focus_group();
    for it in items {
        let Some(it) = it else {
            b = b.child(el().w_full().h(if mac { 0.5 } else { 1.0 }).my(if mac { 5.0 } else { 4.0 }).mx(if mac { 9.0 } else { 0.0 }).fill(Color::WHITE.alpha(0.12)));
            continue;
        };
        let ink = if !it.enabled {
            t.text3
        } else if it.danger {
            t.danger
        } else {
            t.text
        };
        let action = it.action;
        let mut r = row()
            .w_full()
            .h(if mac { 24.0 } else { 36.0 })
            .px(if mac { 10.0 } else { 12.0 })
            .gap(10.0)
            .center_y()
            .radius(t.row_radius)
            .role("menuitem")
            .label(it.title.clone())
            .child(text(it.title).font(TextStyle::new(if mac { 13.0 } else { 14.5 }, 400.0)).color(ink).grow(1.0));
        if it.checked {
            r = r.child(icon(&ic::CHECK, 14.0).color(t.text));
        }
        if !it.keys.is_empty() {
            r = r.child(text(it.keys).font(TextStyle::new(if mac { 13.0 } else { 12.5 }, 400.0)).color(t.text3));
        }
        if it.enabled {
            r = r.focusable(true).hover(if mac { t.accent } else { Color::WHITE.alpha(0.08) }).on_press_tx(move |s: &mut App, tx: &mut Tx| {
                s.menu = None;
                action(s, tx)
            });
        }
        b = b.child(r);
    }
    b
}

/// The open in-app menu over everything, with a click-catcher behind it.
fn menu_layer(s: &App, t: &Theme) -> El {
    let look = s.look;
    let catcher = el().key("menu-catcher").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).on_press(|s: &mut App| s.menu = None);
    let mut layer = col().key("menu-layer").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).child(catcher);
    match &s.menu {
        Some(Menu::Session) => {
            let live = s.current().is_some_and(|x| !x.state.is_terminal());
            let archived = s.current().is_some_and(|x| x.archived_at.is_some());
            let mut items = vec![Some(Item::new("Rename Session…", String::new(), |s, tx| run(s, Cmd::RenameSession, tx)).enabled(!archived))];
            if s.current().is_some_and(|x| x.worktree.is_some()) {
                items.push(Some(Item::new("Copy Branch Name", String::new(), |s, tx| run(s, Cmd::CopyBranch, tx))));
            }
            items.extend([
                None,
                Some(Item::new("Show Session Folder", keys(look, Cmd::ShowFolder), |s, tx| run(s, Cmd::ShowFolder, tx))),
                Some(Item::new("Attach Files…", keys(look, Cmd::Attach), |s, tx| run(s, Cmd::Attach, tx)).enabled(live)),
                None,
                Some(Item::new(if live { "Archive Session…" } else { "Archive Session" }, keys(look, Cmd::Archive), |s, tx| run(s, Cmd::Archive, tx)).enabled(!archived)),
            ]);
            let right = if t.mac() { 14.0 } else { 80.0 };
            layer = layer.child(menu_box(t, items, 260.0).pin(None, Some(t.toolbar_h - 2.0), Some(right), None));
        }
        Some(Menu::Main) => {
            let items = vec![
                Some(Item::new("New Session", keys(look, Cmd::NewSession), |s, tx| run(s, Cmd::NewSession, tx))),
                Some(Item::new("New Quick Task", keys(look, Cmd::QuickTask), |s, tx| run(s, Cmd::QuickTask, tx))),
                Some(Item::new("Automations", String::new(), |s, _| s.open_automations())),
                Some(Item::new("Notes", String::new(), |s, _| s.open_notes()).enabled(s.host_has_notes())),
                Some(Item::new("Terminal", keys(look, Cmd::ToggleTerminal), |s, tx| run(s, Cmd::ToggleTerminal, tx)).enabled(s.terminal_allowed())),
                Some(Item::new("Browser", keys(look, Cmd::ToggleBrowser), |s, tx| run(s, Cmd::ToggleBrowser, tx)).enabled(s.browser_allowed())),
                Some(Item::new("Changes", keys(look, Cmd::ToggleChanges), |s, tx| run(s, Cmd::ToggleChanges, tx)).enabled(s.changes_available())),
                Some(Item::new("Files", keys(look, Cmd::ToggleFiles), |s, tx| run(s, Cmd::ToggleFiles, tx)).enabled(s.files_available())),
                None,
                Some(Item::new("Preferences", keys(look, Cmd::Settings), |s, tx| run(s, Cmd::Settings, tx))),
                Some(Item::new("Pair a Phone…", String::new(), |s, tx| run(s, Cmd::PairPhone, tx))),
                Some(Item::new("Pair with a Remote Host…", String::new(), |s, tx| run(s, Cmd::PairRemoteHost, tx))),
                Some(Item::new("Keyboard Shortcuts", keys(look, Cmd::Shortcuts), |s, tx| run(s, Cmd::Shortcuts, tx))),
                None,
                Some(Item::new("Quit", keys(look, Cmd::Quit), |s, tx| run(s, Cmd::Quit, tx))),
            ];
            let left = if s.sidebar_open { t.sidebar_w - 250.0 } else { 6.0 };
            layer = layer.child(menu_box(t, items, 244.0).pin(Some(left), Some(t.toolbar_h - 4.0), None, None));
        }
        Some(Menu::Row(id, x, y)) => {
            if let Some(sess) = s.session(id) {
                let (a, c, ws, p) = (id.clone(), id.clone(), sess.workspace_id.clone(), id.clone());
                let live = !sess.state.is_terminal();
                let pinned = sess.pinned_at.is_some();
                let mut items = vec![Some(Item::new("Open", String::new(), move |s, _| s.open_session(&a)))];
                if s.has_pins() {
                    items.push(Some(Item::new(if pinned { "Unpin" } else { "Pin" }, String::new(), move |s, _| s.set_pinned(&p, !pinned))));
                }
                items.extend([
                    None,
                    Some(Item::new(if live { "Archive Session…" } else { "Archive Session" }, keys(look, Cmd::Archive), move |s, _| s.archive(&c))),
                    Some(Item::new("Archive Ended Sessions Here", String::new(), move |s, _| s.archive_ended(&ws))),
                ]);
                layer = layer.child(menu_box(t, items, 280.0).pin(Some(*x), Some(*y), None, None));
            }
        }
        Some(Menu::Model) => {
            if let Some(sess) = s.current() {
                layer = layer.children(crate::session::model_popover(s, sess, t));
            }
        }
        Some(Menu::Section(ws, x, y)) => {
            let (ws, here, note) = (ws.clone(), ws.clone(), ws.clone());
            let mut items = vec![Some(Item::new("New Session Here", String::new(), move |s, _| s.open_new_session_in(&here)))];
            if s.host_has_notes() {
                items.push(Some(Item::new("New Note Here", String::new(), move |s, _| s.new_note_in(&note))));
            }
            items.extend([None, Some(Item::new("Archive Ended Sessions", String::new(), move |s, _| s.archive_ended(&ws)))]);
            layer = layer.child(menu_box(t, items, 240.0).pin(Some(*x), Some(*y), None, None));
        }
        _ => {}
    }
    layer
}
