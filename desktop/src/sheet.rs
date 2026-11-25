//! New session: a sheet under the toolbar (macOS) or an AdwDialog (GNOME). And GNOME's
//! keyboard shortcuts window.

use crate::commands::{label as keys, Cmd};
use crate::icons as ic;
use crate::look::{agent_badge, Btn, Theme};
use crate::model::{login_key, App, Menu};
use crate::view::{menu_box, Item};
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, ScrollSpec, TextStyle, Tx};

/// A dimmed layer over the window that holds a modal; Escape or a click outside closes it.
pub fn scrim(key: &str, dim: f32, close: fn(&mut App)) -> El {
    col().key(key.to_string()).pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).fill(Color::BLACK.alpha(dim)).child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).on_press(close))
}

pub fn home_path(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => {
            // Sample data's /home/<user>.
            let parts: Vec<&str> = p.splitn(4, '/').collect();
            if parts.len() == 4 && parts[1] == "home" { format!("~/{}", parts[3]) } else { p.to_string() }
        }
    }
}
pub use home_path as tilde;

/// What a workspace's row says under its name.
fn ws_detail(s: &App, w: &Workspace) -> String {
    if w.kind == WorkspaceKind::Scratch {
        let web = if w.network.presets.contains(&NetworkPreset::Web) { " · open web on" } else { "" };
        return format!("No project folder{web}");
    }
    let mut net: Vec<String> = w
        .network
        .presets
        .iter()
        .map(|p| match p {
            NetworkPreset::Packages => "packages".to_string(),
            NetworkPreset::Github => "GitHub".to_string(),
            NetworkPreset::Web => "open web".to_string(),
        })
        .collect();
    net.extend(w.network.custom_hosts.iter().cloned());
    let net = if net.is_empty() { "agent only".to_string() } else { net.join(", ") };
    let _ = s;
    format!("{} · {net}", tilde(&w.path))
}

fn live_count(s: &App, ws: &str) -> usize {
    s.sessions.iter().filter(|x| x.workspace_id == ws && !x.state.is_terminal()).count()
}

fn pick_folder(tx: &mut Tx) {
    let options = rune::FileDialogOptions { title: "Add a folder agents may work in".into(), ..Default::default() };
    tx.open_folder(options, |s: &mut App, result, _| match result {
        Ok(Some(p)) => s.add_workspace_path(&p.display().to_string(), true),
        Ok(None) => {}
        Err(e) => s.error = Some(format!("Could not open the folder picker: {e}")),
    });
}

pub fn new_session(s: &App, t: &Theme) -> El {
    if t.mac() {
        mac_sheet(s, t)
    } else {
        gnome_dialog(s, t)
    }
}

fn workspace_rows(s: &App, t: &Theme) -> Vec<El> {
    let mac = t.mac();
    let mut ws: Vec<&Workspace> = s.workspaces.iter().collect();
    ws.sort_by_key(|w| w.kind != WorkspaceKind::Scratch);
    let mut rows = vec![];
    for w in ws {
        let quick = w.kind == WorkspaceKind::Scratch;
        let on = s.new_ws.as_deref() == Some(w.workspace_id.as_str());
        let id = w.workspace_id.clone();
        let live = live_count(s, &w.workspace_id);
        let name = if quick { "Quick task".to_string() } else { w.name.clone() };
        let detail = ws_detail(s, w);
        let detail = if !mac && !quick && live > 0 { format!("{} · {live} live session{}", tilde(&w.path), if live == 1 { "" } else { "s" }) } else if !mac && !quick { tilde(&w.path) } else { detail };
        let glyph = if quick { icon(&ic::BOLT, 14.0).color(t.bolt) } else { icon(&ic::FOLDER, 15.0).color(if mac { rune::color::hex("#5fb0ff").unwrap_or(t.accent) } else { t.text }) };
        let mut r = row()
            .key(format!("pick-{}", w.workspace_id))
            .w_full()
            .gap(if mac { 10.0 } else { 12.0 })
            .px(if mac { 10.0 } else { 14.0 })
            .py(if mac { 8.0 } else { 12.0 })
            .center_y()
            .role("radio")
            .checked(on)
            .label(name.clone())
            .focusable(true)
            .hover(Color::WHITE.alpha(0.03))
            .on_press(move |s: &mut App| s.pick_workspace(&id))
            .child(t.radio(on, true))
            .child(row().w(16.0).center().child(glyph))
            .child(col().grow(1.0).min_w(0.0).child(t.label(name)).child(t.small(detail).truncate()));
        if mac && quick {
            r = r.child(t.faint(keys(s.look, Cmd::QuickTask)));
        } else if mac && live > 0 {
            // Informational: each session works on a branch of its own, so several can run.
            r = r.child(text(format!("{live} live session{}", if live == 1 { "" } else { "s" })).font(TextStyle::new(11.0, 400.0)).color(t.text3));
        }
        rows.push(r);
    }
    // Adding a folder picks one on this computer; a paired host would never
    // see it (its folders are its own).
    if s.admin {
        let add = row()
            .key("add-folder")
            .w_full()
            .gap(8.0)
            .px(if mac { 10.0 } else { 14.0 })
            .py(if mac { 7.0 } else { 12.0 })
            .center_y()
            .role("button")
            .label("Add Folder…")
            .focusable(true)
            .hover(Color::WHITE.alpha(0.03))
            .on_press_tx(|_: &mut App, tx: &mut Tx| pick_folder(tx))
            .child(icon(&ic::PLUS_SMALL, 11.0).color(t.accent_text))
            .child(text("Add Folder…").font(TextStyle::new(if mac { 12.0 } else { t.fs }, 400.0)).color(t.accent_text));
        rows.push(add);
    }
    rows
}

fn agent_rows(s: &App, t: &Theme) -> Vec<El> {
    let agents = s.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
    let mut rows = vec![];
    for a in agents {
        let key = login_key(&a);
        let on = s.new_agent.as_deref() == Some(key.as_str());
        let mut r = row()
            .key(format!("login-{key}"))
            .w_full()
            .gap(10.0)
            .px(10.0)
            .py(8.0)
            .center_y()
            .role("radio")
            .checked(on)
            .label(a.label())
            .child(t.radio(on, a.available))
            .child(agent_badge(t, a.agent, 20.0, a.available));
        let mut names = col().grow(1.0).min_w(0.0).child(text(a.label()).font(t.font()).color(if a.available { t.text } else { t.text3 }));
        if !a.available {
            names = names.child(t.faint(a.problem.clone().map(|p| capitalize(&p)).unwrap_or_else(|| "Unavailable".into())));
        } else if let Some(d) = &a.config_dir {
            names = names.child(t.small(tilde(d)));
        }
        r = r.child(names);
        let is_default = s.new_ws.as_ref().and_then(|w| s.default_login(w)).as_deref() == Some(key.as_str());
        if is_default {
            r = r.child(row().key("default-tag").h(16.0).px(6.0).center_y().radius(8.0).fill(Color::WHITE.alpha(0.1)).child(text("Default").font(TextStyle::new(10.5, 600.0)).color(t.text2)));
        }
        if a.available {
            let k2 = key.clone();
            r = r.focusable(true).hover(Color::WHITE.alpha(0.03)).on_press(move |s: &mut App| s.new_agent = Some(k2.clone())).child(text("Ready").font(TextStyle::new(11.0, 400.0)).color(t.ok));
        } else {
            let fix = a.fix.clone().unwrap_or_default();
            r = r.child(
                text("How to Log In…")
                    .font(TextStyle::new(12.0, 400.0))
                    .color(t.accent_text)
                    .role("button")
                    .label(format!("How to log in to {}", a.label()))
                    .on_press_tx(move |s: &mut App, tx: &mut Tx| {
                        tx.copy(fix.clone());
                        s.notice = Some(format!("Run this in a terminal, then pick the login again: {fix} (copied)"));
                    }),
            );
        }
        rows.push(r);
    }
    if rows.is_empty() {
        rows.push(row().px(10.0).py(10.0).child(t.small("Waiting for the host…")));
    }
    rows
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// "Make <login> the default for <workspace>", when the pick isn't the workspace's default.
fn make_default_box(s: &App, t: &Theme) -> Option<El> {
    let (ws, key) = (s.new_ws.as_ref()?, s.new_agent.as_ref()?);
    if s.default_login(ws).as_deref() == Some(key.as_str()) {
        return None;
    }
    let login = s.info.as_ref()?.agents.iter().find(|a| &login_key(a) == key)?.label();
    let w = s.workspace(ws)?;
    let name = if w.kind == WorkspaceKind::Scratch { "Quick tasks".to_string() } else { w.name.clone() };
    let msg = format!("Make {login} the default for {name}");
    Some(
        row()
            .key("make-default")
            .gap(8.0)
            .center_y()
            .child(t.checkbox(s.make_default, &msg, |s| s.make_default = !s.make_default))
            .child(text(msg.clone()).font(TextStyle::new(if t.mac() { 12.0 } else { 14.0 }, 400.0)).color(t.text).role("button").label(msg).on_press(|s: &mut App| s.make_default = !s.make_default)),
    )
}

/// Started from a note (and still in its workspace): which one, since its text goes in the
/// new session's message box.
fn from_note(s: &App) -> Option<String> {
    let (_, _, text) = s.start_note.as_ref().filter(|(_, ws, _)| s.new_ws.as_ref() == Some(ws))?;
    Some(format!("From the note \u{201c}{}\u{201d}: it opens in the message box, ready to send.", crate::view::clip(agenthost_proto::title_of(text), 60)))
}

fn mac_sheet(s: &App, t: &Theme) -> El {
    let ready = s.new_ws.is_some() && s.new_agent.is_some();
    let form = col()
        .w_full()
        .gap(14.0)
        .px(20.0)
        .pt(10.0)
        .pb(4.0)
        .child(col().w_full().gap(6.0).child(t.group_title("Workspace")).child(t.group(workspace_rows(s, t), 58.0)))
        .child(col().w_full().gap(6.0).child(t.group_title("Agent")).child(t.group(agent_rows(s, t), 64.0)));
    let form = match make_default_box(s, t) {
        Some(b) => form.child(b),
        None => form,
    };
    let start = t.button("Start Session", if ready { Btn::Primary } else { Btn::Default }, Some("⌘↩"), |s| s.start_from_sheet()).key("start-session").px(14.0).default_button();
    let sheet = col()
        .key("new-session")
        .w(560.0)
        .max_w(560.0)
        .shrink()
        .min_h(0.0)
        .corners([0.0, 0.0, 12.0, 12.0])
        .fill(t.sheet)
        .border_sides([0.0, 0.5, 0.5, 0.5], Color::WHITE.alpha(0.14))
        .shadow(24.0, 60.0, Color::BLACK.alpha(0.55))
        .modal(true, |s: &mut App| s.new_open = false)
        .child(
            col()
                .w_full()
                .gap(3.0)
                .px(20.0)
                .pt(18.0)
                .pb(6.0)
                .child(text("New Session").font(TextStyle::new(15.0, 600.0)).color(t.text))
                .child(text(from_note(s).unwrap_or_else(|| "Pick where the agent works and which login it uses.".into())).font(TextStyle::new(12.0, 400.0)).color(t.text2).truncate()),
        )
        .child(col().w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(form))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .px(20.0)
                .pt(16.0)
                .pb(18.0)
                .center_y()
                .child(t.faint("Each workspace remembers its login"))
                .child(el().grow(1.0))
                .child(t.button("Cancel", Btn::Default, None, |s| s.new_open = false).px(14.0).key("cancel"))
                .child(start),
        );
    scrim("sheet-layer", 0.32, |s| s.new_open = false).pt(t.toolbar_h).pb(20.0).items_center().child(sheet)
}

fn gnome_dialog(s: &App, t: &Theme) -> El {
    let ready = s.new_ws.is_some() && s.new_agent.is_some();
    let agents = s.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
    let cur = agents.iter().find(|a| Some(login_key(a)) == s.new_agent);
    let default = s.new_ws.as_ref().and_then(|w| s.default_login(w));
    let cur_label = cur.map(|a| if default.as_deref() == Some(login_key(a).as_str()) { format!("{} · Default", a.label()) } else { a.label() });
    let login = row()
        .key("login-row")
        .w_full()
        .h(48.0)
        .px(14.0)
        .gap(12.0)
        .center_y()
        .radius(t.box_radius)
        .fill(t.card)
        .role("combobox")
        .label("Login")
        .expanded(s.menu == Some(Menu::Login))
        .focusable(true)
        .hover(Color::WHITE.alpha(0.03))
        .on_press(|s: &mut App| s.menu = if s.menu == Some(Menu::Login) { None } else { Some(Menu::Login) })
        .child(match cur {
            Some(a) => agent_badge(t, a.agent, 24.0, true),
            None => el().size(24.0, 24.0),
        })
        .child(t.label("Login"))
        .child(el().grow(1.0))
        .child(t.small(cur_label.unwrap_or_else(|| "Choose…".into())).font(TextStyle::new(t.fs, 400.0)))
        .child(icon(&ic::CHEVRON_DOWN, 14.0).color(t.text));
    let agent_block = col().w_full().gap(10.0).child(t.group_title("Agent")).child(login);
    // The drop-down floats over the dialog, under the Login row.
    let login_menu = (s.menu == Some(Menu::Login)).then(|| {
        let items = agents
            .iter()
            .map(|a| {
                let key = login_key(a);
                let on = Some(&key) == s.new_agent.as_ref();
                let note = if !a.available {
                    a.problem.clone().map(|p| capitalize(&p)).unwrap_or_default()
                } else if default.as_deref() == Some(key.as_str()) {
                    "Default".to_string()
                } else {
                    String::new()
                };
                let mut it = Item::new(&a.label(), note, move |s, _| s.new_agent = Some(key.clone())).checked(on);
                if !a.available {
                    it = it.enabled(false);
                }
                Some(it)
            })
            .collect();
        menu_box(t, items, 300.0).key("login-menu").anchored("login-row", rune::Side::Bottom, rune::Align::End, 6.0).raise()
    });
    let body = col()
        .w_full()
        .gap(20.0)
        .px(24.0)
        .pt(12.0)
        .pb(24.0)
        .children(from_note(s).map(|n| t.small(n).truncate()))
        .child(col().w_full().gap(10.0).child(t.group_title("Workspace")).child(t.group(workspace_rows(s, t), 0.0)))
        .child(agent_block);
    let body = match make_default_box(s, t) {
        Some(b) => body.child(b),
        None => body,
    };
    let body = body.child(t.small("Ctrl+Enter starts the session").font(TextStyle::new(13.0, 400.0)));
    let header = row()
        .w_full()
        .h(48.0)
        .px(8.0)
        .center_y()
        .child(t.button("Cancel", Btn::Default, None, |s| {
            s.new_open = false;
            s.menu = None
        }))
        .child(row().grow(1.0).justify_center().child(text("New Session").font(TextStyle::new(14.5, 700.0)).color(t.text)))
        .child(t.button("Start", if ready { Btn::Primary } else { Btn::Default }, None, |s| s.start_from_sheet()).key("start-session").default_button());
    let dialog = col()
        .key("new-session")
        .w(520.0)
        .shrink()
        .min_h(0.0)
        .radius(t.dialog_radius)
        .fill(t.sheet)
        .border(1.0, Color::BLACK.alpha(0.6))
        .shadow(20.0, 60.0, Color::BLACK.alpha(0.5))
        .modal(true, |s: &mut App| {
            s.new_open = false;
            s.menu = None
        })
        .child(header)
        .child(col().w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(body));
    let dialog = match login_menu {
        Some(m) => dialog.child(m),
        None => dialog,
    };
    scrim("sheet-layer", 0.5, |s| {
        s.new_open = false;
        s.menu = None
    })
    .py(30.0)
    .center()
    .child(dialog)
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts (GNOME's window; the same list works on macOS)
// ---------------------------------------------------------------------------

pub fn shortcut_sections(s: &App) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
    let l = |c| keys(s.look, c);
    let jump = if s.look.mac() { "⌘1…9".to_string() } else { "Alt+1…9".to_string() };
    let next_prev = if s.look.mac() { "⌥⌘↓ ↑".to_string() } else { "Alt+↓ ↑".to_string() };
    vec![
        (
            "Sessions",
            vec![
                ("New session", l(Cmd::NewSession)),
                ("New quick task", l(Cmd::QuickTask)),
                ("Next / previous session", next_prev),
                ("Go to session 1–9", jump),
                ("Search sessions", l(Cmd::Search)),
                ("Archive session", l(Cmd::Archive)),
            ],
        ),
        ("Approvals", vec![("Approve request", l(Cmd::Approve)), ("Deny request", l(Cmd::Deny)), ("Next waiting approval", l(Cmd::NextApproval))]),
        (
            "Composer",
            vec![
                ("Send", l(Cmd::Send)),
                ("New line", if s.look.mac() { "⇧↩".into() } else { "Shift+Enter".into() }),
                ("Stop the turn", l(Cmd::Stop)),
                ("Attach files", l(Cmd::Attach)),
                ("Dictate", l(Cmd::Dictate)),
                ("Edit your last message", "↑".into()),
                ("Show or hide side questions", l(Cmd::ToggleSide)),
                ("Show or hide the session's changes", l(Cmd::ToggleChanges)),
                ("Show or hide the session's files", l(Cmd::ToggleFiles)),
            ],
        ),
        (
            "Terminal",
            vec![
                ("Show or hide the terminal", l(Cmd::ToggleTerminal)),
                ("Show or hide the browser", l(Cmd::ToggleBrowser)),
                ("Copy", if s.look.mac() { "⌘C".into() } else { "Ctrl+Shift+C".into() }),
                ("Paste", if s.look.mac() { "⌘V".into() } else { "Ctrl+Shift+V".into() }),
                ("Scroll back a page", if s.look.mac() { "⇧Page Up".into() } else { "Shift+Page Up".into() }),
            ],
        ),
        (
            "General",
            vec![
                ("Preferences", l(Cmd::Settings)),
                ("Toggle sidebar", l(Cmd::ToggleSidebar)),
                ("Main menu", l(Cmd::MainMenu)),
                ("Keyboard shortcuts", l(Cmd::Shortcuts)),
                ("Quit", l(Cmd::Quit)),
            ],
        ),
    ]
}

/// Key caps: "Ctrl+Shift+N" → [Ctrl][Shift][N].
fn caps(t: &Theme, spec: &str) -> El {
    let mut r = row().gap(4.0).center_y();
    let parts: Vec<String> = spec.split('+').map(|p| p.trim().to_string()).collect();
    for p in parts.into_iter().filter(|p| !p.is_empty()) {
        r = r.child(row().h(24.0).px(7.0).center().radius(t.btn_radius).fill(Color::WHITE.alpha(0.10)).child(text(p).font(TextStyle::new(12.5, 400.0)).color(t.text)));
    }
    r
}

pub fn shortcuts(s: &App, t: &Theme) -> El {
    let sections = shortcut_sections(s);
    let section = |(title, rows): &(&'static str, Vec<(&'static str, String)>)| {
        let mut c = col().w_full().gap(8.0).child(text(title.to_string()).font(TextStyle::new(14.5, 700.0)).color(t.accent_text).pb(4.0));
        for (name, k) in rows.iter().filter(|(_, k)| !k.is_empty()) {
            c = c.child(row().w_full().h(28.0).center_y().child(text(name.to_string()).font(TextStyle::new(14.5, 400.0)).color(t.text)).child(el().grow(1.0)).child(caps(t, k)));
        }
        c
    };
    let left = col().grow(1.0).gap(24.0).child(section(&sections[0])).child(section(&sections[1]));
    let right = col().grow(1.0).gap(24.0).child(section(&sections[2])).child(section(&sections[3]));
    let panel = col()
        .key("shortcuts-window")
        .w(920.0)
        .max_w(920.0)
        .shrink()
        .radius(t.dialog_radius)
        .fill(rune::color::hex("#222226").unwrap_or(Color::BLACK))
        .border(1.0, Color::BLACK.alpha(0.6))
        .shadow(20.0, 60.0, Color::BLACK.alpha(0.5))
        .modal(true, |s: &mut App| s.shortcuts_open = false)
        .child(
            row()
                .w_full()
                .h(48.0)
                .px(12.0)
                .center_y()
                .child(el().w(24.0))
                .child(row().grow(1.0).justify_center().child(text("Keyboard Shortcuts").font(TextStyle::new(14.5, 700.0)).color(t.text)))
                .child(
                    row()
                        .size(24.0, 24.0)
                        .radius(12.0)
                        .center()
                        .fill(Color::WHITE.alpha(0.1))
                        .role("button")
                        .label("Close")
                        .on_press(|s: &mut App| s.shortcuts_open = false)
                        .child(icon(&ic::CLOSE, 10.0).color(t.text)),
                ),
        )
        .child(row().w_full().gap(40.0).px(36.0).pt(12.0).pb(28.0).items_start().child(left).child(right));
    scrim("shortcuts-layer", 0.4, |s| s.shortcuts_open = false).padding(30.0).center().child(panel)
}

/// Asked before archiving a live session: archiving stops the agent.
pub fn confirm_archive(s: &App, id: &str, t: &Theme) -> El {
    let title = s.session(id).map(|x| x.title.clone().unwrap_or_else(|| s.login_label(x))).unwrap_or_default();
    let id2 = id.to_string();
    let mac = t.mac();
    let label = format!("Archive \u{201c}{}\u{201d}", crate::view::clip(&title, 60));
    let buttons = row()
        .w_full()
        .gap(8.0)
        .child(el().grow(1.0))
        .child(t.button("Cancel", Btn::Default, None, |s| s.confirm_archive = None).key("cancel-archive"))
        .child(t.button("Archive", Btn::Primary, None, move |s| s.archive(&id2)).key("confirm-archive").default_button());
    let mut panel = col()
        .key("archive-alert")
        .role("alertdialog")
        .label(label)
        .w(if mac { 340.0 } else { 380.0 })
        .gap(if mac { 12.0 } else { 16.0 })
        .padding(if mac { 20.0 } else { 24.0 })
        .fill(t.sheet)
        .shadow(20.0, 50.0, Color::BLACK.alpha(0.5))
        .modal(true, |s: &mut App| s.confirm_archive = None)
        .child(text("Archive this session?").font(TextStyle::new(if mac { 13.0 } else { 16.0 }, 700.0)).color(t.text).wrap())
        .child(t.small("The agent stops and the session moves to Archived.").wrap())
        .child(buttons);
    panel = if mac { panel.radius(t.dialog_radius).border(0.5, Color::WHITE.alpha(0.18)) } else { panel.radius(t.dialog_radius).border(1.0, Color::BLACK.alpha(0.6)) };
    scrim("archive-layer", 0.32, |s| s.confirm_archive = None).center().child(panel)
}
