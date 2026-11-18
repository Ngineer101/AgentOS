//! The main window: the sidebar (sessions by workspace, search, host line), the toolbar or
//! header bar, banners and the open session; sheets, Settings and menus over it.

use crate::commands::{label as keys, run, Cmd};
use crate::icons as ic;
use crate::look::{Btn, Look, Theme};
use crate::model::{App, Conn, Menu, Screen, Status, Tab};
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, Cx, El, ScrollSpec, TextStyle, Tx};

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let t = Theme::new(s.look);
    let k = t.kit();
    let mut body = row().key("body").w_full().h_full().min_h(0.0).fill(t.window);
    if s.sidebar_open {
        body = body.child(sidebar(s, &t));
    }
    body = body.child(main_pane(s, &t, &k, cx));
    let mut root = col().key("root").w_full().h_full().fill(t.window).child(body);
    if s.menu.is_some() && s.menu != Some(Menu::Login) {
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

/// "3m", "2h", "1d" (macOS) or "3 min", "2 h", "1 d" (GNOME).
pub fn short_age(ts: i64, look: Look) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let s = ((now - ts) / 1000).max(0);
    let (n, unit_mac, unit_gnome) = match s {
        s if s < 60 => return "now".into(),
        s if s < 3600 => (s / 60, "m", " min"),
        s if s < 86400 => (s / 3600, "h", " h"),
        s => (s / 86400, "d", " d"),
    };
    format!("{n}{}", if look.mac() { unit_mac } else { unit_gnome })
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

/// The status vocabulary: spinner = working, orange = needs you, red = failed, accent dot =
/// your turn, grey = ended. `on_accent` draws it on a selected (accent) row.
pub fn status_glyph(t: &Theme, st: Status, on_accent: bool) -> El {
    let size = if t.mac() { 8.0 } else { 9.0 };
    let c = if on_accent { Color::WHITE } else { status_color(t, st) };
    let e = match st {
        Status::Working => icon(&ic::SPINNER, if t.mac() { 14.0 } else { 16.0 }).color(if on_accent { Color::WHITE } else if t.mac() { t.text2 } else { t.text }).looping(rune::LoopSpec::rotation(0.9)),
        Status::Ended if on_accent => t.dot(Color::WHITE.alpha(0.6), size),
        _ => t.dot(c, size),
    };
    row().size(14.0, 14.0).center().child(e)
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn sidebar(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let header = if mac {
        row()
            .key("sidebar-top")
            .w_full()
            .h(t.toolbar_h)
            .pl(20.0)
            .pr(12.0)
            .gap(4.0)
            .center_y()
            .window_drag()
            .child(el().w(54.0))
            .child(el().grow(1.0))
            .child(t.icon_button(&ic::SIDEBAR, 18.0, &format!("Hide Sidebar ({})", keys(s.look, Cmd::ToggleSidebar)), |s, tx| run(s, Cmd::ToggleSidebar, tx)).key("toggle-sidebar"))
            .child(t.icon_button(&ic::COMPOSE, 17.0, &format!("New Session ({})", keys(s.look, Cmd::NewSession)), |s, tx| run(s, Cmd::NewSession, tx)).key("nav-new"))
    } else {
        row()
            .key("sidebar-top")
            .w_full()
            .h(t.toolbar_h)
            .px(6.0)
            .gap(4.0)
            .center_y()
            .window_drag()
            .child(t.icon_button(&ic::PLUS, 16.0, &format!("New Session ({})", keys(s.look, Cmd::NewSession)), |s, tx| run(s, Cmd::NewSession, tx)).key("nav-new"))
            .child(row().grow(1.0).justify_center().child(text("AgentHost").font(TextStyle::new(t.fs, 700.0)).color(t.text)))
            .child(t.icon_button(&ic::MENU, 16.0, &format!("Main Menu ({})", keys(s.look, Cmd::MainMenu)), |s, tx| run(s, Cmd::MainMenu, tx)).key("main-menu"))
    };

    // Search.
    let field = rune::text_input(&s.search, TextStyle::new(t.fs, 400.0))
        .single_line()
        .key("search-field")
        .label("Search sessions")
        .grow(1.0)
        .min_w(0.0)
        .color(t.text)
        .placeholder(if mac { "Search" } else { "Search sessions" }, if mac { t.text3 } else { t.text2 })
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.search);
    let mut search = row()
        .key("sidebar-search")
        .h(if mac { 28.0 } else { 34.0 })
        .mx(if mac { 12.0 } else { 10.0 })
        .mt(if mac { 2.0 } else { 4.0 })
        .mb(if mac { 10.0 } else { 8.0 })
        .px(if mac { 8.0 } else { 10.0 })
        .gap(if mac { 6.0 } else { 8.0 })
        .center_y()
        .radius(7.0)
        .fill(Color::WHITE.alpha(if mac { 0.07 } else { 0.08 }))
        .child(icon(&ic::SEARCH, if mac { 13.0 } else { 15.0 }).color(if mac { t.text3 } else { t.text2 }))
        .child(field);
    if s.search.text.is_empty() {
        search = search.child(text(keys(s.look, Cmd::Search)).font(TextStyle::new(if mac { 11.0 } else { 12.0 }, 400.0)).color(if mac { t.text3 } else { t.text2 }));
    } else {
        search = search.child(
            row()
                .role("button")
                .label("Clear search")
                .on_press(|s: &mut App| s.search.replace_text(""))
                .child(icon(&ic::XMARK_CIRCLE, 13.0).color(t.text3)),
        );
    }
    if s.search.focused {
        search = search.box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() });
    }

    // Sessions, grouped.
    let numbered = s.numbered();
    let mut list = col().key("session-list").w_full().px(if mac { 10.0 } else { 6.0 }).pb(10.0).gap(if mac { 1.0 } else { 2.0 }).role("list").focus_group();
    let groups = s.sidebar_groups();
    for (gi, (w, sessions)) in groups.iter().enumerate() {
        let quick = w.kind == WorkspaceKind::Scratch;
        let title = if quick { if mac { "Quick tasks".to_string() } else { "Quick Tasks".to_string() } } else { w.name.clone() };
        let top = if gi == 0 { if mac { 6.0 } else { 8.0 } } else { 12.0 };
        let wsid = w.workspace_id.clone();
        list = list.child(
            text(title)
                .font(if mac { TextStyle::new(11.0, 600.0) } else { TextStyle::new(12.5, 700.0) })
                .color(if mac { t.text3 } else { t.text2 })
                .pt(top)
                .pb(4.0)
                .px(if mac { 10.0 } else { 12.0 })
                .on_secondary_press(move |s: &mut App, p: rune::PointerPress, tx: &mut Tx| {
                    if s.look.mac() && rune::popup_menu_supported() {
                        let ws = wsid.clone();
                        let menu = rune::PopupMenu::new().item(rune::PopupItem::new("archive-ended", "Archive Ended Sessions"));
                        tx.popup_menu(menu, p, move |s: &mut App, picked, _| {
                            if picked == Some("archive-ended") {
                                s.archive_ended(&ws);
                            }
                        });
                    } else {
                        s.menu = Some(Menu::Section(wsid.clone(), p.x, p.y));
                    }
                }),
        );
        for x in sessions {
            let n = numbered.iter().position(|id| *id == x.session_id).map(|i| i + 1);
            list = list.child(session_row(s, t, x, n));
        }
    }
    if groups.is_empty() {
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
        .child(search)
        .child(col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
        .child(host);
    sb = if mac { sb.gradient(t.sidebar_top, t.sidebar_bottom).border_sides([0.0, 0.5, 0.0, 0.0], Color::BLACK.alpha(0.6)) } else { sb.fill(t.sidebar_top) };
    sb
}

fn session_row(s: &App, t: &Theme, x: &Session, n: Option<usize>) -> El {
    let mac = t.mac();
    let selected = s.screen == Screen::Session(x.session_id.clone());
    let ended = x.state.is_terminal();
    let title = x.title.clone().unwrap_or_else(|| s.login_label(x));
    // The status, then the login: a profile's name alone says which ("Your turn · Work · 3m").
    let status = Status::of(x.state);
    let profile = x.profile_id.as_ref().and_then(|id| s.info.as_ref()?.agents.iter().find(|a| a.profile_id.as_ref() == Some(id))?.profile_name.clone());
    let sub = vec![status.word().to_string(), profile.unwrap_or_else(|| x.agent.display_name().to_string()), short_age(x.updated_at, s.look)];
    let sub = sub.join(" · ");
    let (title_c, sub_c) = if selected && mac {
        (Color::WHITE, Color::WHITE.alpha(0.8))
    } else if ended {
        (t.text2, t.text3)
    } else {
        (t.text, t.text2)
    };
    let id = x.session_id.clone();
    let mut r = row()
        .key(x.session_id.clone())
        .w_full()
        .gap(if mac { 9.0 } else { 10.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .py(if mac { 6.0 } else { 8.0 })
        .radius(t.row_radius)
        .role("listitem")
        .label(format!("{title}, {sub}"))
        .focusable(true)
        .on_press(move |s: &mut App| {
            s.close_overlays();
            s.open_session(&id)
        })
        .on_secondary_press({
            let id = x.session_id.clone();
            move |s: &mut App, p: rune::PointerPress, tx: &mut Tx| row_menu(s, &id, p, tx)
        })
        .child(status_glyph(t, status, selected && mac).mt(if mac { 2.0 } else { 0.0 }));
    r = if mac { r.items_start() } else { r.center_y() };
    let weight = if selected && !mac { 700.0 } else { 400.0 };
    r = r.child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(1.0)
            .child(text(title).font(TextStyle { weight, ..t.font() }).color(title_c).truncate())
            .child(text(sub).font(t.font_small()).color(sub_c).truncate()),
    );
    if let (Some(n), true) = (n, mac) {
        r = r.child(text(format!("⌘{n}")).font(TextStyle::new(11.0, 400.0)).color(if selected { Color::WHITE.alpha(0.75) } else { t.text3 }).mt(1.0));
    }
    if selected {
        r.fill(t.row_selected)
    } else {
        r.hover(t.row_hover)
    }
}

// ---------------------------------------------------------------------------
// Main pane: toolbar, banners, content
// ---------------------------------------------------------------------------

fn main_pane(s: &App, t: &Theme, k: &rune_kit::Kit, cx: &mut Cx<App>) -> El {
    let _ = cx;
    let content = match &s.screen {
        Screen::Session(id) if s.session(id).is_some() => crate::session::screen(s, id, t, k),
        _ => home(s, t),
    };
    let mut c = col().key("main").grow(1.0).min_w(0.0).h_full().min_h(0.0).fill(t.content).child(toolbar(s, t));
    if !s.sidebar_open || !t.mac() {
        c = c.border_sides([0.0, 0.0, 0.0, if t.mac() { 0.0 } else { 1.0 }], Color::BLACK.alpha(0.35));
    }
    c.child(banners(s, t)).child(content)
}

fn toolbar(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let sess = s.current();
    let (title, subtitle) = match sess {
        Some(x) => {
            let ws = s.workspace(&x.workspace_id).map(|w| w.name.clone()).unwrap_or_default();
            (x.title.clone().unwrap_or_else(|| s.login_label(x)), format!("{} — {ws}", s.login_label(x)))
        }
        None => {
            let live = s.sessions.iter().filter(|x| !x.state.is_terminal()).count();
            ("AgentHost".to_string(), if live == 0 { "No live sessions".to_string() } else { format!("{live} live session{}", if live == 1 { "" } else { "s" }) })
        }
    };
    let sub_text = text(subtitle).font(TextStyle::new(t.fs_small, 400.0).lh((t.fs_small * 1.2).round())).color(t.text2).truncate();
    // GNOME has no capsule: the status leads the header's subtitle.
    let sub_line = match sess.filter(|x| !mac && x.archived_at.is_none()) {
        Some(x) => {
            let st = Status::of(x.state);
            let word = text(format!("{} · ", st.word())).font(TextStyle::new(t.fs_small, 700.0).lh((t.fs_small * 1.2).round())).color(if st == Status::Working { t.text2 } else { status_color(t, st) });
            row().key("header-status").gap(6.0).center_y().min_w(0.0).child(status_glyph(t, st, false).size(12.0, 12.0)).child(word).child(sub_text.shrink())
        }
        None => row().min_w(0.0).child(sub_text),
    };
    let titles = col()
        .key("title")
        .min_w(0.0)
        .shrink()
        .child(text(title).font(TextStyle::new(t.fs, if mac { 600.0 } else { 700.0 }).lh((t.fs * 1.2).round())).color(t.text).truncate())
        .child(sub_line);
    let running = sess.is_some_and(|x| matches!(x.state, SessionState::Running | SessionState::WaitingApproval));
    let live = sess.is_some_and(|x| !x.state.is_terminal());
    let stop = t
        .icon_button(if mac { &ic::STOP_CIRCLE } else { &ic::STOP }, 16.0, &format!("Stop the Turn ({})", keys(s.look, Cmd::Stop)), |s, tx| run(s, Cmd::Stop, tx))
        .key("stop-turn")
        .opacity(if running { 1.0 } else { 0.35 })
        .disabled(!running);
    let more = t
        .icon_button(if mac { &ic::MORE_H } else { &ic::MORE_V }, 16.0, "Session Menu", |s: &mut App, tx: &mut Tx| session_menu(s, tx))
        .key("session-menu")
        .opacity(if live || sess.is_some() { 1.0 } else { 0.35 });

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
            group = group.child(stop.w(32.0)).child(more.w(32.0));
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
            .window_drag()
            .child(t.icon_button(&ic::SIDEBAR, 17.0, &format!("Toggle Sidebar ({})", keys(s.look, Cmd::ToggleSidebar)), |s, tx| run(s, Cmd::ToggleSidebar, tx)))
            .child(col().grow(1.0).min_w(0.0).items_center().child(titles.items_center()));
        if sess.is_some() {
            bar = bar.child(stop).child(more);
        } else {
            bar = bar.child(el().w(72.0));
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
        row().key(key.to_string()).h(22.0).px(9.0).gap(6.0).center_y().radius(11.0).fill(bg).child(glyph).child(text(label).font(TextStyle::new(11.0, 600.0)).color(fg))
    };
    let mut r = row().gap(6.0).center_y();
    let mut any = false;
    let sess = s.current();
    if let Some(x) = sess.filter(|x| x.archived_at.is_none()) {
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
        let menu = rune::PopupMenu::new()
            .item(rune::PopupItem::new("open", "Open"))
            .item(rune::PopupItem::new("end", "End Session").enabled(!x.state.is_terminal()))
            .separator()
            .item(archive)
            .item(rune::PopupItem::new("archive-ended", "Archive Ended Sessions in This Workspace"));
        let (id, ws) = (id.to_string(), x.workspace_id.clone());
        tx.popup_menu(menu, p, move |s: &mut App, picked, _| match picked {
            Some("open") => s.open_session(&id),
            Some("end") => s.act("End session", agenthost_proto::methods::SESSION_END, serde_json::json!({"session_id": id}), crate::bridge::Refresh::None),
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
        let running = s.current().is_some_and(|x| matches!(x.state, SessionState::Running | SessionState::WaitingApproval));
        let key = |k: &str| k.parse::<rune::Shortcut>().ok();
        let mut stop = rune::PopupItem::new("stop", "Stop Turn").enabled(running);
        if let Some(k) = key("cmd+period") {
            stop = stop.key_equivalent(k);
        }
        let mut folder = rune::PopupItem::new("folder", "Show Session Folder in Finder");
        if let Some(k) = key("alt+cmd+r") {
            folder = folder.key_equivalent(k);
        }
        let mut end = rune::PopupItem::new("end", "End Session").enabled(live);
        if let Some(k) = key("cmd+shift+backspace") {
            end = end.key_equivalent(k);
        }
        let archived = s.current().is_some_and(|x| x.archived_at.is_some());
        let mut archive = rune::PopupItem::new("archive", if live { "Archive Session…" } else { "Archive Session" }).enabled(!archived);
        if let Some(k) = key("ctrl+cmd+a") {
            archive = archive.key_equivalent(k);
        }
        let menu = rune::PopupMenu::new().item(stop).item(folder).separator().item(end).item(archive);
        tx.popup_menu(menu, "session-menu", |s: &mut App, picked, tx| match picked {
            Some("stop") => run(s, Cmd::Stop, tx),
            Some("folder") => run(s, Cmd::ShowFolder, tx),
            Some("end") => run(s, Cmd::EndSession, tx),
            Some("archive") => run(s, Cmd::Archive, tx),
            _ => {}
        });
    } else {
        s.menu = if s.menu == Some(Menu::Session) { None } else { Some(Menu::Session) };
    }
}

/// Connection trouble, approvals waiting (GNOME's banner), errors and notices.
fn banners(s: &App, t: &Theme) -> El {
    let mut c = col().key("banners").w_full();
    let strip = |msg: El, bg: Color| row().w_full().py(8.0).px(16.0).gap(16.0).center_y().justify_center().fill(bg).child(msg);
    let msg = |m: String| text(m).font(TextStyle::new(if t.mac() { 12.0 } else { 14.0 }, 400.0)).color(t.text).wrap().shrink();
    match &s.conn {
        Conn::Connecting => c = c.child(strip(msg("Connecting to the host…".into()), Color::WHITE.alpha(0.05))),
        Conn::StartingHost => c = c.child(strip(msg("No host was running, so the app started one…".into()), Color::WHITE.alpha(0.05))),
        Conn::Down(e) => c = c.child(strip(msg(format!("Not connected to the host: {e}. Retrying.")), t.danger.alpha(0.16)).key("offline-banner")),
        Conn::Connected => {}
    }
    // GNOME: an AdwBanner for the first waiting approval, anywhere.
    if !t.mac() {
        if let Some(a) = s.approvals.first() {
            let sid = a.session_id.clone();
            let agent = s.session(&a.session_id).map(|x| x.agent.display_name().to_string()).unwrap_or_else(|| "An agent".into());
            let here = s.current().is_some_and(|x| x.session_id == a.session_id);
            let more = if s.approvals.len() > 1 { format!(" (+{} more)", s.approvals.len() - 1) } else { String::new() };
            let words = row()
                .gap(0.0)
                .child(text(agent).font(TextStyle::new(14.0, 700.0)).color(t.text))
                .child(text(format!(" wants to {} on {}{more}", crate::session::humanize(&a.tool), a.connector_name)).font(TextStyle::new(14.0, 400.0)).color(t.text));
            c = c.child(
                strip(words, t.banner).key("approval-banner").child(
                    row()
                        .h(30.0)
                        .px(14.0)
                        .center_y()
                        .radius(7.0)
                        .fill(Color::WHITE.alpha(0.12))
                        .role("button")
                        .label("Review")
                        .hover(Color::WHITE.alpha(0.06))
                        .on_press_tx(move |s: &mut App, tx: &mut Tx| {
                            if !here {
                                s.open_session(&sid);
                            }
                            tx.scroll_into_view("approval-card");
                        })
                        .child(text("Review").font(TextStyle::new(14.0, 700.0)).color(t.text)),
                ),
            );
        }
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
    let close = |label: &str, f: fn(&mut App)| t.small_button(label, f);
    if let Some(e) = &s.error {
        c = c.child(strip(msg(e.clone()), t.danger.alpha(0.18)).key("error-banner").child(close("Dismiss", |s| s.error = None)));
    }
    if let Some(n) = &s.notice {
        c = c.child(strip(msg(n.clone()), t.accent.alpha(0.16)).key("notice-banner").child(close("OK", |s| s.notice = None)));
    }
    c
}

/// No session open: what to do next (or why nothing works).
fn home(s: &App, t: &Theme) -> El {
    let mut c = col().key("home").w_full().grow(1.0).min_h(0.0).center().gap(10.0).px(24.0);
    c = c.child(el().w(64.0).h(64.0).pixels(crate::brand::logo()));
    if let Conn::Down(e) = &s.conn {
        return c
            .child(text("Not connected to the host").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
            .child(t.small(format!("{e}")).wrap().align(rune::Align::Center).max_w(460.0))
            .child(t.small("The app keeps retrying. Is agenthostd running?").wrap().align(rune::Align::Center).max_w(460.0));
    }
    c.child(text("No Session Selected").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
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
    pub fn danger(mut self) -> Item {
        self.danger = true;
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
        .radius(if mac { 8.0 } else { 12.0 })
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
            .radius(if mac { 4.0 } else { 7.0 })
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
            let running = s.current().is_some_and(|x| matches!(x.state, SessionState::Running | SessionState::WaitingApproval));
            let items = vec![
                Some(Item::new("Stop the Turn", keys(look, Cmd::Stop), |s, tx| run(s, Cmd::Stop, tx)).enabled(running)),
                Some(Item::new("Show Session Folder", keys(look, Cmd::ShowFolder), |s, tx| run(s, Cmd::ShowFolder, tx))),
                Some(Item::new("Attach Files…", keys(look, Cmd::Attach), |s, tx| run(s, Cmd::Attach, tx)).enabled(live)),
                None,
                Some(Item::new("End Session", keys(look, Cmd::EndSession), |s, tx| run(s, Cmd::EndSession, tx)).enabled(live).danger()),
                Some(Item::new(if live { "Archive Session…" } else { "Archive Session" }, keys(look, Cmd::Archive), |s, tx| run(s, Cmd::Archive, tx)).enabled(s.current().is_some_and(|x| x.archived_at.is_none()))),
            ];
            let right = if t.mac() { 14.0 } else { 80.0 };
            layer = layer.child(menu_box(t, items, 260.0).pin(None, Some(t.toolbar_h - 2.0), Some(right), None));
        }
        Some(Menu::Main) => {
            let items = vec![
                Some(Item::new("New Session", keys(look, Cmd::NewSession), |s, tx| run(s, Cmd::NewSession, tx))),
                Some(Item::new("New Quick Task", keys(look, Cmd::QuickTask), |s, tx| run(s, Cmd::QuickTask, tx))),
                None,
                Some(Item::new("Preferences", keys(look, Cmd::Settings), |s, tx| run(s, Cmd::Settings, tx))),
                Some(Item::new("Pair a Phone…", String::new(), |s, tx| run(s, Cmd::PairPhone, tx))),
                Some(Item::new("Keyboard Shortcuts", keys(look, Cmd::Shortcuts), |s, tx| run(s, Cmd::Shortcuts, tx))),
                None,
                Some(Item::new("Quit", keys(look, Cmd::Quit), |s, tx| run(s, Cmd::Quit, tx))),
            ];
            let left = if s.sidebar_open { t.sidebar_w - 250.0 } else { 6.0 };
            layer = layer.child(menu_box(t, items, 244.0).pin(Some(left), Some(t.toolbar_h - 4.0), None, None));
        }
        Some(Menu::Row(id, x, y)) => {
            if let Some(sess) = s.session(id) {
                let (a, b, c, ws) = (id.clone(), id.clone(), id.clone(), sess.workspace_id.clone());
                let live = !sess.state.is_terminal();
                let items = vec![
                    Some(Item::new("Open", String::new(), move |s, _| s.open_session(&a))),
                    Some(Item::new("End Session", String::new(), move |s, _| s.act("End session", agenthost_proto::methods::SESSION_END, serde_json::json!({"session_id": b}), crate::bridge::Refresh::None)).enabled(live)),
                    None,
                    Some(Item::new(if live { "Archive Session…" } else { "Archive Session" }, keys(look, Cmd::Archive), move |s, _| s.archive(&c))),
                    Some(Item::new("Archive Ended Sessions Here", String::new(), move |s, _| s.archive_ended(&ws))),
                ];
                layer = layer.child(menu_box(t, items, 280.0).pin(Some(*x), Some(*y), None, None));
            }
        }
        Some(Menu::Section(ws, x, y)) => {
            let ws = ws.clone();
            let items = vec![Some(Item::new("Archive Ended Sessions", String::new(), move |s, _| s.archive_ended(&ws)))];
            layer = layer.child(menu_box(t, items, 240.0).pin(Some(*x), Some(*y), None, None));
        }
        _ => {}
    }
    layer
}
