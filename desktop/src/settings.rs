//! Settings (⌘,) with toolbar tabs on macOS, Preferences (Ctrl+,) with a view switcher on
//! GNOME: General (host status), Agents, Workspaces, Connectors and Devices.
//!
//! Drawn in the main window, over it: Rune windows each own their state, and Settings edits
//! the same state as the main window (it would need a second copy kept in step).

use crate::bridge::{Refresh, Tag};
use crate::icons as ic;
use crate::look::{agent_badge, Btn, Theme};
use crate::model::{login_key, App, Tab};
use crate::session::pre;
use crate::sheet::{scrim, tilde};
use crate::view::{clip, short_age};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, Icon, ScrollSpec, TextStyle, Tx};
use serde_json::json;

fn tab_icon(tab: Tab) -> &'static Icon {
    match tab {
        Tab::General => &ic::TAB_GENERAL,
        Tab::Agents => &ic::TAB_AGENTS,
        Tab::Workspaces => &ic::TAB_WORKSPACES,
        Tab::Connectors => &ic::TAB_CONNECTORS,
        Tab::Devices => &ic::TAB_DEVICES,
        Tab::Archived => &ic::TAB_ARCHIVED,
    }
}

pub fn view(s: &App, tab: Tab, t: &Theme) -> El {
    let body = match tab {
        Tab::General => general(s, t),
        Tab::Agents => agents(s, t),
        Tab::Workspaces => workspaces(s, t),
        Tab::Connectors => connectors(s, t),
        Tab::Devices => devices(s, t),
        Tab::Archived => archived(s, t),
    };
    if t.mac() {
        mac_window(s, tab, t, body)
    } else {
        gnome_window(s, tab, t, body)
    }
}

fn close(s: &mut App) {
    s.settings = None;
    s.pairing = None;
}

fn mac_window(s: &App, tab: Tab, t: &Theme, body: El) -> El {
    let dot = |c: Color| el().size(12.0, 12.0).radius(6.0).fill(c);
    let lights = row()
        .w(52.0)
        .gap(8.0)
        .center_y()
        .child(
            row()
                .size(12.0, 12.0)
                .role("button")
                .label("Close Settings")
                .on_press(close)
                .child(dot(rune::color::hex("#ff5f57").unwrap_or(t.danger))),
        )
        .child(dot(Color::WHITE.alpha(0.18)))
        .child(dot(Color::WHITE.alpha(0.18)));
    let mut tabs = row().gap(4.0).justify_center().role("tablist").focus_group();
    for tb in Tab::ALL {
        let on = tb == tab;
        let color = if on { t.accent } else { t.text2 };
        let mut b = col()
            .key(format!("tab-{}", tb.title().to_lowercase()))
            .size(76.0, 48.0)
            .center()
            .gap(3.0)
            .radius(7.0)
            .role("tab")
            .label(tb.title())
            .checked(on)
            .focusable(true)
            .on_press(move |s: &mut App| s.open_settings(tb))
            .child(icon(tab_icon(tb), 22.0).color(color))
            .child(text(tb.title()).font(TextStyle::new(11.0, 400.0)).color(color));
        b = if on { b.fill(Color::WHITE.alpha(0.1)) } else { b.hover(Color::WHITE.alpha(0.05)) };
        tabs = tabs.child(b);
    }
    let header = col()
        .w_full()
        .fill(t.toolbar)
        .border_sides([0.0, 0.0, 0.5, 0.0], Color::BLACK.alpha(0.6))
        .child(row().w_full().h(28.0).px(12.0).center_y().child(lights).child(row().grow(1.0).justify_center().child(text(tab.title()).font(TextStyle::new(13.0, 600.0)).color(t.text))).child(el().w(52.0)))
        .child(row().w_full().justify_center().pt(6.0).pb(8.0).child(tabs));
    let mut panel = col()
        .key("settings")
        .w(760.0)
        .max_w(760.0)
        .shrink()
        .min_h(0.0)
        .radius(12.0)
        .fill(t.panel)
        .border(0.5, Color::WHITE.alpha(0.18))
        .shadow(30.0, 80.0, Color::BLACK.alpha(0.55))
        .clip_rounded(12.0)
        .child(header)
        .child(col().key("settings-body").w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(body.px(24.0).pt(20.0).pb(24.0)));
    let pairing = tab == Tab::Devices && s.pairing.is_some();
    if pairing {
        // The sheet hangs from the tab bar, over a dimmed tab.
        panel = panel.min_h(640.0).child(el().pin(Some(0.0), Some(94.0), Some(0.0), Some(0.0)).fill(Color::BLACK.alpha(0.3)));
    } else {
        panel = panel.modal(true, close);
    }
    // Settings windows keep their top edge where it is as tabs change height.
    let mut layer = scrim("settings-layer", 0.32, close).pt(40.0).px(24.0).pb(24.0).items_center().child(panel);
    if pairing {
        layer = layer.child(col().key("pair-layer").pin(Some(0.0), Some(40.0 + 94.0), Some(0.0), None).items_center().child(pairing_sheet(s, t)));
    }
    layer
}

fn gnome_window(s: &App, tab: Tab, t: &Theme, body: El) -> El {
    let mut switcher = row().gap(6.0).role("tablist").focus_group();
    for tb in Tab::ALL {
        let on = tb == tab;
        let mut b = row()
            .key(format!("tab-{}", tb.title().to_lowercase()))
            .h(34.0)
            .px(14.0)
            .gap(8.0)
            .center_y()
            .radius(7.0)
            .role("tab")
            .label(tb.title())
            .checked(on)
            .focusable(true)
            .on_press(move |s: &mut App| s.open_settings(tb))
            .child(icon(tab_icon(tb), 16.0).color(if on { t.text } else { t.text2 }))
            .child(text(tb.title()).font(TextStyle::new(14.5, 700.0)).color(if on { t.text } else { t.text2 }));
        b = if on { b.fill(Color::WHITE.alpha(0.1)) } else { b.hover(Color::WHITE.alpha(0.05)) };
        switcher = switcher.child(b);
    }
    let header = row()
        .w_full()
        .h(54.0)
        .px(10.0)
        .center_y()
        .child(el().w(24.0))
        .child(row().grow(1.0).justify_center().child(switcher))
        .child(
            row()
                .size(24.0, 24.0)
                .radius(12.0)
                .center()
                .fill(Color::WHITE.alpha(0.1))
                .role("button")
                .label("Close Preferences")
                .on_press(close)
                .child(icon(&ic::CLOSE, 10.0).color(t.text)),
        );
    let mut panel = col()
        .key("settings")
        .w(880.0)
        .max_w(880.0)
        .shrink()
        .min_h(0.0)
        .radius(14.0)
        .fill(t.panel)
        .border(1.0, Color::BLACK.alpha(0.6))
        .shadow(20.0, 60.0, Color::BLACK.alpha(0.5))
        .clip_rounded(14.0)
        .child(header)
        .child(col().key("settings-body").w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(row().w_full().px(40.0).pt(10.0).pb(32.0).justify_center().child(body.max_w(680.0))));
    let pairing = tab == Tab::Devices && s.pairing.is_some();
    if pairing {
        panel = panel.min_h(640.0).child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).fill(Color::BLACK.alpha(0.4)));
    } else {
        panel = panel.modal(true, close);
    }
    let mut layer = scrim("settings-layer", 0.5, close).padding(30.0).center().child(panel);
    if pairing {
        layer = layer.child(col().key("pair-layer").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().child(pairing_sheet(s, t)));
    }
    layer
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

pub(crate) fn section(t: &Theme, title: &str, note: Option<String>, content: El) -> El {
    let mut head = row().w_full().gap(8.0).center_y().child(t.section_title(title)).child(el().grow(1.0));
    if let Some(n) = note {
        head = head.child(t.faint(n));
    }
    col().w_full().gap(if t.mac() { 6.0 } else { 10.0 }).child(head).child(content)
}

/// A form row: a title (and detail under it) at the start, `trailing` at the end.
pub(crate) fn form_row(t: &Theme, title: &str, detail: Option<String>, trailing: El) -> El {
    let mut names = col().grow(1.0).min_w(0.0).gap(1.0).child(t.label(title.to_string()));
    if let Some(d) = detail {
        names = names.child(t.small(d).wrap());
    }
    row().w_full().gap(10.0).px(if t.mac() { 12.0 } else { 14.0 }).py(if t.mac() { 8.0 } else { 12.0 }).center_y().child(names).child(trailing)
}

pub(crate) fn footnote(t: &Theme, msg: &str) -> El {
    text(msg.to_string()).font(TextStyle::new(if t.mac() { 11.0 } else { 13.0 }, 400.0).lh(if t.mac() { 16.0 } else { 19.0 })).color(t.text3).wrap()
}

fn policy_name(p: ToolPolicy) -> &'static str {
    match p {
        ToolPolicy::Auto => "Allow",
        ToolPolicy::Ask => "Ask Every Time",
        ToolPolicy::Disabled => "Off",
    }
}
fn policy_short(p: ToolPolicy) -> &'static str {
    match p {
        ToolPolicy::Auto => "Allow",
        ToolPolicy::Ask => "Ask",
        ToolPolicy::Disabled => "Off",
    }
}
fn policy_id(p: ToolPolicy) -> &'static str {
    match p {
        ToolPolicy::Auto => "auto",
        ToolPolicy::Ask => "ask",
        ToolPolicy::Disabled => "disabled",
    }
}
fn policy_of(id: &str) -> Option<ToolPolicy> {
    Some(match id {
        "auto" => ToolPolicy::Auto,
        "ask" => ToolPolicy::Ask,
        "disabled" => ToolPolicy::Disabled,
        _ => return None,
    })
}
fn next_policy(p: ToolPolicy) -> ToolPolicy {
    match p {
        ToolPolicy::Auto => ToolPolicy::Ask,
        ToolPolicy::Ask => ToolPolicy::Disabled,
        ToolPolicy::Disabled => ToolPolicy::Auto,
    }
}

/// Set a connector's policy: `which` is `read`, `write`, or a tool's name (`None` clears its
/// override).
fn set_policy(s: &mut App, cid: &str, which: &str, p: Option<ToolPolicy>) {
    let params = match which {
        "read" => json!({"connector_id": cid, "read_policy": p}),
        "write" => json!({"connector_id": cid, "write_policy": p}),
        tool => json!({"connector_id": cid, "tool_policies": {tool: p}}),
    };
    s.act("Set tool policy", m::CONNECTOR_UPDATE, params, Refresh::Connectors);
}

/// A pop-up picker for a policy: the native menu on macOS, else each press steps to the next.
fn pick_policy(s: &mut App, tx: &mut Tx, key: String, cid: String, which: String, current: ToolPolicy, can_reset: bool) {
    if rune::popup_menu_supported() {
        let mut menu = rune::PopupMenu::new();
        for p in [ToolPolicy::Auto, ToolPolicy::Ask, ToolPolicy::Disabled] {
            menu = menu.item(rune::PopupItem::new(policy_id(p), policy_name(p)).checked(p == current));
        }
        if can_reset {
            menu = menu.separator().item(rune::PopupItem::new("default", "Use the Default"));
        }
        tx.popup_menu(menu, key, move |s: &mut App, picked, _| match picked {
            Some("default") => set_policy(s, &cid, &which, None),
            Some(id) => set_policy(s, &cid, &which, policy_of(id)),
            None => {}
        });
    } else {
        set_policy(s, &cid, &which, Some(next_policy(current)));
    }
}

pub(crate) fn popup_button(t: &Theme, key: String, label: &str) -> El {
    let mac = t.mac();
    row()
        .key(key)
        .h(if mac { 22.0 } else { 34.0 })
        .pl(if mac { 9.0 } else { 12.0 })
        .pr(if mac { 6.0 } else { 10.0 })
        .gap(8.0)
        .center_y()
        .radius(if mac { 5.0 } else { 7.0 })
        .fill(t.control)
        .role("combobox")
        .label(label.to_string())
        .focusable(true)
        .hover(Color::WHITE.alpha(0.06))
        .child(text(label.to_string()).font(TextStyle::new(if mac { 12.0 } else { 14.0 }, 400.0)).color(t.text))
        .child(icon(if mac { &ic::UPDOWN } else { &ic::CHEVRON_DOWN }, 10.0).color(t.text2))
}

/// A selectable row in a source list (Workspaces, Connectors).
fn source_row(t: &Theme, key: String, selected: bool, glyph: El, name: &str, detail: El, f: impl Fn(&mut App) + 'static) -> El {
    let mac = t.mac();
    let mut r = row()
        .key(key)
        .w_full()
        .gap(8.0)
        .px(8.0)
        .py(if mac { 6.0 } else { 10.0 })
        .center_y()
        .radius(if mac { 6.0 } else { 8.0 })
        .role("listitem")
        .label(name.to_string())
        .focusable(true)
        .on_press(f)
        .child(row().w(22.0).center().child(glyph))
        .child(col().grow(1.0).min_w(0.0).gap(1.0).child(text(name.to_string()).font(t.font()).color(if selected && mac { Color::WHITE } else { t.text }).truncate()).child(detail));
    r = if selected { r.fill(if mac { t.accent } else { Color::WHITE.alpha(0.1) }) } else { r.hover(Color::WHITE.alpha(0.04)) };
    r
}

/// The two-pane layout: a source list on the left, the detail on the right.
fn split(t: &Theme, list: El, detail: El) -> El {
    let mac = t.mac();
    let mut l = col().w(220.0).min_h(300.0).radius(t.box_radius).fill(t.boxed).clip_rounded(t.box_radius).child(list.grow(1.0));
    if mac {
        l = l.border(0.5, t.sep);
    }
    row().w_full().gap(16.0).child(l).child(detail.grow(1.0).min_w(0.0))
}

// ---------------------------------------------------------------------------
// General: host status
// ---------------------------------------------------------------------------

fn general(s: &App, t: &Theme) -> El {
    let mut c = col().key("general").w_full().gap(18.0);
    let Some(info) = &s.info else {
        return c.child(t.small("Waiting for the host…"));
    };
    c = c.child(
        row()
            .w_full()
            .gap(12.0)
            .center_y()
            .child(el().w(44.0).h(44.0).pixels(crate::brand::logo()))
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(2.0)
                    .child(text("AgentHost").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
                    .help(format!("Host {} · version {} · {} · protocol {}", info.host_id, info.version, info.os, info.protocol_version)),
            )
            .child(t.button("Re-run Checks", Btn::Default, None, |s| s.call(Tag::HostInfo, m::HOST_CHECK, json!({}))).key("rerun")),
    );
    let ok = |b: bool, yes: &str, no: &str| t.status(if b { t.ok } else { t.warn }, if b { yes } else { no }, !b);
    let sb = &info.sandbox;
    let mut rows = vec![
        form_row(t, "Sandbox", Some(format!("{} keeps each agent inside its workspace and network rules", sb.backend)), ok(sb.ok, "Working", "Needs attention")),
        form_row(t, "sandbox-runtime", None, t.small(sb.runtime_version.clone().unwrap_or_else(|| "Not installed".into()))),
    ];
    for p in &sb.prerequisites {
        let trailing = if p.present { icon(&ic::CHECK, 14.0).color(t.ok) } else { t.mono(p.install.clone().unwrap_or_else(|| "missing".into())) };
        rows.push(form_row(t, &p.name, None, trailing));
    }
    if let Some(pr) = &sb.problem {
        rows.push(form_row(t, "Problem", Some(pr.clone()), el()));
    }
    c = c.child(section(t, "Sandbox", Some("What keeps agents in their workspace".into()), t.group(rows, if t.mac() { 12.0 } else { 0.0 })));
    c = c.child(section(t, if t.mac() { "Voice input" } else { "Voice Input" }, None, voice_box(s, t)));
    let r = &info.relay;
    let relay = vec![
        form_row(
            t,
            "Status",
            r.error.clone(),
            if !r.configured {
                t.small("Off")
            } else {
                t.status(if r.connected { t.ok } else { t.warn }, if r.connected { "Connected" } else { "Disconnected" }, !r.connected)
            },
        ),
        form_row(t, "Address", None, t.mono(r.url.clone().unwrap_or_else(|| "Not configured: start the host with --relay-url to pair phones".into())).font(TextStyle::new(t.fs_mono, 400.0).mono()).color(t.text2)),
    ];
    c.child(section(t, "Relay", Some("How your phone reaches this computer".into()), t.group(relay, if t.mac() { 12.0 } else { 0.0 })))
}

fn voice_box(s: &App, t: &Theme) -> El {
    let Some(v) = &s.voice else {
        return t.group(vec![form_row(t, "Speech model", Some("This host has no voice input. Update agenthostd.".into()), el())], 0.0);
    };
    let detail = format!("{} · on this computer · {} MB", v.model, v.download_bytes / 1_000_000);
    let trailing = if v.ready {
        text("Installed").font(TextStyle::new(if t.mac() { 12.0 } else { 13.0 }, 400.0)).color(t.ok)
    } else if let Some(p) = &v.progress {
        t.small(format!("Downloading… {}%", p.received * 100 / p.total.max(1)))
    } else {
        t.small_button("Download", |s| s.download_voice())
    };
    let mut rows = vec![form_row(t, "Speech model", Some(detail), trailing)];
    if let (Some(e), None) = (&v.error, &v.progress) {
        rows.push(form_row(t, "Last error", Some(e.clone()), el()));
    }
    t.group(rows, if t.mac() { 12.0 } else { 0.0 })
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

fn login_detail(a: &AgentStatus) -> String {
    let mut parts = vec![if a.profile_name.is_some() { "Profile".to_string() } else { "Default login".to_string() }];
    if let Some(d) = &a.config_dir {
        parts.push(tilde(d));
    }
    if a.profile_name.is_none() {
        if let Some(v) = &a.version {
            parts.push(v.clone());
        }
    }
    parts.join(" · ")
}

fn sessions_of(s: &App, key: &str) -> (usize, usize) {
    let mine: Vec<&Session> = s.sessions.iter().filter(|x| x.profile_id.clone().unwrap_or_else(|| x.agent.as_str().to_string()) == key).collect();
    (mine.iter().filter(|x| !x.state.is_terminal()).count(), mine.len())
}

fn agents(s: &App, t: &Theme) -> El {
    let Some(info) = &s.info else { return col().w_full().child(t.small("Waiting for the host…")) };
    if !t.mac() {
        return gnome_agents(s, info, t);
    }
    let selected = s.sel_agent.clone().or_else(|| info.agents.iter().find(|a| a.profile_id.is_some() && a.profile_name.is_some()).map(login_key));
    let mut rows = vec![];
    for a in &info.agents {
        let key = login_key(a);
        let on = selected.as_deref() == Some(key.as_str());
        let status = if a.available {
            t.status(t.ok, "Ready", false)
        } else {
            t.status(t.warn, &a.problem.clone().map(|p| capitalize(&p)).unwrap_or_else(|| "Unavailable".into()), true)
        };
        let k2 = key.clone();
        let mut r = row()
            .key(format!("agent-{key}"))
            .w_full()
            .gap(10.0)
            .px(12.0)
            .py(10.0)
            .center_y()
            .role("listitem")
            .label(a.label())
            .focusable(true)
            .on_press(move |s: &mut App| s.sel_agent = Some(k2.clone()))
            .child(agent_badge(t, a.agent, 26.0, true))
            .child(col().grow(1.0).min_w(0.0).child(t.label(a.label())).child(t.small(login_detail(a))))
            .child(status);
        if on {
            r = r.fill(t.accent.alpha(0.22));
        }
        rows.push(r);
    }
    let sel = info.agents.iter().find(|a| Some(login_key(a)) == selected);
    let removable = sel.and_then(|a| a.profile_name.as_ref().and(a.profile_id.clone()));
    let remove: Option<Box<dyn Fn(&mut App)>> = removable.map(|id| {
        Box::new(move |s: &mut App| {
            s.act("Remove profile", m::AGENT_PROFILE_REMOVE, json!({"profile_id": id}), Refresh::HostInfo);
            s.sel_agent = None;
        }) as Box<dyn Fn(&mut App)>
    });
    let list = t.group(rows, 48.0).child(t.plus_minus(
        "Add a Login",
        |s: &mut App, _: &mut Tx| {
            s.adding_profile = true;
            s.profile_name.focused = true;
        },
        remove,
        "+ adds another login (a profile) of an agent",
    ));
    let mut c = col().key("agents").w_full().gap(18.0).child(section(t, "Agent logins", Some("Each login can be picked per session".into()), list));
    if s.adding_profile {
        c = c.child(profile_form(s, info, t));
    } else if let Some(a) = sel {
        c = c.child(agent_detail(s, a, t));
    }
    c
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn agent_detail(s: &App, a: &AgentStatus, t: &Theme) -> El {
    let key = login_key(a);
    let (live, total) = sessions_of(s, &key);
    let env = a.agent.profile_env().unwrap_or("its config folder setting");
    let mut rows = vec![];
    if let Some(n) = &a.profile_name {
        rows.push(form_row(t, "Name", None, t.label(n.clone())));
    }
    if let Some(d) = &a.config_dir {
        let dir = d.clone();
        rows.push(form_row(
            t,
            "Config folder",
            Some(format!("What {env} points at")),
            row().gap(10.0).center_y().child(t.mono(tilde(d)).color(t.text2)).child(t.small_button(if t.mac() { "Show in Finder" } else { "Open Folder" }, move |s| s.reveal(&dir))),
        ));
    }
    let login_state = if a.available { "Logged in".to_string() } else { capitalize(a.problem.as_deref().unwrap_or("not logged in")) };
    let fix = a.fix.clone();
    let login_btn = match fix {
        Some(f) if !a.available => t.small_button_tx("Copy Login Command", move |s, tx| {
            tx.copy(f.clone());
            s.notice = Some(format!("Copied. Run it in a terminal: {f}"));
        }),
        _ => el(),
    };
    rows.push(form_row(t, "Login", Some(login_state), login_btn));
    rows.push(form_row(t, "Sessions", None, t.small(format!("{live} live · {total} total")).font(TextStyle::new(t.fs, 400.0))));
    let note = if a.profile_name.is_some() {
        "Sessions on this login can write only to its folder, and no other session can read it. Removing the profile keeps the folder."
    } else {
        "The agent's own login. Add a profile with + to use a second subscription (work and personal, say)."
    };
    col().w_full().gap(6.0).child(t.section_title(&a.label())).child(t.group(rows, 12.0)).child(footnote(t, note))
}

/// "Add a login": another login of an agent, kept in its own config folder.
fn profile_form(s: &App, info: &HostInfo, t: &Theme) -> El {
    let mut kinds: Vec<AgentKind> = vec![];
    for a in &info.agents {
        if a.agent.profile_env().is_some() && !kinds.contains(&a.agent) {
            kinds.push(a.agent);
        }
    }
    let mut pick = row().gap(6.0).flex_wrap();
    for kind in kinds {
        let on = s.profile_agent == Some(kind);
        pick = pick.child(t.button(kind.display_name(), if on { Btn::Primary } else { Btn::Default }, None, move |s| s.profile_agent = Some(kind)).key(format!("kind-{}", kind.as_str())));
    }
    let setting = s
        .profile_agent
        .and_then(|a| a.profile_env())
        .map(|v| format!("The folder the agent's {v} setting points at, e.g. ~/.claude-work. Log in there once (the command shows after adding)."))
        .unwrap_or_else(|| "Pick the agent, then give the login a name and its config folder.".into());
    let choose = t.small_button_tx("Choose…", |_, tx| {
        let options = rune::FileDialogOptions { title: "The agent's config folder".into(), ..Default::default() };
        tx.open_folder(options, |s: &mut App, result, _| match result {
            Ok(Some(p)) => s.profile_dir.replace_text(&p.display().to_string()),
            Ok(None) => {}
            Err(e) => s.error = Some(format!("Could not open the folder picker: {e}")),
        });
    });
    let field_w = if t.mac() { 300.0 } else { 340.0 };
    let rows = vec![
        form_row(t, "Agent", None, pick),
        form_row(t, "Name", None, row().w(field_w).child(t.field(&s.profile_name, "Work", |s| &mut s.profile_name))),
        form_row(t, "Config folder", Some(setting), row().w(field_w).gap(6.0).center_y().child(row().grow(1.0).min_w(0.0).child(t.field_submit(&s.profile_dir, "~/.claude-work", |s| &mut s.profile_dir, |s: &mut App, _: &mut Tx| s.add_profile()))).child(choose)),
    ];
    col()
        .key("profile-form")
        .w_full()
        .gap(6.0)
        .child(t.section_title("New login"))
        .child(t.group(rows, 12.0))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .pt(6.0)
                .child(footnote(t, "Each login is its own subscription. New sessions can pick it; its folder stays hidden from every other session.").shrink())
                .child(el().grow(1.0))
                .child(t.button("Cancel", Btn::Default, None, |s| s.adding_profile = false))
                .child(t.button("Add Login", Btn::Primary, None, |s| s.add_profile()).key("add-profile")),
        )
}

fn gnome_agents(s: &App, info: &HostInfo, t: &Theme) -> El {
    let mut rows = vec![];
    for a in &info.agents {
        let key = login_key(a);
        let profile = a.profile_name.is_some();
        let open = profile && s.sel_agent.as_deref() == Some(key.as_str());
        let k2 = key.clone();
        let mut r = row()
            .key(format!("agent-{key}"))
            .w_full()
            .gap(12.0)
            .px(14.0)
            .py(12.0)
            .center_y()
            .child(agent_badge(t, a.agent, 32.0, true))
            .child(col().grow(1.0).min_w(0.0).child(t.label(a.label())).child(t.small(if a.available { login_detail(a) } else { capitalize(a.problem.as_deref().unwrap_or("not logged in")) })));
        if a.available {
            r = r.child(text("Ready").font(TextStyle::new(13.0, 400.0)).color(t.ok));
        } else if let Some(f) = a.fix.clone() {
            r = r.child(t.button_tx("How to Log In", Btn::Default, None, move |s, tx| {
                tx.copy(f.clone());
                s.notice = Some(format!("Copied. Run it in a terminal: {f}"));
            }));
        }
        if profile {
            r = r
                .role("button")
                .expanded(open)
                .focusable(true)
                .on_press(move |s: &mut App| s.sel_agent = if s.sel_agent.as_deref() == Some(k2.as_str()) { None } else { Some(k2.clone()) })
                .child(icon(if open { &ic::CHEVRON_UP } else { &ic::CHEVRON_DOWN }, 14.0).color(t.text));
        }
        let mut block = col().w_full().child(r);
        if open {
            let (live, _) = sessions_of(s, &key);
            let env = a.agent.profile_env().unwrap_or("");
            let sub = |title: &str, detail: El, trailing: El| {
                row()
                    .w_full()
                    .pl(58.0)
                    .pr(14.0)
                    .py(10.0)
                    .gap(10.0)
                    .center_y()
                    .border_sides([1.0, 0.0, 0.0, 0.0], Color::BLACK.alpha(0.25))
                    .child(col().grow(1.0).min_w(0.0).gap(2.0).child(t.label(title.to_string())).child(detail))
                    .child(trailing)
            };
            if let Some(n) = &a.profile_name {
                block = block.child(
                    row()
                        .w_full()
                        .pl(58.0)
                        .pr(14.0)
                        .py(10.0)
                        .border_sides([1.0, 0.0, 0.0, 0.0], Color::BLACK.alpha(0.25))
                        .child(col().gap(2.0).child(t.small("Name")).child(t.label(n.clone()))),
                );
            }
            if let Some(d) = &a.config_dir {
                let dir = d.clone();
                block = block.child(sub(
                    "Config Folder",
                    t.mono_small(format!("{} · {env}", tilde(d))),
                    row().size(34.0, 34.0).center().radius(7.0).role("button").label("Open the folder").hover(Color::WHITE.alpha(0.08)).on_press(move |s: &mut App| s.reveal(&dir)).child(icon(&ic::FOLDER, 16.0).color(t.text)),
                ));
            }
            let login = if a.available { format!("Logged in · {live} live session{}", if live == 1 { "" } else { "s" }) } else { capitalize(a.problem.as_deref().unwrap_or("")) };
            let copy = match a.fix.clone() {
                Some(f) => t.button_tx("Copy Login Command", Btn::Default, None, move |s, tx| {
                    tx.copy(f.clone());
                    s.notice = Some(format!("Copied: {f}"));
                }),
                None => el(),
            };
            block = block.child(sub("Login", t.small(login), copy));
            if let Some(id) = a.profile_id.clone() {
                block = block.child(
                    row().w_full().pl(58.0).py(10.0).border_sides([1.0, 0.0, 0.0, 0.0], Color::BLACK.alpha(0.25)).child(t.button("Remove Profile…", Btn::Destructive, None, move |s| {
                        s.act("Remove profile", m::AGENT_PROFILE_REMOVE, json!({"profile_id": id}), Refresh::HostInfo);
                        s.sel_agent = None;
                    })),
                );
            }
            block = block.fill(Color::WHITE.alpha(0.02));
        }
        rows.push(block);
    }
    let head = row()
        .w_full()
        .center_y()
        .child(col().grow(1.0).gap(2.0).child(t.section_title("Agent Logins")).child(t.small("Each login can be picked when you start a session").font(TextStyle::new(14.0, 400.0))))
        .child(
            row()
                .h(34.0)
                .px(12.0)
                .gap(6.0)
                .center_y()
                .radius(7.0)
                .fill(t.control)
                .role("button")
                .label("Add Profile")
                .hover(Color::WHITE.alpha(0.06))
                .on_press(|s: &mut App| s.adding_profile = true)
                .child(icon(&ic::PLUS, 13.0).color(t.text))
                .child(text("Add Profile").font(TextStyle::new(14.5, 700.0)).color(t.text)),
        );
    let mut c = col().key("agents").w_full().gap(24.0).child(col().w_full().gap(12.0).child(head).child(t.group(rows, 0.0)));
    if s.adding_profile {
        c = c.child(profile_form(s, info, t));
    }
    c.child(col().w_full().gap(12.0).child(t.section_title("Voice Input")).child(voice_box(s, t)))
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

fn pick_workspace_folder(tx: &mut Tx) {
    let options = rune::FileDialogOptions { title: "Add a folder agents may work in".into(), ..Default::default() };
    tx.open_folder(options, |s: &mut App, result, _| match result {
        Ok(Some(p)) => s.add_workspace_path(&p.display().to_string(), false),
        Ok(None) => {}
        Err(_) => {
            // No picker here (no portal, headless): type the path instead.
            s.adding_workspace = true;
            s.ws_path.focused = true;
        }
    });
}

fn workspaces(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let mut ws: Vec<&Workspace> = s.workspaces.iter().collect();
    ws.sort_by_key(|w| w.kind != WorkspaceKind::Scratch);
    let selected = s.sel_ws.clone().filter(|id| s.workspace(id).is_some()).or_else(|| ws.iter().find(|w| w.kind != WorkspaceKind::Scratch).or(ws.first()).map(|w| w.workspace_id.clone()));
    let mut list = col().w_full().grow(1.0).padding(4.0).gap(2.0).role("list");
    for w in &ws {
        let on = selected.as_deref() == Some(w.workspace_id.as_str());
        let quick = w.kind == WorkspaceKind::Scratch;
        let glyph = if quick { icon(&ic::BOLT, 14.0).color(if on && mac { Color::WHITE } else { t.bolt }) } else { icon(&ic::FOLDER, 15.0).color(if on && mac { Color::WHITE } else { rune::color::hex("#5fb0ff").unwrap_or(t.accent) }) };
        let kind = match w.kind {
            WorkspaceKind::Scratch => "Built in",
            WorkspaceKind::Repo => "Git repo",
            WorkspaceKind::Folder => "Folder",
        };
        let id = w.workspace_id.clone();
        let detail = text(kind.to_string()).font(t.font_small()).color(if on && mac { Color::WHITE.alpha(0.8) } else { t.text2 });
        list = list.child(source_row(t, format!("ws-{}", w.workspace_id), on, glyph, if quick { "Quick tasks" } else { &w.name }, detail, move |s| s.sel_ws = Some(id.clone())));
    }
    let sel = selected.as_ref().and_then(|id| s.workspace(id));
    let remove: Option<Box<dyn Fn(&mut App)>> = sel.filter(|w| w.kind != WorkspaceKind::Scratch).map(|w| {
        let id = w.workspace_id.clone();
        Box::new(move |s: &mut App| {
            s.act("Remove workspace", m::WORKSPACE_REMOVE, json!({"workspace_id": id}), Refresh::Workspaces);
            s.sel_ws = None;
        }) as Box<dyn Fn(&mut App)>
    });
    let mut left = col().w_full().child(list);
    if mac {
        left = left.child(t.plus_minus("Add Folder", |_, tx| pick_workspace_folder(tx), remove, ""));
    } else {
        let mut bar = row().w_full().gap(6.0).padding(8.0).child(t.small_button_tx("Add Folder…", |_, tx| pick_workspace_folder(tx)));
        if let Some(f) = remove {
            bar = bar.child(t.small_button("Remove", move |s| f(s)));
        }
        left = left.child(bar);
    }
    let detail = match sel {
        Some(w) => workspace_detail(s, w, t),
        None => col().child(t.small("No workspaces yet.")),
    };
    let mut c = col().key("workspaces").w_full().gap(18.0);
    if s.adding_workspace {
        let add = |s: &mut App| {
            let p = s.ws_path.text.trim().to_string();
            if !p.is_empty() {
                s.add_workspace_path(&p, false);
            }
            s.ws_path.clear();
            s.adding_workspace = false;
        };
        c = c.child(t.group(
            vec![form_row(
                t,
                "Add a folder",
                Some("A repo or plain folder agents may read and change.".into()),
                row().w(360.0).gap(6.0).center_y().child(t.field(&s.ws_path, "/path/to/project", |s| &mut s.ws_path)).child(t.small_button("Add", add)).child(t.small_button("Cancel", |s| s.adding_workspace = false)),
            )],
            0.0,
        ));
    }
    c = c.child(split(t, left, detail));
    c.child(project_roots(s, t))
}

fn workspace_detail(s: &App, w: &Workspace, t: &Theme) -> El {
    let mac = t.mac();
    let quick = w.kind == WorkspaceKind::Scratch;
    let id = w.workspace_id.clone();
    let preset = |p: NetworkPreset, name: &str, detail: &str| {
        let on = w.network.presets.contains(&p);
        let net = w.network.clone();
        let id = id.clone();
        form_row(
            t,
            name,
            Some(detail.to_string()),
            t.switch(on, name, move |s: &mut App| {
                let mut net = net.clone();
                if on {
                    net.presets.retain(|x| *x != p);
                } else {
                    net.presets.push(p);
                }
                s.act("Change network rules", m::WORKSPACE_UPDATE, json!({"workspace_id": id, "network": net}), Refresh::Workspaces);
            })
            .key(format!("{}-{:?}", w.workspace_id, p)),
        )
    };
    let mut hosts = row().gap(6.0).center_y().flex_wrap();
    for h in &w.network.custom_hosts {
        let (id2, h2) = (id.clone(), h.clone());
        let net = w.network.clone();
        hosts = hosts.child(
            row()
                .h(if mac { 22.0 } else { 28.0 })
                .pl(8.0)
                .pr(5.0)
                .gap(5.0)
                .center_y()
                .radius(11.0)
                .fill(Color::WHITE.alpha(0.1))
                .child(text(h.clone()).font(TextStyle::new(if mac { 12.0 } else { 13.0 }, 400.0)).color(t.text))
                .child(
                    row()
                        .role("button")
                        .label(format!("Remove {h}"))
                        .on_press(move |s: &mut App| {
                            let mut n = net.clone();
                            n.custom_hosts.retain(|x| *x != h2);
                            s.act("Remove host", m::WORKSPACE_UPDATE, json!({"workspace_id": id2, "network": n}), Refresh::Workspaces);
                        })
                        .child(icon(&ic::XMARK_CIRCLE, 12.0).color(Color::WHITE.alpha(0.3))),
                ),
        );
    }
    let editing = s.host_for.as_deref() == Some(w.workspace_id.as_str());
    let add_host = if editing {
        let idh = id.clone();
        row().w(220.0).child(
            t.field_submit(&s.custom_host, "api.example.com", |s| &mut s.custom_host, move |s: &mut App, _: &mut Tx| {
                let h = s.custom_host.text.trim().to_string();
                if !h.is_empty() {
                    s.act("Add host", m::WORKSPACE_UPDATE, json!({"workspace_id": idh, "add_custom_host": h}), Refresh::Workspaces);
                }
                s.custom_host.clear();
                s.host_for = None;
            }),
        )
    } else {
        let idh = id.clone();
        t.small_button("Add Host…", move |s| {
            s.host_for = Some(idh.clone());
            s.custom_host = rune::TextEdit::new("");
            s.custom_host.focused = true;
        })
    };
    let extra = row()
        .w_full()
        .gap(10.0)
        .px(if mac { 12.0 } else { 14.0 })
        .py(if mac { 6.0 } else { 10.0 })
        .center_y()
        .child(t.label("Extra hosts"))
        .child(el().grow(1.0))
        .child(hosts)
        .child(add_host);
    let net = t.group(
        vec![
            preset(NetworkPreset::Web, "Open web", "Any public site. Your local network stays blocked."),
            preset(NetworkPreset::Packages, "Package registries", "npm, PyPI, crates.io, Go modules"),
            preset(NetworkPreset::Github, "GitHub", "github.com, its API and downloads"),
            extra,
        ],
        if mac { 12.0 } else { 0.0 },
    );
    let mut conns = vec![];
    for c in &s.connectors {
        let on = w.connectors.contains(&c.connector_id);
        let (idc, cid) = (id.clone(), c.connector_id.clone());
        let current = w.connectors.clone();
        conns.push(
            row()
                .w_full()
                .gap(10.0)
                .px(if mac { 12.0 } else { 14.0 })
                .py(if mac { 8.0 } else { 12.0 })
                .center_y()
                .child(
                    t.checkbox(on, &c.name, move |s: &mut App| {
                        let mut list = current.clone();
                        if on {
                            list.retain(|x| *x != cid);
                        } else {
                            list.push(cid.clone());
                        }
                        s.act("Change connectors", m::WORKSPACE_UPDATE, json!({"workspace_id": idc, "connectors": list}), Refresh::Workspaces);
                    })
                    .key(format!("{}-{}", w.workspace_id, c.connector_id)),
                )
                .child(t.label(c.name.clone()))
                .child(el().grow(1.0))
                .child(t.small(format!("{} tools", c.tools.len()))),
        );
    }
    if conns.is_empty() {
        conns.push(form_row(t, "No connectors", Some("Add one in the Connectors tab.".into()), el()));
    }
    let place = if quick {
        t.small(format!("No project folder: each session gets a fresh, empty folder of its own (in {}).", tilde(&w.path))).wrap()
    } else {
        t.mono(tilde(&w.path)).color(t.text2)
    };
    col()
        .w_full()
        .gap(if mac { 6.0 } else { 10.0 })
        .child(text(if quick { "Quick tasks".to_string() } else { w.name.clone() }).font(TextStyle::new(15.0, t.w_bold)).color(t.text))
        .child(place)
        .child(el().h(8.0))
        .child(t.group_title(if mac { "New sessions" } else { "New Sessions" }))
        .child(t.group(vec![default_login_row(s, w, t)], 0.0))
        .child(el().h(6.0))
        .child(t.group_title("Network"))
        .child(net)
        .child(footnote(t, "The agent's own provider hosts are always allowed."))
        .child(el().h(6.0))
        .child(t.group_title("Connectors"))
        .child(t.group(conns, if mac { 12.0 } else { 0.0 }))
}

/// Where paired phones may pick or create project folders.
fn project_roots(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let mut rows = vec![];
    for r in &s.project_roots {
        let on = s.sel_root.as_deref() == Some(r.as_str());
        let r2 = r.clone();
        let mut e = row()
            .key(format!("root-{r}"))
            .w_full()
            .gap(8.0)
            .px(12.0)
            .py(if mac { 8.0 } else { 12.0 })
            .center_y()
            .role("listitem")
            .label(r.clone())
            .focusable(true)
            .on_press(move |s: &mut App| s.sel_root = Some(r2.clone()))
            .child(icon(&ic::FOLDER, 15.0).color(if on && mac { Color::WHITE } else { rune::color::hex("#5fb0ff").unwrap_or(t.accent) }))
            .child(t.mono(tilde(r)).color(if on && mac { Color::WHITE } else { t.text }));
        if on {
            e = e.fill(if mac { t.accent } else { Color::WHITE.alpha(0.1) });
        }
        rows.push(e);
    }
    if rows.is_empty() {
        rows.push(form_row(t, "None", Some("Phones can't create projects.".into()), el()));
    }
    let add = |_: &mut App, tx: &mut Tx| {
        let options = rune::FileDialogOptions { title: "A folder your phone may create projects in".into(), ..Default::default() };
        tx.open_folder(options, |s: &mut App, result, _| match result {
            Ok(Some(p)) => {
                let mut roots = s.project_roots.clone();
                roots.push(p.display().to_string());
                s.set_project_roots(roots);
            }
            Ok(None) => {}
            Err(e) => s.error = Some(format!("Could not open the folder picker: {e}")),
        });
    };
    let remove: Option<Box<dyn Fn(&mut App)>> = s.sel_root.clone().filter(|r| s.project_roots.contains(r)).map(|r| {
        Box::new(move |s: &mut App| {
            let rest: Vec<String> = s.project_roots.iter().filter(|x| **x != r).cloned().collect();
            s.set_project_roots(rest);
            s.sel_root = None;
        }) as Box<dyn Fn(&mut App)>
    });
    let mut b = t.group(rows, 0.0);
    if mac {
        b = b.child(t.plus_minus("Add Project Folder", add, remove, ""));
    } else {
        let mut bar = row().w_full().gap(6.0).padding(8.0).border_sides([1.0, 0.0, 0.0, 0.0], Color::BLACK.alpha(0.25)).child(t.small_button_tx("Add Folder…", add));
        if let Some(f) = remove {
            bar = bar.child(t.small_button("Remove", move |s| f(s)));
        }
        b = b.child(bar);
    }
    section(t, if mac { "Project folders" } else { "Project Folders" }, Some("Where your iPhone may create projects".into()), b).key("project-roots")
}

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

/// A service's icon from its `data:` URL, clipped to the badge's rounded square.
fn icon_badge(url: Option<&str>, size: f32) -> Option<El> {
    let px = crate::brand::data_url_pixels(url?)?;
    let r = (size * 0.24).round();
    Some(el().size(size, size).radius(r).clip_rounded(r).pixels(&px))
}

fn conn_badge(t: &Theme, c: &Connector, size: f32) -> El {
    if let Some(b) = icon_badge(c.icon.as_deref(), size) {
        return b;
    }
    if c.name.to_lowercase().contains("github") || c.url.contains("github") {
        return row().size(size, size).radius((size * 0.24).round()).center().fill(rune::color::hex("#24292f").unwrap_or(Color::BLACK)).child(icon(&ic::GITHUB, size * 0.6).color(Color::WHITE));
    }
    let palette = ["#4b3a8c", "#0f766e", "#9a3412", "#1d4ed8", "#7e22ce"];
    let i = c.name.bytes().map(|b| b as usize).sum::<usize>() % palette.len();
    t.badge_letter(c.name.chars().next().unwrap_or('?').to_ascii_uppercase(), rune::color::hex(palette[i]).unwrap_or(Color::BLACK), Color::WHITE, size)
}

fn conn_status(s: &App, c: &Connector) -> (String, bool) {
    if s.signing_in.contains_key(&c.connector_id) {
        return ("Waiting for the browser…".into(), true);
    }
    let n = c.tools.len();
    match c.status.as_str() {
        "ok" if c.auth == ConnectorAuth::None => (format!("No sign-in · {n} tools"), false),
        "ok" => (format!("Signed in · {n} tools"), false),
        "needs_sign_in" => ("Sign-in needed".into(), true),
        "error" => ("Error".into(), true),
        _ => ("Checking…".into(), false),
    }
}

fn connectors(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let selected = s.sel_conn.clone().filter(|id| s.connectors.iter().any(|c| &c.connector_id == id)).or_else(|| s.connectors.first().map(|c| c.connector_id.clone()));
    let mut list = col().w_full().grow(1.0).padding(4.0).gap(2.0).role("list");
    for c in &s.connectors {
        let on = !s.adding_connector && selected.as_deref() == Some(c.connector_id.as_str());
        let (st, warn) = conn_status(s, c);
        let detail = text(st).font(t.font_small()).color(if warn { t.warn } else if on && mac { Color::WHITE.alpha(0.8) } else { t.text2 }).truncate();
        let id = c.connector_id.clone();
        list = list.child(source_row(t, format!("conn-{}", c.connector_id), on, conn_badge(t, c, 22.0), &c.name, detail, move |s| {
            if s.sel_conn.as_deref() != Some(id.as_str()) {
                s.renaming = None;
                s.rename_error = None;
            }
            s.sel_conn = Some(id.clone());
            s.close_add_connector();
        }));
    }
    if s.connectors.is_empty() {
        list = list.child(t.small("No connectors yet.").padding(8.0));
    }
    let remove: Option<Box<dyn Fn(&mut App)>> = selected.clone().filter(|_| !s.adding_connector).map(|id| {
        Box::new(move |s: &mut App| {
            s.act("Remove connector", m::CONNECTOR_REMOVE, json!({"connector_id": id}), Refresh::Connectors);
            s.sel_conn = None;
        }) as Box<dyn Fn(&mut App)>
    });
    let mut left = col().w_full().child(list);
    let start_add = |s: &mut App, _: &mut Tx| s.open_add_connector();
    if mac {
        left = left.child(t.plus_minus("Add a Connector", start_add, remove, ""));
    } else {
        let mut bar = row().w_full().gap(6.0).padding(8.0).child(t.small_button_tx("Add…", start_add));
        if let Some(f) = remove {
            bar = bar.child(t.small_button("Remove", move |s| f(s)));
        }
        left = left.child(bar);
    }
    let detail = if s.adding_connector || s.connectors.is_empty() {
        add_connector(s, t)
    } else {
        match selected.as_ref().and_then(|id| s.connectors.iter().find(|c| &c.connector_id == id)) {
            Some(c) => connector_detail(s, c, t),
            None => col(),
        }
    };
    let mut c = col().key("connectors").w_full().gap(18.0).child(split(t, left, detail));
    if !s.builtins.is_empty() {
        c = c.child(builtin_tools(s, t));
    }
    c
}

/// A built-in tool's policy, as its pop-up shows it (in the case the tab's other pop-ups use).
fn builtin_policy_name(_t: &Theme, p: ToolPolicy) -> &'static str {
    match p {
        ToolPolicy::Auto => "Don't Ask",
        _ => "Ask First",
    }
}

/// The host's own tools (opening pull requests, pushing, installing, running commands):
/// whether each asks before it runs. Installing always asks; so does a force-push, whatever
/// Push says; commands ask unless listed under Run commands (each with Ask First to undo).
fn builtin_tools(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let rows = s
        .builtins
        .iter()
        .flat_map(|b| {
            let mut rows = vec![builtin_row(t, b)];
            for rule in &b.allowed {
                let shown = format!("{} \u{2026}", rule.join(" "));
                let (r, key) = (rule.clone(), format!("allowed-{}", rule.join("-")));
                let undo = t.button("Ask First", Btn::Default, None, move |s| s.forget_command(&r)).key(key.clone()).label(format!("Ask first before {shown}"));
                let names = col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(1.0)
                    .child(pre(t, shown))
                    .child(t.small("Runs without asking"));
                rows.push(row().w_full().gap(10.0).pl(if mac { 28.0 } else { 30.0 }).pr(if mac { 12.0 } else { 14.0 }).py(if mac { 6.0 } else { 10.0 }).center_y().child(names).child(undo).key(format!("row-{key}")));
            }
            rows
        })
        .collect();
    section(
        t,
        if mac { "Built-in tools" } else { "Built-in Tools" },
        None,
        col().w_full().gap(if mac { 6.0 } else { 8.0 }).child(t.group(rows, if mac { 12.0 } else { 0.0 })).child(footnote(t, "AgentHost's own tools run on this computer as you, outside the sandbox, with your git and GitHub CLI logins. Shells, interpreters and build tools always ask.")),
    )
    .key("builtin-tools")
}

/// One built-in tool: its title, what it does, and Ask First / Don't Ask (or Always Asks).
fn builtin_row(t: &Theme, b: &BuiltinTool) -> El {
    let trailing = if b.can_auto {
        let key = format!("builtin-{}", b.name);
        let (k2, name, current) = (key.clone(), b.name.clone(), b.policy);
        let label = builtin_policy_name(t, b.policy);
        let names = [builtin_policy_name(t, ToolPolicy::Ask), builtin_policy_name(t, ToolPolicy::Auto)];
        popup_button(t, key, label).label(format!("{}: {label}", b.title)).on_press_tx(move |s: &mut App, tx: &mut Tx| {
            if rune::popup_menu_supported() {
                let menu = [ToolPolicy::Ask, ToolPolicy::Auto]
                    .into_iter()
                    .zip(names)
                    .fold(rune::PopupMenu::new(), |m, (p, n)| m.item(rune::PopupItem::new(policy_id(p), n).checked(p == current)));
                let name = name.clone();
                tx.popup_menu(menu, k2.clone(), move |s: &mut App, picked, _| {
                    if let Some(p) = picked.and_then(policy_of).filter(|p| *p != current) {
                        s.set_builtin_policy(&name, p);
                    }
                });
            } else {
                s.set_builtin_policy(&name, if current == ToolPolicy::Auto { ToolPolicy::Ask } else { ToolPolicy::Auto });
            }
        })
    } else {
        t.small("Always Asks").key(format!("builtin-{}", b.name))
    };
    form_row(t, &b.title, Some(b.description.clone()), trailing).key(format!("builtin-row-{}", b.name))
}

fn add_connector(s: &App, t: &Theme) -> El {
    if s.add_manual {
        return add_by_url(s, t);
    }
    let mac = t.mac();
    let input = rune::text_input(&s.conn_search, TextStyle::new(t.fs, 400.0).lh((t.fs * 1.3).round()))
        .single_line()
        .key("conn-search-field")
        .label("Search MCP servers")
        .grow(1.0)
        .min_w(0.0)
        .color(t.text)
        .placeholder("Linear, Sentry, Notion…", t.text3)
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.conn_search)
        .on_input_change(|s: &mut App, tx: &mut Tx| {
            s.search_edited(std::time::Instant::now());
            tx.wake_after(crate::model::SEARCH_DEBOUNCE);
        })
        .on_submit(|s: &mut App, tx: &mut Tx| {
            // Return searches now.
            if !s.conn_search.text.trim().is_empty() {
                s.search_due = Some(std::time::Instant::now());
                tx.wake_after(std::time::Duration::ZERO);
            }
        });
    let mut field = row()
        .key("conn-search")
        .w_full()
        .h(if mac { 26.0 } else { 36.0 })
        .px(if mac { 8.0 } else { 10.0 })
        .gap(if mac { 6.0 } else { 8.0 })
        .center_y()
        .radius(if mac { 7.0 } else { 7.0 })
        .fill(t.field)
        .child(icon(&ic::SEARCH, if mac { 12.0 } else { 14.0 }).color(t.text2))
        .child(input);
    if mac {
        field = field.border(0.5, t.field_border);
    }
    if !s.conn_search.text.is_empty() {
        field = field.child(row().role("button").label("Clear search").on_press(|s: &mut App| {
            s.conn_search.replace_text("");
            s.search_edited(std::time::Instant::now());
        }).child(icon(&ic::XMARK_CIRCLE, 13.0).color(t.text3)));
    }
    if s.conn_search.focused {
        field = field.box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() });
    }

    let q = s.conn_search.text.trim();
    let mut results: Vec<El> = vec![];
    let status: Option<El> = if q.is_empty() {
        Some(footnote(t, "Search the integrations.sh catalog of remote MCP servers."))
    } else if s.searching || s.search_due.is_some() || s.search_query != q {
        Some(row().key("conn-searching").gap(6.0).center_y().child(icon(&ic::SPINNER, 12.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9))).child(t.small("Searching…")))
    } else if let Some(e) = &s.search_error {
        Some(text(format!("Couldn't search the catalog: {e}")).font(t.font_small()).color(t.danger).wrap())
    } else if s.suggestions.is_empty() {
        Some(t.small(format!("No servers match \u{201c}{q}\u{201d}. Enter its URL instead.")).wrap())
    } else {
        None
    };
    for (i, sug) in s.suggestions.iter().enumerate().filter(|_| status.is_none()) {
        results.push(suggestion_row(t, i, sug));
    }
    let manual = text("Enter a URL Instead…")
        .key("conn-manual")
        .font(TextStyle::new(if mac { 12.0 } else { 14.0 }, 400.0))
        .color(t.accent_text)
        .role("button")
        .label("Enter a URL instead")
        .focusable(true)
        .on_press(|s: &mut App| {
            s.add_manual = true;
            // Carry over what was typed when it looks like an address.
            let q = s.conn_search.text.trim().to_string();
            if q.contains('.') && !q.contains(' ') {
                s.conn_url.replace_text(&q);
            }
            s.conn_url.focused = true;
        });
    let mut c = col().key("add-connector").w_full().gap(8.0).child(text("Add a Connector").font(TextStyle::new(15.0, t.w_bold)).color(t.text)).child(field);
    if let Some(st) = status {
        c = c.child(st.pt(2.0));
    }
    if !results.is_empty() {
        c = c.child(t.group(results, if mac { 46.0 } else { 0.0 }).key("conn-results"));
    }
    c.child(footnote(t, "Servers that need an account open your browser to sign in. New connectors are on in every workspace. For two accounts on one service, add it twice and rename each (work, personal)."))
        .child(row().w_full().gap(8.0).center_y().child(manual).child(el().grow(1.0)).child(t.button("Cancel", Btn::Default, None, |s| s.close_add_connector())))
}

/// One catalog result: icon, name and domain, one line of description. Ones that need an API
/// key are shown but can't be picked yet.
fn suggestion_row(t: &Theme, i: usize, sug: &ConnectorSuggestion) -> El {
    let mac = t.mac();
    let ok = crate::model::suggestion_supported(sug);
    let size = if mac { 28.0 } else { 32.0 };
    let badge = icon_badge(sug.icon.as_deref(), size).unwrap_or_else(|| {
        let palette = ["#4b3a8c", "#0f766e", "#9a3412", "#1d4ed8", "#7e22ce"];
        let n = sug.name.bytes().map(|b| b as usize).sum::<usize>() % palette.len();
        t.badge_letter(sug.name.chars().next().unwrap_or('?').to_ascii_uppercase(), rune::color::hex(palette[n]).unwrap_or(Color::BLACK), Color::WHITE, size)
    });
    let detail = if ok { sug.description.clone().unwrap_or_else(|| sug.url.clone()) } else { "Needs an API key — not supported yet".to_string() };
    let names = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(1.0)
        .child(row().gap(6.0).center_y().min_w(0.0).child(t.strong(sug.name.clone()).truncate().shrink()).child(t.faint(sug.domain.clone()).truncate().shrink()))
        .child(text(detail).font(t.font_small()).color(if ok { t.text2 } else { t.text3 }).truncate());
    let mut r = row()
        .key(format!("sug-{i}"))
        .w_full()
        .gap(if mac { 10.0 } else { 12.0 })
        .px(if mac { 10.0 } else { 14.0 })
        .py(if mac { 7.0 } else { 10.0 })
        .center_y()
        .role("button")
        .label(format!("Add {}", sug.name))
        .help(sug.url.clone())
        .child(badge)
        .child(names);
    if ok {
        let sg = sug.clone();
        r = r.focusable(true).hover(Color::WHITE.alpha(0.04)).on_press(move |s: &mut App| s.add_suggestion(&sg)).child(t.small_button("Add", {
            let sg = sug.clone();
            move |s| s.add_suggestion(&sg)
        }));
    } else {
        r = r.disabled(true).opacity(0.55);
    }
    r
}

/// Add by address: the URL and an optional name. How it signs in is the host's to find out.
fn add_by_url(s: &App, t: &Theme) -> El {
    let w = if t.mac() { 300.0 } else { 320.0 };
    let rows = vec![
        form_row(t, "Server URL", Some("A remote MCP server".into()), row().w(w).child(t.field_submit(&s.conn_url, "https://mcp.sentry.dev/mcp", |s| &mut s.conn_url, |s: &mut App, _: &mut Tx| s.add_connector()))),
        form_row(t, "Name", Some("Optional; agents see it in tool names".into()), row().w(w).child(t.field_submit(&s.conn_name, "e.g. work", |s| &mut s.conn_name, |s: &mut App, _: &mut Tx| s.add_connector()))),
    ];
    let back = text("Search Instead")
        .font(TextStyle::new(if t.mac() { 12.0 } else { 14.0 }, 400.0))
        .color(t.accent_text)
        .role("button")
        .label("Search the catalog instead")
        .focusable(true)
        .on_press(|s: &mut App| {
            s.add_manual = false;
            s.conn_search.focused = true;
        });
    col()
        .key("add-connector")
        .w_full()
        .gap(8.0)
        .child(text("Add a Connector").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
        .child(t.group(rows, 12.0))
        .child(footnote(t, "If the server needs an account, your browser opens so you can sign in."))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(back)
                .child(el().grow(1.0))
                .child(t.button("Cancel", Btn::Default, None, |s| s.close_add_connector()))
                .child(t.button("Add", Btn::Primary, None, |s| s.add_connector()).key("add-connector-button").default_button()),
        )
}

/// Renaming a connector in place: Return renames, Escape cancels, the host's refusal shows
/// under it.
fn rename_field(s: &App, c: &Connector, t: &Theme) -> El {
    let mac = t.mac();
    let id = c.connector_id.clone();
    let idk = id.clone();
    let input = rune::text_input(&s.rename, TextStyle::new(t.fs, 400.0).lh((t.fs * 1.3).round()))
        .single_line()
        .key("conn-rename-field")
        .label(format!("Name of {}", c.name))
        .grow(1.0)
        .min_w(0.0)
        .color(t.text)
        .placeholder("e.g. work", t.text3)
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.rename)
        .on_submit(move |s: &mut App, _: &mut Tx| s.rename_connector(&idk))
        .on_input_key(|s: &mut App, ev: &rune::KeyEvent, _: &mut Tx| {
            if ev.key == rune::Key::Escape {
                s.renaming = None;
                s.rename_error = None;
                return true;
            }
            false
        });
    let failed = s.rename_error.as_ref().is_some_and(|(cid, _)| *cid == c.connector_id);
    let mut f = row().h(if mac { 24.0 } else { 34.0 }).w(if mac { 220.0 } else { 240.0 }).px(if mac { 7.0 } else { 10.0 }).center_y().radius(if mac { 5.0 } else { 7.0 }).fill(t.field).child(input);
    if mac {
        f = f.border(0.5, if failed { t.danger } else { t.field_border });
    }
    f = f.box_shadow(rune::Shadow { spread: 3.0, color: if failed { t.danger.alpha(0.4) } else { t.focus }, ..Default::default() });
    let idr = id.clone();
    row()
        .key("conn-renaming")
        .gap(8.0)
        .center_y()
        .child(conn_badge(t, c, if mac { 26.0 } else { 30.0 }))
        .child(f)
        .child(t.small_button("Cancel", |s| {
            s.renaming = None;
            s.rename_error = None;
        }))
        .child(t.button("Rename", Btn::Primary, None, move |s| s.rename_connector(&idr)).key("conn-rename-save").h(if mac { 22.0 } else { 34.0 }))
}

fn connector_detail(s: &App, c: &Connector, t: &Theme) -> El {
    let mac = t.mac();
    let id = c.connector_id.clone();
    let signing = s.signing_in.get(&c.connector_id);
    let renaming = s.renaming.as_deref() == Some(c.connector_id.as_str());
    let title: El = if renaming {
        rename_field(s, c, t)
    } else {
        let idr = id.clone();
        let idb = id.clone();
        row()
            .gap(10.0)
            .center_y()
            .min_w(0.0)
            .child(conn_badge(t, c, if mac { 26.0 } else { 30.0 }))
            .child(
                text(c.name.clone())
                    .key("conn-name")
                    .font(TextStyle::new(15.0, t.w_bold))
                    .color(t.text)
                    .truncate()
                    .shrink()
                    .role("button")
                    .label(format!("Rename {}", c.name))
                    .help("Double-click to rename")
                    .on_double_press(move |s: &mut App| s.start_rename(&idr)),
            )
            .child(t.small_button("Rename…", move |s| s.start_rename(&idb)).key("conn-rename"))
    };
    let mut actions = row().gap(8.0).center_y();
    match signing {
        Some(link) if !link.is_empty() => {
            let l = link.clone();
            actions = actions.child(t.small_button("Open Browser Again", move |s| s.open_queue.push(l.clone())));
        }
        Some(_) => {}
        None if c.status == "needs_sign_in" || (c.status == "error" && c.auth == ConnectorAuth::Oauth) => {
            let ids = id.clone();
            actions = actions.child(t.button("Sign In…", Btn::Primary, None, move |s| s.sign_in(&ids)).key("sign-in"));
        }
        None => {}
    }
    let idf = id.clone();
    actions = actions.child(t.small_button("Refresh Tools", move |s| s.act("Refresh", m::CONNECTOR_REFRESH, json!({"connector_id": idf}), Refresh::Connectors)));
    let ide = id.clone();
    let enabled = c.enabled;
    actions = actions.child(
        t.switch(enabled, "On", move |s: &mut App| s.act("Turn connector on or off", m::CONNECTOR_UPDATE, json!({"connector_id": ide, "enabled": !enabled}), Refresh::Connectors))
            .key(format!("{}-enabled", c.connector_id)),
    );
    let mut d = col()
        .w_full()
        .gap(if mac { 6.0 } else { 10.0 })
        .child(row().w_full().gap(10.0).items_start().child(col().grow(1.0).min_w(0.0).gap(4.0).child(title).child(t.mono(clip(c.url.trim_start_matches("https://"), 60)).color(t.text2))).child(if renaming { el() } else { actions.flex_wrap().max_w(240.0).justify_end() }));
    if let Some(link) = signing.filter(|l| !l.is_empty()) {
        d = d.child(footnote(t, "Finish signing in in your browser. This updates by itself when you're done. If no browser opened, open this link:")).child(t.mono_small(link.clone()));
    } else if c.status == "error" {
        if let Some(det) = &c.status_detail {
            d = d.child(text(det.clone()).font(t.font_small()).color(t.danger).wrap());
        }
    }
    if renaming {
        if let Some((_, why)) = s.rename_error.as_ref().filter(|(cid, _)| *cid == c.connector_id) {
            d = d.child(text(why.clone()).key("rename-error").font(t.font_small()).color(t.danger).wrap());
        }
        let new = s.rename.text.trim();
        let slug = connector_slug(if new.is_empty() { &c.name } else { new });
        d = d.child(t.faint(format!("Sessions started after the rename see its tools as {slug}__tool. Running sessions keep {}__tool.", connector_slug(&c.name))).wrap());
    } else {
        d = d.child(t.faint(format!("Agents see its tools as {}__tool. A new name applies to sessions started afterwards.", connector_slug(&c.name))).wrap());
    }
    // Defaults.
    let default_row = |label: &str, which: &'static str, p: ToolPolicy| {
        let key = format!("pol-{}-{which}", c.connector_id);
        let (k2, cid) = (key.clone(), c.connector_id.clone());
        form_row(t, label, None, popup_button(t, key, policy_name(p)).on_press_tx(move |s: &mut App, tx: &mut Tx| pick_policy(s, tx, k2.clone(), cid.clone(), which.into(), p, false)))
    };
    d = d.child(el().h(8.0)).child(t.group_title("By default")).child(t.group(
        vec![default_row("Tools that read", "read", c.read_policy), default_row("Tools that change things", "write", c.write_policy)],
        if mac { 12.0 } else { 0.0 },
    ));
    if c.tools.is_empty() {
        return d;
    }
    let changed = c.tools.iter().filter(|x| x.policy_override.is_some()).count();
    // The tool list can be long: collapsed until asked for.
    let tools_key = format!("tools-{}", c.connector_id);
    let open = s.expanded.contains(&tools_key);
    let k2 = tools_key.clone();
    let summary = if changed > 0 {
        format!("{} tools · {changed} changed from the default", c.tools.len())
    } else {
        format!("{} tools", c.tools.len())
    };
    d = d.child(el().h(8.0)).child(
        row()
            .key(tools_key)
            .w_full()
            .gap(6.0)
            .center_y()
            .role("button")
            .label(if open { "Hide tools" } else { "Show tools" })
            .focusable(true)
            .on_press(move |s: &mut App| {
                if !s.expanded.remove(&k2) {
                    s.expanded.insert(k2.clone());
                }
            })
            .child(icon(if open { &ic::CHEVRON_D } else { &ic::CHEVRON_R }, 10.0).color(t.text2))
            .child(t.group_title("Tools"))
            .child(el().grow(1.0))
            .child(t.faint(summary)),
    );
    if !open {
        return d;
    }
    let head_cell = |s: &str, w: Option<f32>| {
        let e = text(s.to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 400.0)).color(t.text2);
        match w {
            Some(w) => row().w(w).child(e),
            None => row().grow(1.0).child(e),
        }
    };
    let mut table = col()
        .w_full()
        .radius(t.box_radius)
        .fill(t.boxed)
        .clip_rounded(t.box_radius)
        .child(row().w_full().px(12.0).py(8.0).border_sides([0.0, 0.0, 0.5, 0.0], t.sep).child(head_cell("Tool", Some(180.0))).child(head_cell("Kind", Some(120.0))).child(head_cell("Policy", None)));
    if mac {
        table = table.border(0.5, t.sep);
    }
    for (i, tool) in c.tools.iter().enumerate() {
        let key = format!("pol-{}-{}", c.connector_id, tool.name);
        let (k2, cid, name, p) = (key.clone(), c.connector_id.clone(), tool.name.clone(), tool.policy);
        let color = match tool.policy {
            ToolPolicy::Auto => t.text,
            ToolPolicy::Ask => t.warn,
            ToolPolicy::Disabled => t.danger,
        };
        let mut pol = row()
            .key(key)
            .gap(6.0)
            .center_y()
            .role("combobox")
            .label(format!("{} policy: {}", tool.name, policy_short(tool.policy)))
            .focusable(true)
            .on_press_tx(move |s: &mut App, tx: &mut Tx| pick_policy(s, tx, k2.clone(), cid.clone(), name.clone(), p, true))
            .child(text(policy_short(tool.policy)).font(t.font()).color(color));
        if tool.policy_override.is_some() {
            pol = pol.child(row().gap(4.0).center_y().child(t.dot(t.accent, 6.0)).child(text("changed").font(TextStyle::new(11.0, 400.0)).color(t.accent_text)));
        }
        let mut r = row()
            .key(format!("tool-{}", tool.name))
            .w_full()
            .px(12.0)
            .py(if mac { 7.0 } else { 9.0 })
            .center_y()
            .help(tool.description.clone().unwrap_or_default())
            .child(row().w(180.0).child(text(tool.name.clone()).font(TextStyle::new(t.fs_mono, 400.0).mono()).color(t.text).truncate()))
            .child(row().w(120.0).child(t.small(if tool.kind == ToolKind::Read { "Reads" } else { "Changes" }).font(t.font())))
            .child(row().grow(1.0).child(pol));
        if i % 2 == 1 {
            r = r.fill(Color::WHITE.alpha(0.03));
        }
        table = table.child(r);
    }
    d.child(table)
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

fn devices(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let mut rows = vec![];
    for d in &s.devices {
        let id = d.device_id.clone();
        let renaming = s.renaming.as_deref() == Some(d.device_id.as_str());
        let seen = d.last_seen_at.map(|ts| {
            let a = short_age(ts, s.look);
            if a == "now" { "Online now".to_string() } else { format!("Last seen {a} ago") }
        });
        let name: El = if renaming {
            let idn = id.clone();
            row().w(260.0).child(t.field_submit(&s.rename, "Name", |s| &mut s.rename, move |s: &mut App, _: &mut Tx| {
                let n = s.rename.text.trim().to_string();
                if !n.is_empty() {
                    s.act("Rename device", m::DEVICE_RENAME, json!({"device_id": idn, "name": n}), Refresh::Devices);
                }
                s.renaming = None;
            }))
        } else {
            col().gap(1.0).child(t.label(d.name.clone())).child(t.small(seen.unwrap_or_else(|| "Never seen".into())))
        };
        let (idr, idv, nm) = (id.clone(), id.clone(), d.name.clone());
        rows.push(
            row()
                .key(format!("dev-{}", d.device_id))
                .w_full()
                .gap(10.0)
                .px(12.0)
                .py(10.0)
                .center_y()
                .child(icon(&ic::PHONE, 22.0).color(t.text2))
                .child(name)
                .child(el().grow(1.0))
                .child(t.small_button("Rename…", move |s| {
                    s.renaming = Some(idr.clone());
                    s.rename = rune::TextEdit::new(nm.clone());
                    s.rename.focused = true;
                }))
                .child(t.small_button_danger("Revoke…", move |s| s.act("Revoke device", m::DEVICE_REVOKE, json!({"device_id": idv}), Refresh::Devices)).key(format!("revoke-{}", d.device_id))),
        );
    }
    if rows.is_empty() {
        rows.push(form_row(t, "No paired devices", Some("Pair your iPhone to start sessions and answer approvals from it.".into()), el()));
    }
    let pair = row().w_full().pt(6.0).child(el().grow(1.0)).child(t.small_button("Pair iPhone…", |s| s.call(Tag::PairStart, m::DEVICE_PAIR_START, json!({}))).key("pair"));
    let r = s.info.as_ref().map(|i| i.relay.clone()).unwrap_or_default();
    let relay = vec![
        form_row(
            t,
            "Status",
            r.error.clone(),
            if !r.configured {
                t.small("Off")
            } else {
                t.status(if r.connected { t.ok } else { t.warn }, if r.connected { "Connected" } else { "Disconnected" }, false)
            },
        ),
        form_row(t, "Address", None, t.mono(r.url.clone().unwrap_or_else(|| "Not configured".into())).font(TextStyle::new(t.fs_mono, 400.0).mono()).color(t.text2)),
    ];
    let here = if cfg!(target_os = "macos") && mac { "this Mac" } else { "this computer" };
    col()
        .key("devices")
        .w_full()
        .gap(18.0)
        .child(col().w_full().gap(0.0).child(section(t, if mac { "Paired devices" } else { "Paired Devices" }, Some(format!("Phones reach {here} through your relay")), t.group(rows, 44.0))).child(pair))
        .child(section(t, "Relay", None, t.group(relay, if mac { 12.0 } else { 0.0 })))
        .child(screen_sharing(s, t))
}

/// Whether paired phones may see this computer's screen (through screen-relay),
/// and click and type on it. Only this app can turn it on.
fn screen_sharing(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let title = if mac { "Screen sharing" } else { "Screen Sharing" };
    let Some(st) = &s.screen_share else {
        return section(t, title, None, t.group(vec![form_row(t, "Not available", Some("Update agenthostd to share this screen with your phone.".into()), el())], 0.0));
    };
    let p = st.policy;
    let mut rows = vec![
        form_row(
            t,
            "Let phones see this screen",
            Some("For a sign-in page or an approval in a browser while you're away".into()),
            t.switch(p.enabled, "Let phones see this screen", move |s: &mut App| {
                s.call(Tag::Screen, m::SCREEN_SET_POLICY, json!({"enabled": !p.enabled, "allow_control": p.allow_control && !p.enabled}))
            }),
        ),
        form_row(
            t,
            "Let phones click and type",
            Some(if p.enabled { "Full control of everything you're logged in to".into() } else { "Turn on screen sharing first".into() }),
            if p.enabled {
                t.switch(p.allow_control, "Let phones click and type", move |s: &mut App| {
                    s.call(Tag::Screen, m::SCREEN_SET_POLICY, json!({"enabled": true, "allow_control": !p.allow_control}))
                })
            } else {
                t.small("Off")
            },
        ),
    ];
    for mode in &st.modes {
        let name = match mode.mode {
            ScreenMode::Desktop => "Your desktop",
            ScreenMode::Browser => "Browser on this computer",
        };
        rows.push(form_row(t, name, Some(mode.detail.clone()), t.status(if mode.available { t.ok } else { t.warn }, if mode.available { "Available" } else { "Unavailable" }, false)));
    }
    if let Some(r) = &st.running {
        let what = if r.control { "Controlled" } else { "Watched" };
        let mut who = if r.viewers > 0 { format!("{what} by {}", r.started_by) } else { format!("Started by {}, waiting for the phone", r.started_by) };
        if let Some(o) = &r.output {
            who.push_str(&format!(" · {o}"));
        }
        rows.push(form_row(t, "Sharing now", Some(who), t.small_button_danger("Stop", |s| s.call(Tag::Screen, m::SCREEN_STOP, json!({}))).key("screen-stop")));
    }
    if let Some(e) = &st.error {
        rows.push(form_row(t, "Last error", Some(e.clone()), el()));
    }
    section(t, title, Some("Through screen-relay, started only while a phone watches".into()), t.group(rows, if mac { 12.0 } else { 0.0 }))
}

fn qr(payload: &str, t: &Theme) -> El {
    let Ok(code) = qrcode::QrCode::new(payload.as_bytes()) else { return t.mono(payload.to_string()) };
    let w = code.width();
    let colors = code.to_colors();
    let module = (150.0 / w as f32).floor().max(2.0);
    let mut grid = col().key("qr");
    for y in 0..w {
        let mut r = row();
        for x in 0..w {
            let dark = colors[y * w + x] == qrcode::Color::Dark;
            r = r.child(el().size(module, module).fill(if dark { Color::BLACK } else { Color::WHITE }));
        }
        grid = grid.child(r);
    }
    col().padding(14.0).radius(12.0).fill(Color::WHITE).child(grid)
}

fn pairing_sheet(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let Some(p) = &s.pairing else { return el() };
    let mut c = col().w_full().items_center().gap(12.0);
    let title = text("Pair an iPhone").font(TextStyle::new(15.0, t.w_bold)).color(t.text);
    let center = |msg: &str| text(msg.to_string()).font(TextStyle::new(if mac { 12.0 } else { 14.0 }, 400.0).lh(if mac { 18.0 } else { 21.0 })).color(t.text2).wrap().align(rune::Align::Center).max_w(330.0);
    let buttons;
    match (&p.pending, &p.done) {
        (_, Some(done)) => {
            c = c.child(title).child(t.label(done.clone()));
            buttons = row().w_full().gap(8.0).child(el().grow(1.0)).child(t.button("Done", Btn::Primary, None, |s| s.pairing = None).default_button());
        }
        (pend, None) => {
            c = c.child(title).child(center("In AgentHost on your iPhone, tap Pair and scan this code. It works once and expires in 2 minutes.")).child(qr(&p.qr, t));
            match pend {
                Some(pend) => {
                    let mut code = col()
                        .key("confirm")
                        .w_full()
                        .gap(6.0)
                        .items_center()
                        .padding(14.0)
                        .radius(10.0)
                        .fill(Color::WHITE.alpha(0.05))
                        .child(center(&format!("\u{201c}{}\u{201d} wants to pair. Check it shows the same code:", pend.device_name)))
                        .child(text(pend.short_code.clone()).font(TextStyle::new(26.0, 600.0).mono().tracking(2.0)).color(t.text).key("short-code"));
                    if mac {
                        code = code.border(0.5, Color::WHITE.alpha(0.14));
                    }
                    c = c.child(code);
                    buttons = row()
                        .w_full()
                        .gap(8.0)
                        .child(el().grow(1.0))
                        .child(t.button("Refuse", Btn::Default, None, |s| s.confirm_pairing(false)).key("refuse"))
                        .child(
                            t.button("Codes Match, Pair", Btn::Primary, Some(if mac { "↩" } else { "Enter" }), |s| s.confirm_pairing(true))
                                .key("codes-match")
                                .default_button(),
                        );
                }
                None => {
                    c = c.child(t.faint("Waiting for your iPhone…"));
                    buttons = row().w_full().gap(8.0).child(el().grow(1.0)).child(t.button("Cancel", Btn::Default, None, |s| s.pairing = None).key("cancel-pair"));
                }
            }
        }
    }
    let mut sheet = col()
        .key("pairing")
        .w(420.0)
        .gap(18.0)
        .px(24.0)
        .pt(24.0)
        .pb(20.0)
        .fill(t.sheet)
        .shadow(24.0, 60.0, Color::BLACK.alpha(0.55))
        .modal(true, |s: &mut App| s.pairing = None)
        .child(c)
        .child(buttons);
    sheet = if mac { sheet.corners([0.0, 0.0, 12.0, 12.0]).border_sides([0.0, 0.5, 0.5, 0.5], Color::WHITE.alpha(0.14)) } else { sheet.radius(14.0).border(1.0, Color::BLACK.alpha(0.6)) };
    sheet
}

// ---------------------------------------------------------------------------
// A workspace's default login
// ---------------------------------------------------------------------------

const NO_DEFAULT: &str = "None — set by the next session";

fn default_login_row(s: &App, w: &Workspace, t: &Theme) -> El {
    let current = s.default_login(&w.workspace_id);
    let agents = s.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
    let label = current.as_ref().and_then(|k| agents.iter().find(|a| &login_key(a) == k)).map(|a| a.label()).unwrap_or_else(|| NO_DEFAULT.to_string());
    let key = format!("default-login-{}", w.workspace_id);
    let (k2, ws) = (key.clone(), w.workspace_id.clone());
    let picker = popup_button(t, key, &label).on_press_tx(move |s: &mut App, tx: &mut Tx| pick_default_login(s, tx, k2.clone(), ws.clone()));
    form_row(t, "Default agent", Some("New sessions here start with this login".into()), picker)
}

/// The default-login picker: the logins that work, and None. Natively on macOS; elsewhere each
/// press steps to the next.
fn pick_default_login(s: &mut App, tx: &mut Tx, key: String, ws: String) {
    let agents: Vec<AgentStatus> = s.info.as_ref().map(|i| i.agents.iter().filter(|a| a.available).cloned().collect()).unwrap_or_default();
    let current = s.default_login(&ws);
    if rune::popup_menu_supported() {
        let mut menu = rune::PopupMenu::new();
        for a in &agents {
            let k = login_key(a);
            menu = menu.item(rune::PopupItem::new(k.clone(), a.label()).checked(current.as_deref() == Some(k.as_str())));
        }
        menu = menu.separator().item(rune::PopupItem::new("none", NO_DEFAULT).checked(current.is_none()));
        tx.popup_menu(menu, key, move |s: &mut App, picked, _| match picked {
            Some("none") => s.set_default_login(&ws, None),
            Some(k) => s.set_default_login(&ws, Some(k)),
            None => {}
        });
    } else {
        let keys: Vec<Option<String>> = agents.iter().map(|a| Some(login_key(a))).chain([None]).collect();
        let i = keys.iter().position(|k| *k == current).map(|i| (i + 1) % keys.len()).unwrap_or(0);
        s.set_default_login(&ws, keys[i].as_deref());
    }
}

// ---------------------------------------------------------------------------
// Archived
// ---------------------------------------------------------------------------

fn archived(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let q = s.archive_search.text.trim().to_lowercase();
    let mut rows = vec![];
    for x in &s.archived {
        let title = x.title.clone().unwrap_or_else(|| s.login_label(x));
        let ws = s.workspace(&x.workspace_id).map(|w| if w.kind == WorkspaceKind::Scratch { "Quick tasks".to_string() } else { w.name.clone() }).unwrap_or_default();
        let login = s.login_label(x);
        if !q.is_empty() && ![&title, &ws, &login].iter().any(|f| f.to_lowercase().contains(&q)) {
            continue;
        }
        let when = x.archived_at.map(|ts| {
            let a = short_age(ts, s.look);
            if a == "now" { "Archived just now".to_string() } else { format!("Archived {a} ago") }
        });
        let (open, unarchive) = (x.session_id.clone(), x.session_id.clone());
        rows.push(
            row()
                .key(format!("archived-{}", x.session_id))
                .w_full()
                .gap(10.0)
                .px(12.0)
                .py(if mac { 8.0 } else { 12.0 })
                .center_y()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(1.0)
                        .child(t.label(title).truncate())
                        .child(t.small(format!("{login} · {ws}{}", when.map(|w| format!(" · {w}")).unwrap_or_default())).truncate()),
                )
                .child(t.small_button("Open", move |s| {
                    s.close_overlays();
                    s.open_session(&open);
                }))
                .child(t.small_button("Unarchive", move |s| s.unarchive(&unarchive)).key(format!("unarchive-{}", x.session_id))),
        );
    }
    if rows.is_empty() {
        let msg = if q.is_empty() { "No archived sessions" } else { "No archived sessions match" };
        rows.push(form_row(t, msg, Some(format!("Archive a session from its menu or with {}.", crate::commands::label(s.look, crate::commands::Cmd::Archive))), el()));
    }
    let search = row().w(if mac { 220.0 } else { 260.0 }).child(t.field(&s.archive_search, "Search archived", |s| &mut s.archive_search));
    col()
        .key("archived")
        .w_full()
        .gap(if mac { 6.0 } else { 10.0 })
        .child(row().w_full().gap(8.0).center_y().child(t.section_title(if mac { "Archived sessions" } else { "Archived Sessions" })).child(el().grow(1.0)).child(search))
        .child(t.group(rows, if mac { 12.0 } else { 0.0 }))
        .child(footnote(t, "Archived sessions keep their transcript; open one to read it. They can't be deleted: the event log is append-only."))
}
