//! Automations: saved sessions that start on a schedule, from a webhook, or by hand. The
//! screen lists them beside the chosen one (its webhook, prompt and runs); the editor is a
//! sheet (macOS) or dialog (GNOME) like New Session. Each run is an ordinary session, opened
//! from its row.

use crate::icons as ic;
use crate::look::{agent_badge, Btn, Theme};
use crate::model::{auto_approve_title, login_key, outcome_word, run_trigger_word, span_label, trigger_line, App, Menu, Preset, TriggerKind, WEEKDAYS};
use crate::settings::{footnote, form_row, popup_button, section};
use crate::sheet::{scrim, tilde};
use crate::view::{clip, menu_box, now_ms, when_label, Item};
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, Icon, ScrollSpec, TextEdit, TextStyle, Tx};

/// The toolbar's subtitle on this screen.
pub fn summary(s: &App) -> String {
    let n = s.automations.len();
    let running = s.automations_running();
    match (n, running) {
        (0, _) => "No automations".into(),
        (n, 0) => format!("{n} automation{}", if n == 1 { "" } else { "s" }),
        (n, r) => format!("{n} automation{} · {r} running", if n == 1 { "" } else { "s" }),
    }
}

fn trigger_icon(t: &AutomationTrigger) -> &'static Icon {
    match t {
        AutomationTrigger::Schedule { .. } => &ic::CLOCK,
        AutomationTrigger::Webhook { .. } => &ic::HOOK,
        AutomationTrigger::Manual => &ic::PLAY,
    }
}

fn outcome_color(t: &Theme, o: RunOutcome) -> Color {
    match o {
        RunOutcome::Running => t.text2,
        RunOutcome::Succeeded => t.ok,
        RunOutcome::Failed | RunOutcome::TimedOut => t.danger,
        RunOutcome::Cancelled | RunOutcome::Skipped => t.text3,
    }
}

fn outcome_glyph(t: &Theme, o: RunOutcome) -> El {
    let e = if o == RunOutcome::Running { icon(&ic::SPINNER, 13.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)) } else { t.dot(outcome_color(t, o), if t.mac() { 8.0 } else { 9.0 }) };
    row().size(14.0, 14.0).center().child(e)
}

/// "Next in 5h · Succeeded 19h ago", or "Running now", or "Off".
fn status_line(s: &App, a: &Automation, now: i64) -> String {
    let mut parts = vec![];
    if !a.enabled {
        parts.push("Off".to_string());
    }
    match &a.last_run {
        Some(r) if r.outcome == RunOutcome::Running => parts.push("Running now".into()),
        _ => {
            if let Some(next) = a.next_run_at.filter(|_| a.enabled) {
                parts.push(format!("Next {}", when_label(next, now, s.look)));
            }
            if let Some(r) = &a.last_run {
                parts.push(format!("{} {}", outcome_word(r.outcome), when_label(r.started_at, now, s.look)));
            }
        }
    }
    if parts.is_empty() {
        parts.push(if matches!(a.trigger, AutomationTrigger::Manual) { "Never run".into() } else { "No runs yet".into() });
    }
    parts.join(" · ")
}

/// Where and as whom it runs: "relay · Claude Code · Work".
fn place_line(s: &App, a: &Automation) -> String {
    let ws = s.workspace(&a.workspace_id).map(|w| if w.kind == WorkspaceKind::Scratch { "Quick tasks".to_string() } else { w.name.clone() }).unwrap_or_else(|| "A removed workspace".into());
    format!("{ws} · {}", login_name(s, a.agent, a.profile_id.as_deref()))
}

fn login_name(s: &App, agent: AgentKind, profile: Option<&str>) -> String {
    let key = profile.map(str::to_string).unwrap_or_else(|| agent.as_str().to_string());
    let agents = s.info.as_ref().map(|i| i.agents.as_slice()).unwrap_or_default();
    agents.iter().find(|a| login_key(a) == key).map(|a| a.label()).unwrap_or_else(|| agent.display_name().to_string())
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

pub fn screen(s: &App, t: &Theme) -> El {
    let base = col().key("automations").w_full().grow(1.0).min_h(0.0);
    if s.info.is_some() && !s.host_has_automations() {
        return base
            .center()
            .gap(10.0)
            .px(24.0)
            .child(icon(&ic::CLOCK, 40.0).color(t.text3))
            .child(text("Update AgentOS").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
            .child(t.small("This host has no automations. Update agenthostd to run sessions on a schedule or from a webhook.").wrap().align(rune::Align::Center).max_w(420.0));
    }
    if s.automations.is_empty() {
        return empty(t).key("automations");
    }
    let Some(sel) = s.selected_automation() else { return base };
    let now = now_ms();
    let mac = t.mac();
    let mut list = col().key("automation-list").w_full().px(if mac { 10.0 } else { 6.0 }).py(6.0).gap(if mac { 2.0 } else { 4.0 }).role("list").focus_group();
    for a in &s.automations {
        list = list.child(list_row(s, t, a, a.automation_id == sel.automation_id, now));
    }
    let head = row()
        .w_full()
        .h(if mac { 40.0 } else { 46.0 })
        .px(if mac { 16.0 } else { 14.0 })
        .gap(8.0)
        .center_y()
        .child(t.section_title(if mac { "All automations" } else { "All Automations" }))
        .child(el().grow(1.0))
        .child(t.icon_button(&ic::PLUS, if mac { 14.0 } else { 16.0 }, "New Automation", |s: &mut App, _: &mut Tx| s.new_automation()).key("new-automation"));
    let mut left = col()
        .key("automation-pane")
        .w(if mac { 300.0 } else { 320.0 })
        .h_full()
        .min_h(0.0)
        .child(head)
        .child(col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list));
    left = if mac { left.fill(Color::WHITE.alpha(0.02)).border_sides([0.0, 0.5, 0.0, 0.0], t.sep) } else { left.border_sides([0.0, 1.0, 0.0, 0.0], Color::BLACK.alpha(0.35)) };
    let detail = col()
        .key("automation-detail")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(row().w_full().px(24.0).pt(20.0).pb(28.0).justify_center().child(detail(s, t, sel, now).w_full().max_w(t.column)));
    row().key("automations").w_full().grow(1.0).min_h(0.0).child(left).child(detail)
}

/// No automations yet: what they are, and the button that makes one.
fn empty(t: &Theme) -> El {
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .center()
        .gap(10.0)
        .px(24.0)
        .child(icon(&ic::CLOCK, 40.0).color(t.text3))
        .child(text("No Automations").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
        .child(
            t.small("An automation is a session that starts by itself: on a schedule, when a webhook arrives (a Sentry alert, a GitHub event), or when you run it. Each run is a session you can open and ask follow-up questions in.")
                .wrap()
                .align(rune::Align::Center)
                .max_w(460.0),
        )
        .child(row().pt(6.0).child(t.button("New Automation", Btn::Primary, None, |s| s.new_automation()).key("empty-new-automation")))
}

fn list_row(s: &App, t: &Theme, a: &Automation, selected: bool, now: i64) -> El {
    let mac = t.mac();
    let id = a.automation_id.clone();
    let on = a.enabled;
    let (ink, sub) = if a.enabled { (t.text, t.text2) } else { (t.text2, t.text3) };
    let names = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(1.0)
        .child(text(a.name.clone()).font(TextStyle { weight: if selected && !mac { 700.0 } else { 400.0 }, ..t.font() }).color(ink).truncate())
        .child(text(trigger_line(a)).font(t.font_small()).color(sub).truncate())
        .child(text(status_line(s, a, now)).font(t.font_small()).color(t.text3).truncate());
    let ids = id.clone();
    let mut r = row()
        .key(format!("auto-{}", a.automation_id))
        .w_full()
        .gap(if mac { 9.0 } else { 10.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .py(if mac { 7.0 } else { 9.0 })
        .items_start()
        .radius(t.row_radius)
        .role("listitem")
        .label(format!("{}, {}", a.name, trigger_line(a)))
        .focusable(true)
        .on_press(move |s: &mut App| s.select_automation(&ids))
        .child(row().size(16.0, 16.0).mt(1.0).center().child(icon(trigger_icon(&a.trigger), 14.0).color(sub)))
        .child(names)
        .child(t.switch(on, &format!("{} on", a.name), move |s: &mut App| s.set_automation_enabled(&id, !on)).key(format!("auto-enabled-{}", a.automation_id)).mt(1.0));
    r = if selected { r.fill(if mac { t.accent.alpha(0.22) } else { t.row_selected }) } else { r.hover(t.row_hover) };
    r
}

fn detail(s: &App, t: &Theme, a: &Automation, now: i64) -> El {
    let mac = t.mac();
    let id = a.automation_id.clone();
    let (idr, ide, idd) = (id.clone(), id.clone(), id.clone());
    let can_run = s.can_run_now(a);
    let running = a.last_run.as_ref().is_some_and(|r| r.outcome == RunOutcome::Running);
    let run_now = t
        .button("Run Now", if can_run { Btn::Primary } else { Btn::Default }, None, move |s| s.run_automation_now(&idr))
        .key("run-now")
        .help(if running { "A run is still going" } else { "Start a run now" })
        .disabled(!can_run)
        .opacity(if can_run { 1.0 } else { 0.5 });
    let on = a.enabled;
    let actions = row()
        .gap(8.0)
        .center_y()
        .child(t.switch(on, "On", move |s: &mut App| s.set_automation_enabled(&ide, !on)).key("detail-enabled"))
        .child(run_now)
        .child(t.small_button("Edit…", move |s| s.edit_automation(&idd)).key("edit-automation"));
    let head = row()
        .w_full()
        .gap(12.0)
        .items_start()
        .child(row().size(30.0, 30.0).radius(7.0).center().fill(Color::WHITE.alpha(0.08)).child(icon(trigger_icon(&a.trigger), 16.0).color(t.text)))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(text(a.name.clone()).key("automation-name").font(TextStyle::new(15.0, t.w_bold)).color(t.text).truncate())
                .child(t.small(format!("{} · {}", trigger_line(a), place_line(s, a))).truncate()),
        )
        .child(actions);
    let mut c = col().key("automation").w_full().gap(if mac { 6.0 } else { 10.0 }).child(head);
    // When it runs next, or why it won't.
    let next = match (&a.trigger, a.enabled, a.next_run_at) {
        (_, _, _) if running => None,
        (AutomationTrigger::Schedule { .. }, true, Some(n)) => Some(format!("Next run {}", when_label(n, now, s.look))),
        (AutomationTrigger::Manual, _, _) => Some("Runs only when you click Run Now.".to_string()),
        (_, false, _) => Some("Off: it doesn't start by itself until you turn it on.".to_string()),
        _ => None,
    };
    if let Some(n) = next {
        c = c.child(t.faint(n).pt(2.0));
    }
    if let Some(w) = webhook_box(s, t, a, now) {
        c = c.child(el().h(8.0)).child(w);
    }
    // What each run is asked.
    c = c
        .child(el().h(8.0))
        .child(t.group_title("Prompt"))
        .child(t.group(vec![row().w_full().px(if mac { 12.0 } else { 14.0 }).py(if mac { 8.0 } else { 12.0 }).child(text(clip(&a.prompt, 1200)).font(TextStyle::new(t.fs, 400.0).lh((t.fs * 1.45).round())).color(t.text).wrap().selectable(true).shrink().min_w(0.0))], 0.0));
    let approve = if a.auto_approve.is_empty() { "Nothing: every tool asks".to_string() } else { a.auto_approve.iter().map(|x| auto_approve_title(x)).collect::<Vec<_>>().join(", ") };
    c = c.child(el().h(8.0)).child(t.group_title("Each run")).child(t.group(
        vec![
            form_row(t, "Runs without asking", None, t.small(approve)),
            form_row(t, "Stops after", None, t.small(format!("{} minutes", a.max_minutes))),
        ],
        if mac { 12.0 } else { 0.0 },
    ));
    c.child(el().h(8.0)).child(runs(s, t, a, now))
}

/// A webhook automation's endpoint: the URL to give the sender, the secret, and what became
/// of the last delivery (how setup gets debugged).
fn webhook_box(s: &App, t: &Theme, a: &Automation, now: i64) -> Option<El> {
    let AutomationTrigger::Webhook { source, .. } = &a.trigger else { return None };
    let mac = t.mac();
    let info = a.webhook.clone().unwrap_or(WebhookInfo { url: None, has_secret: false, last_delivery: None });
    let mut rows = vec![url_row(s, t, "", &a.automation_id, info.url.as_deref())];
    let (secret, warn) = match (info.has_secret, source) {
        (true, _) => ("Stored", false),
        (false, WebhookSource::Generic) => ("None: the URL is the secret", false),
        (false, _) => ("Not set: paste it in Edit…", true),
    };
    rows.push(form_row(t, "Signing secret", None, text(secret).font(t.font_small()).color(if warn { t.warn } else { t.text2 })));
    let last = match &info.last_delivery {
        Some(d) => {
            let color = match d.result.as_str() {
                "started" => t.ok,
                "refused" => t.danger,
                _ => t.warn,
            };
            let word = capitalize(&d.result);
            let mut names = col().grow(1.0).min_w(0.0).gap(1.0).child(t.label("Last delivery"));
            if !d.detail.is_empty() {
                names = names.child(t.small(d.detail.clone()).wrap().selectable(true));
            }
            row()
                .key("last-delivery")
                .w_full()
                .gap(10.0)
                .px(if mac { 12.0 } else { 14.0 })
                .py(if mac { 8.0 } else { 12.0 })
                .center_y()
                .child(names)
                .child(row().gap(6.0).center_y().child(t.dot(color, 7.0)).child(text(format!("{word} · {}", when_label(d.at, now, s.look))).font(t.font_small()).color(color)))
        }
        None => form_row(t, "Last delivery", Some("None yet. Send a test from the sender to check the setup.".into()), el()),
    };
    rows.push(last);
    let mut c = col().key("webhook").w_full().gap(if mac { 6.0 } else { 10.0 }).child(t.group_title(format!("{} webhook", if *source == WebhookSource::Generic { "Incoming" } else { source.display_name() }))).child(t.group(rows, if mac { 12.0 } else { 0.0 }));
    if let Some(h) = setup_hint(*source) {
        c = c.child(footnote(t, h));
    }
    Some(c)
}

/// How to point the sender at the URL.
fn setup_hint(src: WebhookSource) -> Option<&'static str> {
    match src {
        WebhookSource::Sentry => Some("In Sentry: Settings → Developer Settings → Custom Integrations → New Internal Integration. Paste this URL as the Webhook URL, turn on Alert Rule Action, save, copy the Client Secret here. Then add \u{2018}Send a notification via <integration>\u{2019} to the alert rules you want investigated."),
        WebhookSource::Github => Some("In GitHub: the repository's Settings → Webhooks → Add webhook. Paste this URL as the Payload URL, pick application/json, type a secret and paste the same one here, then choose the events."),
        WebhookSource::Generic => Some("POST JSON to this URL. With a secret, send it as Authorization: Bearer <secret>."),
    }
}

/// The webhook URL with Copy and New URL…, or why there is none. `keys` prefixes its keys
/// (the editor shows it too, over the screen's).
fn url_row(s: &App, t: &Theme, keys: &str, id: &str, url: Option<&str>) -> El {
    let mac = t.mac();
    let Some(url) = url else {
        let why = if s.info.as_ref().is_some_and(|i| i.relay.configured) {
            "The relay hasn't given this automation a URL yet."
        } else {
            "This host has no relay, so nothing outside this computer can reach it. Start agenthostd with --relay-url to receive webhooks."
        };
        return form_row(t, "URL", Some(why.into()), el()).key(format!("{keys}webhook-url"));
    };
    let (u, idr) = (url.to_string(), id.to_string());
    row()
        .key(format!("{keys}webhook-url"))
        .w_full()
        .gap(10.0)
        .px(if mac { 12.0 } else { 14.0 })
        .py(if mac { 8.0 } else { 12.0 })
        .center_y()
        .child(t.label("URL"))
        .child(text(url.to_string()).font(TextStyle::new(t.fs_mono_small, 400.0).mono()).color(t.text2).selectable(true).truncate_middle().grow(1.0).shrink().min_w(0.0).help(url.to_string()))
        .child(
            t.small_button_tx("Copy", move |s, tx| {
                tx.copy(u.clone());
                s.notice = Some("Copied the webhook URL".into());
            })
            .key(format!("{keys}copy-url")),
        )
        .child(t.small_button("New URL…", move |s| s.rotate_webhook(&idr)).key(format!("{keys}rotate-url")))
}

fn runs(s: &App, t: &Theme, a: &Automation, now: i64) -> El {
    let mac = t.mac();
    let list = s.runs.get(&a.automation_id);
    let mut rows: Vec<El> = list.into_iter().flatten().map(|r| run_row(s, t, r, now)).collect();
    if rows.is_empty() {
        let msg = if list.is_none() { "Loading…" } else { "No runs yet" };
        rows.push(form_row(t, msg, Some("Run Now starts one. Each run is a session: its transcript is what the run did.".into()), el()));
    }
    let note = list.filter(|l| !l.is_empty()).map(|l| format!("{} shown · click one to open its session", l.len()));
    section(t, "Runs", note, t.group(rows, if mac { 34.0 } else { 0.0 })).key("runs")
}

fn run_row(s: &App, t: &Theme, r: &AutomationRun, now: i64) -> El {
    let mac = t.mac();
    let mut meta = vec![run_trigger_word(r.trigger).to_string(), when_label(r.started_at, now, s.look)];
    if let Some(f) = r.finished_at {
        meta.push(format!("took {}", span_label(f - r.started_at)));
    }
    let head = row()
        .gap(6.0)
        .center_y()
        .min_w(0.0)
        .child(text(outcome_word(r.outcome)).font(TextStyle { weight: t.w_bold, ..t.font() }).color(if r.outcome == RunOutcome::Running { t.text } else { outcome_color(t, r.outcome) }))
        .child(text(format!("· {}", meta.join(" · "))).font(t.font_small()).color(t.text2).truncate().shrink());
    let mut names = col().grow(1.0).min_w(0.0).gap(1.0).child(head);
    if let Some(d) = r.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        names = names.child(t.small(clip(d.trim(), 200)).truncate());
    }
    if let Some(sum) = r.summary.as_deref().filter(|d| !d.trim().is_empty()) {
        let line = sum.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        names = names.child(t.faint(clip(line, 200)).truncate());
    }
    let mut row_el = row()
        .key(format!("run-{}", r.run_id))
        .w_full()
        .gap(if mac { 10.0 } else { 12.0 })
        .px(if mac { 12.0 } else { 14.0 })
        .py(if mac { 8.0 } else { 11.0 })
        .items_start()
        .child(outcome_glyph(t, r.outcome).mt(2.0))
        .child(names);
    match r.session_id.clone() {
        Some(sid) => {
            row_el = row_el
                .role("button")
                .label(format!("{} run, {}: open its session", outcome_word(r.outcome), when_label(r.started_at, now, s.look)))
                .focusable(true)
                .hover(Color::WHITE.alpha(0.04))
                .on_press(move |s: &mut App| s.open_run(&sid))
                .child(icon(&ic::CHEVRON_R, 10.0).color(t.text3).mt(4.0))
        }
        None => row_el = row_el.role("listitem").label(format!("{} run", outcome_word(r.outcome))),
    }
    row_el
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Pickers: a native pop-up menu where there is one, else the app's own under the button
// ---------------------------------------------------------------------------

pub(crate) struct Opt {
    pub id: String,
    pub title: String,
    pub checked: bool,
    pub enabled: bool,
}

pub(crate) fn picker(t: &Theme, key: &'static str, label: &str, options: Vec<Opt>, pick: fn(&mut App, &str)) -> El {
    popup_button(t, key.to_string(), label).on_press_tx(move |s: &mut App, tx: &mut Tx| {
        if rune::popup_menu_supported() {
            let menu = options.iter().fold(rune::PopupMenu::new(), |m, o| m.item(rune::PopupItem::new(o.id.clone(), o.title.clone()).checked(o.checked).enabled(o.enabled)));
            tx.popup_menu(menu, key, move |s: &mut App, picked, _| {
                if let Some(id) = picked {
                    pick(s, id);
                }
            });
        } else {
            s.menu = if s.menu == Some(Menu::Pick(key)) { None } else { Some(Menu::Pick(key)) };
        }
    })
}

pub(crate) fn pick_menu(s: &App, t: &Theme, key: &'static str, options: Vec<Opt>, pick: fn(&mut App, &str)) -> Option<El> {
    if s.menu != Some(Menu::Pick(key)) {
        return None;
    }
    let items = options
        .into_iter()
        .map(|o| {
            let id = o.id.clone();
            Some(Item::new(&o.title, String::new(), move |s, _| pick(s, &id)).checked(o.checked).enabled(o.enabled))
        })
        .collect();
    Some(menu_box(t, items, 280.0).key(format!("{key}-menu")).anchored(key, rune::Side::Bottom, rune::Align::Start, 4.0).raise())
}

fn workspace_options(s: &App) -> Vec<Opt> {
    let mut ws: Vec<&Workspace> = s.workspaces.iter().collect();
    ws.sort_by_key(|w| w.kind == WorkspaceKind::Scratch);
    ws.into_iter()
        .map(|w| Opt {
            id: w.workspace_id.clone(),
            title: if w.kind == WorkspaceKind::Scratch { "Quick tasks (no project folder)".into() } else { format!("{} · {}", w.name, tilde(&w.path)) },
            checked: s.draft.workspace.as_deref() == Some(w.workspace_id.as_str()),
            enabled: true,
        })
        .collect()
}

fn login_options(s: &App) -> Vec<Opt> {
    let agents = s.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
    agents
        .iter()
        .map(|a| {
            let key = login_key(a);
            let title = if a.available { a.label() } else { format!("{} ({})", a.label(), a.problem.clone().unwrap_or_else(|| "unavailable".into())) };
            Opt { checked: s.draft.login.as_deref() == Some(key.as_str()), id: key, title, enabled: a.available }
        })
        .collect()
}

fn preset_options(s: &App) -> Vec<Opt> {
    Preset::ALL.into_iter().map(|p| Opt { id: p.id().into(), title: p.title().into(), checked: s.draft.preset == p, enabled: true }).collect()
}

fn weekday_options(s: &App) -> Vec<Opt> {
    // Monday first, as calendars here read.
    [1u8, 2, 3, 4, 5, 6, 0].into_iter().map(|d| Opt { id: d.to_string(), title: format!("Every {}", WEEKDAYS[d as usize]), checked: s.draft.weekday == d, enabled: true }).collect()
}

fn pick_ws(s: &mut App, id: &str) {
    s.draft_pick_workspace(id);
}
fn pick_login(s: &mut App, id: &str) {
    s.draft.login = Some(id.to_string());
}
fn pick_preset(s: &mut App, id: &str) {
    if let Some(p) = Preset::of_id(id) {
        s.draft_set_preset(p);
    }
}
fn pick_weekday(s: &mut App, id: &str) {
    if let Ok(d) = id.parse() {
        s.draft_set_weekday(d);
    }
}

// ---------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------

/// A one-line field; `live` re-checks the schedule as it's typed.
fn field(t: &Theme, edit: &TextEdit, key: &str, placeholder: &str, access: fn(&mut App) -> &mut TextEdit, live: bool, mono: bool) -> El {
    let style = if mono { TextStyle::new(t.fs_mono, 400.0).mono().lh((t.fs * 1.3).round()) } else { TextStyle::new(t.fs, 400.0).lh((t.fs * 1.3).round()) };
    let mut input = rune::text_input(edit, style)
        .single_line()
        .key(key.to_string())
        .label(placeholder.to_string())
        .grow(1.0)
        .min_w(0.0)
        .color(t.text)
        .placeholder(placeholder.to_string(), t.text3)
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(access)
        .on_submit(|s: &mut App, _: &mut Tx| s.save_automation());
    if live {
        input = input.on_input_change(|s: &mut App, _: &mut Tx| s.recheck_schedule());
    }
    if edit.is_secure() {
        input = input.secure();
    }
    boxed(t, edit.focused, input, false)
}

/// A multi-line field that grows from `min` to `max` lines.
fn area(t: &Theme, edit: &TextEdit, key: &str, placeholder: &str, access: fn(&mut App) -> &mut TextEdit, min: usize, max: usize) -> El {
    let input = rune::text_input(edit, TextStyle::new(t.fs, 400.0).lh((t.fs * 1.45).round()))
        .key(key.to_string())
        .label(placeholder.to_string())
        .w_full()
        .color(t.text)
        .placeholder(placeholder.to_string(), t.text3)
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(access);
    boxed(t, edit.focused, rune::auto_grow(input, min, max), true)
}

fn boxed(t: &Theme, focused: bool, input: El, multi: bool) -> El {
    let mac = t.mac();
    let mut b = row().w_full().px(if mac { 7.0 } else { 10.0 }).radius(if mac { 5.0 } else { 7.0 }).fill(t.field).child(input);
    b = if multi { b.py(if mac { 5.0 } else { 8.0 }).items_start() } else { b.h(if mac { 22.0 } else { 34.0 }).center_y() };
    if mac {
        b = b.border(0.5, t.field_border);
    }
    if focused {
        b = b.box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() });
    }
    b
}

/// A row of buttons, one per choice, the chosen one filled.
fn segmented<T: Copy + PartialEq + 'static>(t: &Theme, key: &str, choices: &[(T, &str)], current: T, pick: fn(&mut App, T)) -> El {
    let mut r = row().key(key.to_string()).gap(6.0).center_y().role("radiogroup").focus_group();
    for (v, title) in choices {
        let v = *v;
        r = r.child(t.button(title, if v == current { Btn::Primary } else { Btn::Default }, None, move |s| pick(s, v)).key(format!("{key}-{}", title.to_lowercase().replace(' ', "-"))).checked(v == current));
    }
    r
}

/// A titled block of the form.
fn block(t: &Theme, title: &str, content: El) -> El {
    col().w_full().gap(6.0).child(t.group_title(title)).child(content)
}

fn editor_body(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let d = &s.draft;
    let inset = if mac { 12.0 } else { 0.0 };
    let fw = if mac { 300.0 } else { 320.0 };
    let ws_label = d.workspace.as_deref().and_then(|w| s.workspace(w)).map(|w| if w.kind == WorkspaceKind::Scratch { "Quick tasks".to_string() } else { w.name.clone() }).unwrap_or_else(|| "Choose…".into());
    let agents = s.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
    let login = d.login.as_deref().and_then(|k| agents.iter().find(|a| login_key(a) == k));
    let login_label = login.map(|a| a.label()).unwrap_or_else(|| "Choose…".into());
    let login_el = row()
        .gap(8.0)
        .center_y()
        .child(match login {
            Some(a) => agent_badge(t, a.agent, 18.0, a.available),
            None => el(),
        })
        .child(picker(t, "auto-login", &login_label, login_options(s), pick_login));
    let about = t.group(
        vec![
            form_row(t, "Name", None, row().w(fw).child(field(t, &d.name, "auto-name", "Nightly dependency upgrade", |s| &mut s.draft.name, false, false))),
            form_row(t, "Workspace", Some("Each run works on a branch of its own".into()), picker(t, "auto-ws", &ws_label, workspace_options(s), pick_ws)),
            form_row(t, "Agent", None, login_el),
        ],
        inset,
    );
    let mut body = col().w_full().gap(if mac { 14.0 } else { 20.0 }).child(block(t, "Automation", about));

    // What starts it.
    let kinds: Vec<(TriggerKind, &str)> = TriggerKind::ALL.iter().map(|k| (*k, k.title())).collect();
    let mut starts = col().w_full().gap(if mac { 8.0 } else { 10.0 }).child(segmented(t, "auto-trigger", &kinds, d.trigger, |s, k| s.draft_set_trigger(k)));
    match d.trigger {
        TriggerKind::Schedule => starts = starts.child(schedule_box(s, t)),
        TriggerKind::Webhook => starts = starts.child(webhook_editor(s, t)),
        TriggerKind::Manual => starts = starts.child(footnote(t, "It runs only when you click Run Now.")),
    }
    body = body.child(block(t, "Starts", starts));

    // The prompt.
    let note = match d.trigger {
        TriggerKind::Webhook => "Fields like {{sentry.title}} or {{github.event}} are filled in from each delivery; the whole payload is attached as webhook.json.",
        _ => "Each run starts a session with this prompt. Its newest run stays open for follow-up questions.",
    };
    body = body.child(block(t, "Prompt", col().w_full().gap(6.0).child(area(t, &d.prompt, "auto-prompt", "What should each run do?", |s| &mut s.draft.prompt, 5, 14)).child(footnote(t, note))));

    // What it may do without asking.
    let approve_rows = AUTO_APPROVABLE
        .iter()
        .map(|tool| {
            let on = d.auto_approve.iter().any(|x| x == tool);
            let detail = match *tool {
                "git_push" => Some("Only the run's own branch. A force-push still asks.".to_string()),
                "create_pull_request" => Some("From the run's branch, with your GitHub CLI login".to_string()),
                _ => None,
            };
            let title = auto_approve_title(tool);
            form_row(t, title, detail, t.switch(on, title, move |s: &mut App| s.draft_toggle_approve(tool)).key(format!("approve-{tool}")))
        })
        .collect();
    body = body.child(block(
        t,
        "Runs without asking",
        col().w_full().gap(6.0).child(t.group(approve_rows, inset)).child(footnote(t, "Everything else asks, as in any session: the request waits in the run's session and on your phone.")),
    ));
    let max = row().gap(8.0).center_y().child(row().w(64.0).child(field(t, &d.max_minutes, "auto-max", "60", |s| &mut s.draft.max_minutes, false, false))).child(t.small("minutes"));
    body = body.child(t.group(vec![form_row(t, "Stop a run after", Some("1 to 720 minutes".into()), max)], inset));
    if let Some(e) = &d.error {
        body = body.child(text(e.clone()).key("auto-error").font(t.font_small()).color(t.danger).wrap());
    }
    body
}

fn schedule_box(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let d = &s.draft;
    let mut rows = vec![form_row(t, "Repeats", None, picker(t, "auto-preset", d.preset.title(), preset_options(s), pick_preset))];
    if d.preset == Preset::Weekly {
        rows.push(form_row(t, "On", None, picker(t, "auto-weekday", &format!("Every {}", WEEKDAYS[d.weekday.min(6) as usize]), weekday_options(s), pick_weekday)));
    }
    if d.preset == Preset::Monthly {
        rows.push(form_row(t, "Day of the month", None, row().w(56.0).child(field(t, &d.day, "auto-day", "1", |s| &mut s.draft.day, true, false))));
    }
    if !matches!(d.preset, Preset::Hourly | Preset::Custom) {
        rows.push(form_row(t, "At", Some("This computer's time".into()), row().w(72.0).child(field(t, &d.time, "auto-time", "09:00", |s| &mut s.draft.time, true, false))));
    }
    if d.preset == Preset::Custom {
        rows.push(form_row(
            t,
            "Cron",
            Some("Minute, hour, day of month, month, day of week, in this computer's time. Or @hourly, @daily, @weekly, @monthly.".into()),
            row().w(if mac { 200.0 } else { 220.0 }).child(field(t, &d.cron, "auto-cron", "0 9 * * 1-5", |s| &mut s.draft.cron, true, true)),
        ));
    }
    // The host's word on it: the schedule in words and the next few times, or what's wrong.
    let check = match &d.check {
        Some(ScheduleCheck { error: Some(e), .. }) => text(e.clone()).key("schedule-error").font(t.font_small()).color(t.danger).wrap(),
        Some(c) => {
            let now = now_ms();
            let next: Vec<String> = c.next.iter().take(3).map(|n| when_label(*n, now, s.look)).collect();
            let mut col_el = col().key("schedule-check").gap(1.0).child(t.label(c.text.clone().unwrap_or_default()));
            if !next.is_empty() {
                col_el = col_el.child(t.small(format!("Next: {}", next.join(", "))));
            }
            col_el
        }
        None if !d.checked.is_empty() && s.host_has_automations() => t.small("Checking…").key("schedule-check"),
        None => el(),
    };
    rows.push(row().w_full().gap(10.0).px(if mac { 12.0 } else { 14.0 }).py(if mac { 8.0 } else { 12.0 }).center_y().child(icon(&ic::CLOCK, 14.0).color(t.text2)).child(check.grow(1.0).min_w(0.0)));
    let mut c = col().w_full().gap(if mac { 8.0 } else { 10.0 }).child(t.group(rows, if mac { 12.0 } else { 0.0 }));
    if d.id.is_none() {
        let mut ex = row().key("schedule-examples").w_full().gap(6.0).center_y().flex_wrap().child(t.small("Start from an example:"));
        for (i, (name, _)) in SCHEDULE_EXAMPLES.iter().enumerate() {
            ex = ex.child(t.small_button(name, move |s| s.draft_use_example(i)).key(format!("example-{i}")));
        }
        c = c.child(ex);
    }
    c
}

fn webhook_editor(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let d = &s.draft;
    let inset = if mac { 12.0 } else { 0.0 };
    let existing = d.id.as_deref().and_then(|id| s.automation(id));
    let info = existing.and_then(|a| a.webhook.clone());
    // The secret only means something for the sender it was stored for.
    let same_source = existing.is_some_and(|a| matches!(&a.trigger, AutomationTrigger::Webhook { source, .. } if *source == d.source));
    let stored = info.as_ref().is_some_and(|w| w.has_secret) && same_source;
    let sources = [(WebhookSource::Sentry, "Sentry"), (WebhookSource::Github, "GitHub"), (WebhookSource::Generic, "Generic")];
    let what = match d.source {
        WebhookSource::Sentry => "The integration's Client Secret, so only Sentry can start runs",
        WebhookSource::Github => "The webhook's secret, so only GitHub can start runs",
        WebhookSource::Generic => "Optional: senders then send Authorization: Bearer <secret>",
    };
    let placeholder = if stored && !d.remove_secret {
        "Stored · leave blank to keep it"
    } else if stored {
        "Removed when you save"
    } else {
        "Paste the secret"
    };
    let mut secret = row().gap(6.0).center_y().child(row().w(if mac { 220.0 } else { 240.0 }).child(field(t, &d.secret, "auto-secret", placeholder, |s| &mut s.draft.secret, false, false)));
    if stored {
        secret = secret.child(t.small_button(if d.remove_secret { "Keep" } else { "Remove" }, |s| s.draft.remove_secret = !s.draft.remove_secret).key("secret-remove"));
    }
    let mut rows = vec![form_row(t, "Sender", None, segmented(t, "auto-source", &sources, d.source, |s, src| s.draft_set_source(src))), form_row(t, "Secret", Some(what.into()), secret)];
    match existing {
        Some(a) if matches!(a.trigger, AutomationTrigger::Webhook { .. }) => rows.push(url_row(s, t, "editor-", &a.automation_id, info.as_ref().and_then(|w| w.url.as_deref()))),
        _ => {
            let note = if s.info.as_ref().is_some_and(|i| !i.relay.configured) {
                "This host has no relay, so nothing outside this computer can reach a webhook. Start agenthostd with --relay-url first."
            } else {
                "The URL to give the sender appears here and beside the automation once you save."
            };
            rows.push(form_row(t, "URL", Some(note.into()), el()).key("editor-webhook-url"));
        }
    }
    let mut c = col().w_full().gap(if mac { 8.0 } else { 10.0 }).child(t.group(rows, inset));
    if let Some(h) = setup_hint(d.source) {
        c = c.child(footnote(t, h));
    }
    // Rarely needed: collapsed.
    let open = d.advanced;
    c = c.child(
        row()
            .key("auto-advanced")
            .gap(6.0)
            .center_y()
            .role("button")
            .label(if open { "Hide more options" } else { "More options" })
            .expanded(open)
            .focusable(true)
            .on_press(|s: &mut App| s.draft.advanced = !s.draft.advanced)
            .child(icon(if open { &ic::CHEVRON_D } else { &ic::CHEVRON_R }, 10.0).color(t.text2))
            .child(t.small("Repeats and filters")),
    );
    if open {
        let dedupe_note = if d.source == WebhookSource::Sentry { "Sentry alerts for the same issue are ignored for the cooldown by themselves. A template here replaces that." } else { "A template like {{payload.issue.id}}: a delivery with the same key as a recent run starts nothing" };
        let rows = vec![
            form_row(t, "Ignore repeats of", Some(dedupe_note.into()), row().w(if mac { 200.0 } else { 220.0 }).child(field(t, &d.dedupe, "auto-dedupe", "{{sentry.key}}", |s| &mut s.draft.dedupe, false, true))),
            form_row(t, "For", None, row().gap(8.0).center_y().child(row().w(72.0).child(field(t, &d.cooldown, "auto-cooldown", "1440", |s| &mut s.draft.cooldown, false, false))).child(t.small("minutes"))),
            col()
                .w_full()
                .gap(6.0)
                .px(if mac { 12.0 } else { 14.0 })
                .py(if mac { 8.0 } else { 12.0 })
                .child(t.label("Only when"))
                .child(t.small("One rule per line, field = value, like payload.action = opened or headers.x-github-event = issues. Every rule must hold.").wrap())
                .child(area(t, &d.filters, "auto-filters", "payload.action = opened", |s| &mut s.draft.filters, 2, 6)),
        ];
        c = c.child(t.group(rows, inset));
    }
    c
}

/// The editor: a sheet under the toolbar (macOS) or a dialog (GNOME).
pub fn editor(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let new = s.draft.id.is_none();
    let title = if new { "New Automation" } else { "Edit Automation" };
    let menus: Vec<El> = [
        pick_menu(s, t, "auto-ws", workspace_options(s), pick_ws),
        pick_menu(s, t, "auto-login", login_options(s), pick_login),
        pick_menu(s, t, "auto-preset", preset_options(s), pick_preset),
        pick_menu(s, t, "auto-weekday", weekday_options(s), pick_weekday),
    ]
    .into_iter()
    .flatten()
    .collect();
    let save_label = if s.draft.saving { "Saving…" } else if new { "Create" } else { "Save" };
    let close = |s: &mut App| s.close_automation_editor();
    let delete = s.draft.id.clone().map(|id| t.small_button_danger("Delete…", move |s| s.delete_automation(&id)).key("delete-automation"));
    if mac {
        let save = t.button(save_label, Btn::Primary, Some("⌘↩"), |s| s.save_automation()).key("save-automation").px(14.0).default_button();
        let mut foot = row().w_full().gap(8.0).px(20.0).pt(16.0).pb(18.0).center_y();
        if let Some(d) = delete {
            foot = foot.child(d);
        }
        foot = foot.child(el().grow(1.0)).child(t.button("Cancel", Btn::Default, None, close).px(14.0).key("cancel-automation")).child(save);
        let sheet = col()
            .key("automation-editor")
            .w(620.0)
            .max_w(620.0)
            .shrink()
            .min_h(0.0)
            .corners([0.0, 0.0, 12.0, 12.0])
            .fill(t.sheet)
            .border_sides([0.0, 0.5, 0.5, 0.5], Color::WHITE.alpha(0.14))
            .shadow(24.0, 60.0, Color::BLACK.alpha(0.55))
            .modal(true, close)
            .child(
                col()
                    .w_full()
                    .gap(3.0)
                    .px(20.0)
                    .pt(18.0)
                    .pb(6.0)
                    .child(text(title).font(TextStyle::new(15.0, 600.0)).color(t.text))
                    .child(text("A session that starts by itself. Each run's transcript is its output.").font(TextStyle::new(12.0, 400.0)).color(t.text2)),
            )
            .child(col().w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(editor_body(s, t).px(20.0).pt(10.0).pb(4.0)))
            .child(foot)
            .children(menus);
        return scrim("automation-layer", 0.32, close).pt(t.toolbar_h).pb(20.0).items_center().child(sheet);
    }
    let header = row()
        .w_full()
        .h(48.0)
        .px(8.0)
        .center_y()
        .child(t.button("Cancel", Btn::Default, None, close).key("cancel-automation"))
        .child(row().grow(1.0).justify_center().child(text(title).font(TextStyle::new(14.5, 700.0)).color(t.text)))
        .child(t.button(save_label, Btn::Primary, None, |s| s.save_automation()).key("save-automation").default_button());
    let mut body = editor_body(s, t).px(24.0).pt(12.0).pb(24.0);
    body = body.child(t.small("Ctrl+Enter saves").font(TextStyle::new(13.0, 400.0)));
    if let Some(d) = delete {
        body = body.child(row().w_full().child(d));
    }
    let dialog = col()
        .key("automation-editor")
        .w(600.0)
        .shrink()
        .min_h(0.0)
        .radius(14.0)
        .fill(t.sheet)
        .border(1.0, Color::BLACK.alpha(0.6))
        .shadow(20.0, 60.0, Color::BLACK.alpha(0.5))
        .modal(true, close)
        .child(header)
        .child(col().w_full().shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(body))
        .children(menus);
    scrim("automation-layer", 0.5, close).py(30.0).center().child(dialog)
}

/// Delete… and New URL… ask first.
pub fn confirm(s: &App, t: &Theme) -> Option<El> {
    let mac = t.mac();
    let (key, title, msg, action, id, rotate) = if let Some(id) = &s.confirm_delete_auto {
        let name = s.automation(id).map(|a| a.name.clone()).unwrap_or_default();
        ("delete-alert", format!("Delete \u{201c}{}\u{201d}?", clip(&name, 60)), "It won't run again. Its runs' sessions stay in the sidebar.".to_string(), "Delete", id.clone(), false)
    } else if let Some(id) = &s.confirm_rotate {
        let sender = match s.automation(id).map(|a| &a.trigger) {
            Some(AutomationTrigger::Webhook { source: WebhookSource::Sentry, .. }) => "Sentry",
            Some(AutomationTrigger::Webhook { source: WebhookSource::Github, .. }) => "GitHub",
            _ => "the sender",
        };
        ("rotate-alert", "Make a new webhook URL?".to_string(), format!("The current URL stops working at once. Give the new one to {sender}."), "New URL", id.clone(), true)
    } else {
        return None;
    };
    let cancel = |s: &mut App| {
        s.confirm_delete_auto = None;
        s.confirm_rotate = None;
    };
    let go = t
        .button(action, if rotate { Btn::Primary } else { Btn::Destructive }, None, move |s| if rotate { s.rotate_webhook(&id) } else { s.delete_automation(&id) })
        .key(format!("confirm-{key}"))
        .default_button();
    let buttons = row().w_full().gap(8.0).child(el().grow(1.0)).child(t.button("Cancel", Btn::Default, None, cancel).key(format!("cancel-{key}"))).child(go);
    let mut panel = col()
        .key(key)
        .role("alertdialog")
        .label(title.clone())
        .w(if mac { 340.0 } else { 380.0 })
        .gap(if mac { 12.0 } else { 16.0 })
        .padding(if mac { 20.0 } else { 24.0 })
        .fill(t.sheet)
        .shadow(20.0, 50.0, Color::BLACK.alpha(0.5))
        .modal(true, cancel)
        .child(text(title).font(TextStyle::new(if mac { 13.0 } else { 16.0 }, 700.0)).color(t.text).wrap())
        .child(t.small(msg).wrap())
        .child(buttons);
    panel = if mac { panel.radius(12.0).border(0.5, Color::WHITE.alpha(0.18)) } else { panel.radius(14.0).border(1.0, Color::BLACK.alpha(0.6)) };
    Some(scrim("confirm-automation-layer", 0.32, cancel).center().child(panel))
}
