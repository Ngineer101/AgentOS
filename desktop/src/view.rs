//! The seven screens: Sessions (sidebar), Session, New session, Workspaces,
//! Connectors, Devices and Host status.

use crate::bridge::{Refresh, Tag};
use crate::model::{ago, login_key, App, Conn, Screen};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use rune::{col, el, row, text, Color, Cx, El, Icon, ScrollSpec, Tx};
use rune_kit::shell::{RowStatus, Sidebar, SidebarRow, SidebarSection, SplitView, Toolbar};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Markdown, Switch, TextInput};
use serde_json::{json, Value};

static PLUS: Icon = Icon {
    name: "ah-plus",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static FOLDER: Icon = Icon {
    name: "ah-folder",
    view: 16.0,
    body: r#"<path d="M2.5 4.5H6.5L8 6H13.5V12.5H2.5Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
static PLUG: Icon = Icon {
    name: "ah-plug",
    view: 16.0,
    body: r#"<path d="M6 2.5V5.5M10 2.5V5.5M4.5 5.5H11.5V8A3.5 3.5 0 0 1 4.5 8Z M8 11.5V13.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static PHONE: Icon = Icon {
    name: "ah-phone",
    view: 16.0,
    body: r#"<rect x="4.5" y="2" width="7" height="12" rx="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M7 11.5H9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static CLIP: Icon = Icon {
    name: "ah-clip",
    view: 16.0,
    body: r#"<path d="M13 7.5L8.2 12.3A3 3 0 0 1 4 8.1L9 3.1A2 2 0 0 1 11.8 5.9L7 10.7A1 1 0 0 1 5.6 9.3L10 4.9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static MIC: Icon = Icon {
    name: "ah-mic",
    view: 16.0,
    body: r#"<rect x="6" y="2" width="4" height="8" rx="2" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M3.5 7.5A4.5 4.5 0 0 0 12.5 7.5M8 12V14" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" fill="none"/>"#,
};
static CLOSE: Icon = Icon {
    name: "ah-close",
    view: 16.0,
    body: r#"<path d="M4.5 4.5L11.5 11.5M11.5 4.5L4.5 11.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static STOP: Icon = Icon {
    name: "ah-stop",
    view: 16.0,
    body: r#"<rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor"/>"#,
};
static PULSE: Icon = Icon {
    name: "ah-pulse",
    view: 16.0,
    body: r#"<path d="M2 8.5H5L6.5 5L9 11.5L10.5 8.5H14" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

fn kit(cx: &Cx<App>) -> Kit {
    Kit::neutral(cx.system_dark())
}

fn label(t: impl Into<String>, k: &Kit) -> El {
    text(t.into()).font(k.ty.ui).color(k.c.text_base)
}
fn muted(t: impl Into<String>, k: &Kit) -> El {
    text(t.into()).font(k.ty.small).color(k.c.text_muted)
}
fn muted_wrap(t: impl Into<String>, k: &Kit) -> El {
    muted(t, k).wrap()
}
fn heading(t: impl Into<String>, k: &Kit) -> El {
    text(t.into()).font(k.ty.body_medium).color(k.c.text_strong)
}
fn mono(t: impl Into<String>, k: &Kit) -> El {
    text(t.into()).font(k.ty.mono).color(k.c.text_base).wrap().selectable(true)
}
/// An agent's reply rendered as Markdown. `streaming` is for the reply still
/// arriving: an open emphasis or code span draws styled rather than as markers.
/// Links open in the browser, but only web and mail links: the text comes from
/// the agent, so `file:` and custom schemes are left as styled text.
fn reply(src: &str, streaming: bool, k: &Kit) -> El {
    Markdown::new(src)
        .selectable(true)
        .streaming(streaming)
        .scroll_wide(true)
        .on_link(|s: &mut App, url: &str| {
            if is_web_link(url) {
                s.open_queue.push(url.to_string());
            }
        })
        .build(k)
}
pub(crate) fn is_web_link(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:"].iter().any(|p| lower.starts_with(p))
}
fn card(k: &Kit) -> El {
    col().w_full().gap(8.0).padding(14.0).radius(k.radius.md).fill(k.c.layer_01).border(1.0, k.c.border_weak)
}
fn button(t: &str, k: &Kit, v: ButtonVariant, f: impl Fn(&mut App) + 'static) -> El {
    Button::new(t.to_string()).variant(v).control_size(ControlSize::Regular).on_press(f).build(k)
}
fn badge(t: &str, accent: bool, k: &Kit) -> El {
    Badge::new(t.to_string()).variant(if accent { BadgeVariant::Accent } else { BadgeVariant::Subtle }).build(k)
}

fn state_label(s: SessionState) -> &'static str {
    match s {
        SessionState::Created => "created",
        SessionState::Starting => "starting",
        SessionState::Idle => "idle",
        SessionState::Running => "running",
        SessionState::WaitingApproval => "needs approval",
        SessionState::Ended => "ended",
        SessionState::Failed => "failed",
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = kit(cx);
    let (title, body) = match &s.screen {
        Screen::Session(id) => (session_title(s, id), session_screen(s, id, &k)),
        Screen::NewSession => ("New session".to_string(), new_session_screen(s, &k)),
        Screen::Workspaces => ("Workspaces".to_string(), workspaces_screen(s, &k)),
        Screen::Connectors => ("Connectors".to_string(), connectors_screen(s, &k)),
        Screen::Devices => ("Devices".to_string(), devices_screen(s, &k)),
        Screen::Host => ("Host status".to_string(), host_screen(s, &k)),
    };
    let main = col().key("main").w_full().h_full().min_h(0.0).child(banners(s, &k)).child(body);
    SplitView::new(main)
        .sidebar(&s.sidebar, |s: &mut App| &mut s.sidebar, sidebar(s, &k))
        .toolbar(Toolbar::new().title(title))
        .titlebar_inset(cx.titlebar_inset())
        .frame_inset(8.0)
        .toolbar_in_panel()
        .sidebar_padding(18.0, 10.0)
        .build(&k)
}

fn session_title(s: &App, id: &str) -> String {
    match s.session(id) {
        Some(x) => {
            let ws = s.workspace(&x.workspace_id).map(|w| w.name.clone()).unwrap_or_default();
            let profile = x
                .profile_id
                .as_ref()
                .and_then(|id| s.info.as_ref()?.agents.iter().find(|a| a.profile_id.as_ref() == Some(id)))
                .and_then(|a| a.profile_name.clone())
                .map(|n| format!(" · {n}"))
                .unwrap_or_default();
            format!("{}{profile} · {}", x.agent.display_name(), ws)
        }
        None => "Session".into(),
    }
}

/// Connection state, approvals waiting anywhere, and errors.
fn banners(s: &App, k: &Kit) -> El {
    let mut c = col().key("banners").w_full().gap(6.0).px(16.0).pt(10.0);
    let strip = |t: String, role: rune_kit::StateColors| {
        row().w_full().px(12.0).py(8.0).radius(k.radius.md).fill(role.bg).border(1.0, role.border).gap(10.0).center_y().child(
            text(t).font(k.ty.ui_medium).color(role.fg).wrap().grow(1.0),
        )
    };
    match &s.conn {
        Conn::Connecting => c = c.child(strip("Connecting to the host…".into(), k.c.info)),
        Conn::StartingHost => c = c.child(strip("No host was running, so the app started one…".into(), k.c.info)),
        Conn::Down(e) => c = c.child(strip(format!("Not connected to the host: {e}. Retrying."), k.c.warning)),
        Conn::Connected => {}
    }
    if let Some(a) = s.approvals.first() {
        let sid = a.session_id.clone();
        let more = if s.approvals.len() > 1 { format!(" (+{} more)", s.approvals.len() - 1) } else { String::new() };
        c = c.child(
            strip(format!("Approval needed: {} / {}{more}", a.connector_name, a.tool), k.c.warning)
                .key("approval-banner")
                .child(el().grow(1.0))
                .child(button("Review", k, ButtonVariant::Contrast, move |s: &mut App| s.open_session(&sid))),
        );
    }
    if let Some(e) = &s.error {
        c = c.child(
            strip(e.clone(), k.c.danger)
                .key("error-banner")
                .child(el().grow(1.0))
                .child(button("Dismiss", k, ButtonVariant::Ghost, |s: &mut App| s.error = None)),
        );
    }
    if let Some(n) = &s.notice {
        c = c.child(strip(n.clone(), k.c.success).child(el().grow(1.0)).child(button("OK", k, ButtonVariant::Ghost, |s: &mut App| s.notice = None)));
    }
    c
}

// ---------------------------------------------------------------------------
// Sessions (sidebar)
// ---------------------------------------------------------------------------

fn sidebar(s: &App, k: &Kit) -> El {
    let nav = |t: &str, icon: &'static Icon, screen: Screen, key: &str| {
        let selected = s.screen == screen;
        SidebarRow::new(t.to_string())
            .icon(icon)
            .height(32.0)
            .selected(selected)
            .on_select(move |s: &mut App, _: &mut Tx| {
                s.screen = screen.clone();
                if screen == Screen::Host {
                    s.refresh(Refresh::HostInfo);
                }
            })
            .build(k)
            .key(key.to_string())
    };
    let mut sb = Sidebar::new()
        .label("Sessions")
        .inset(0.0)
        .header_gap(2.0)
        .header_padding(0.0, 12.0)
        .row_gap(4.0)
        .header(nav("New session", &PLUS, Screen::NewSession, "nav-new"))
        .footer(
            col()
                .gap(2.0)
                .child(nav("Workspaces", &FOLDER, Screen::Workspaces, "nav-workspaces"))
                .child(nav("Connectors", &PLUG, Screen::Connectors, "nav-connectors"))
                .child(nav("Devices", &PHONE, Screen::Devices, "nav-devices"))
                .child(nav("Host status", &PULSE, Screen::Host, "nav-host")),
        );
    // Sessions grouped by workspace, newest first.
    for w in &s.workspaces {
        let mut list: Vec<&Session> = s.sessions.iter().filter(|x| x.workspace_id == w.workspace_id).collect();
        if list.is_empty() {
            continue;
        }
        list.sort_by_key(|x| std::cmp::Reverse(x.updated_at));
        let rows = list.into_iter().map(|x| {
            let id = x.session_id.clone();
            let mut r = SidebarRow::new(x.title.clone().unwrap_or_else(|| x.agent.display_name().to_string()))
                .height(45.0)
                .subtitle(format!("{} · {}", state_label(x.state), ago(x.updated_at)))
                .selected(s.screen == Screen::Session(x.session_id.clone()))
                .on_select(move |s: &mut App, _: &mut Tx| s.open_session(&id));
            r = match x.state {
                SessionState::Running | SessionState::Starting => r.status(RowStatus::Busy),
                SessionState::WaitingApproval | SessionState::Failed => r.status(RowStatus::Attention),
                _ => r,
            };
            r.build(k).key(x.session_id.clone())
        });
        sb = sb.section(SidebarSection::new(w.name.clone()).rows(rows));
    }
    sb.build(k)
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

fn session_screen(s: &App, id: &str, k: &Kit) -> El {
    let Some(sess) = s.session(id) else { return col().padding(24.0).child(muted("This session no longer exists.", k)) };
    let t = s.transcripts.get(id);
    let events: &[Event] = t.map(|t| t.events.as_slice()).unwrap_or(&[]);
    // Tool results by call id, so a call row shows its result.
    let mut results: std::collections::HashMap<&str, &Event> = std::collections::HashMap::new();
    for e in events {
        if e.kind == EventType::ToolResult {
            if let Some(cid) = e.payload["tool_call_id"].as_str() {
                results.insert(cid, e);
            }
        }
    }
    let mut lines = col().key("transcript-rows").w_full().gap(10.0).px(24.0).py(16.0);
    for e in events {
        if let Some(row) = event_row(s, sess, e, &results, k) {
            lines = lines.child(row.key(format!("ev-{}", e.seq)));
        }
    }
    if let Some(live) = t.map(|t| t.live.as_str()).filter(|l| !l.is_empty()) {
        lines = lines.child(reply(live, true, k).key("live"));
    }
    if events.is_empty() {
        lines = lines.child(muted("No messages yet.", k));
    }
    let transcript = col().key("transcript").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: true }).child(lines);

    let live = !sess.state.is_terminal();
    let running = matches!(sess.state, SessionState::Running | SessionState::WaitingApproval);
    let mut actions = row().gap(8.0).center_y();
    if running {
        let sid = id.to_string();
        actions = actions.child(button("Cancel", k, ButtonVariant::Outline, move |s: &mut App| {
            s.act("Cancel", m::SESSION_CANCEL, json!({"session_id": sid}), Refresh::None)
        }));
    }
    if live {
        let sid = id.to_string();
        actions = actions.child(button("End session", k, ButtonVariant::Ghost, move |s: &mut App| {
            s.act("End session", m::SESSION_END, json!({"session_id": sid}), Refresh::None)
        }));
    }
    let status = row()
        .gap(8.0)
        .center_y()
        .child(badge(state_label(sess.state), sess.state == SessionState::WaitingApproval, k))
        .child(muted(sess.state_reason.clone().unwrap_or_default(), k));
    let composer = if live {
        let mut c = col().key("composer").w_full().gap(8.0).px(16.0).pb(14.0);
        if let Some(ask) = voice_offer(s, k) {
            c = c.child(ask);
        }
        if let Some(chips) = file_chips(s, id, k) {
            c = c.child(chips);
        }
        let uploading = s.files.get(id).is_some_and(|f| f.iter().any(|f| matches!(f.state, crate::model::FileState::Uploading)));
        let left = if s.host_has_media() { media_controls(s, k) } else { row() };
        c.child(
            TextInput::new(&s.prompt)
                .width(None)
                .placeholder("Message the agent — Enter to send")
                .large()
                .on_edit(|s: &mut App| &mut s.prompt)
                .on_submit(|s: &mut App, _: &mut Tx| s.send_prompt())
                .build(k),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(left)
                .child(status)
                .child(el().grow(1.0))
                .child(actions)
                .child(Button::new("Send").variant(ButtonVariant::Submit).disabled(uploading).on_press(|s: &mut App| s.send_prompt()).build(k)),
        )
    } else {
        col().key("composer").w_full().px(16.0).pb(14.0).child(status)
    };
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .on_drop_files(|s: &mut App, files: &[std::path::PathBuf], _: &mut Tx| s.attach(files))
        .child(transcript)
        .child(composer)
}

/// Attach and microphone buttons, or the recording strip while recording.
fn media_controls(s: &App, k: &Kit) -> El {
    use crate::model::Mic;
    match &s.mic {
        Mic::Recording(r) => {
            let secs = r.seconds();
            let level = (s.mic_level * 4.0).clamp(0.05, 1.0);
            row()
                .key("recording")
                .gap(8.0)
                .center_y()
                .child(IconButton::new(&STOP).variant(IconButtonVariant::Contrast).label("Stop and transcribe").on_press(|s: &mut App| s.mic_press()).build(k))
                .child(el().w(8.0).h(8.0).radius(4.0).fill(k.c.danger.fg))
                .child(text(format!("{}:{:02}", secs / 60, secs % 60)).font(k.ty.ui_medium).color(k.c.text_strong))
                .child(row().w(60.0).h(4.0).radius(2.0).fill(k.c.layer_02).child(el().w(60.0 * level).h(4.0).radius(2.0).fill(k.c.text_base)))
                .child(button("Discard", k, ButtonVariant::GhostMuted, |s: &mut App| s.mic_cancel()))
        }
        Mic::Transcribing => row().key("transcribing").gap(8.0).center_y().child(muted("Transcribing…", k)),
        Mic::Idle => {
            let mic_help = match &s.voice {
                Some(v) if v.ready => "Voice input: click, talk, click again",
                Some(v) if v.progress.is_some() => "Voice input: the speech model is downloading",
                _ => "Voice input (downloads a speech model once)",
            };
            row()
                .key("media")
                .gap(2.0)
                .center_y()
                .child(
                    IconButton::new(&CLIP)
                        .variant(IconButtonVariant::Ghost)
                        .label("Attach files")
                        .on_press_tx(|_: &mut App, tx: &mut Tx| {
                            let options = rune::FileDialogOptions { title: "Attach a file".into(), ..Default::default() };
                            tx.open_file(options, |s: &mut App, result, _| match result {
                                Ok(Some(p)) => s.attach(&[p]),
                                Ok(None) => {}
                                Err(e) => s.error = Some(format!("Could not open the file picker: {e}")),
                            });
                        })
                        .build(k),
                )
                .child(IconButton::new(&MIC).variant(IconButtonVariant::Ghost).label(mic_help).on_press(|s: &mut App| s.mic_press()).build(k))
        }
    }
}

/// The one-time question before the speech model downloads, then its progress.
fn voice_offer(s: &App, k: &Kit) -> Option<El> {
    let v = s.voice.as_ref()?;
    if let Some(p) = &v.progress {
        let pct = p.received * 100 / p.total.max(1);
        return Some(muted(format!("Downloading the speech model for voice input… {pct}% of {} MB", p.total / 1_000_000), k).key("voice-progress"));
    }
    if !s.ask_voice_download {
        return None;
    }
    Some(
        card(k)
            .key("voice-offer")
            .child(label("Download the speech model?", k))
            .child(muted_wrap(
                format!(
                    "Voice input turns speech into text on this computer with {} ({} MB, downloaded once). Your recordings never leave your machines. Phones paired with this host use it too.",
                    v.model,
                    v.download_bytes / 1_000_000
                ),
                k,
            ))
            .child(
                row()
                    .gap(8.0)
                    .child(button("Download", k, ButtonVariant::Submit, |s: &mut App| s.download_voice()))
                    .child(button("Not now", k, ButtonVariant::Ghost, |s: &mut App| s.ask_voice_download = false)),
            ),
    )
}

/// Files waiting to go with the next message.
fn file_chips(s: &App, id: &str, k: &Kit) -> Option<El> {
    use crate::model::FileState;
    let files = s.files.get(id).filter(|f| !f.is_empty())?;
    let mut r = row().key("files").w_full().gap(6.0).wrap();
    for f in files {
        let lid = f.local_id;
        let (note, color) = match &f.state {
            FileState::Uploading => ("uploading…".to_string(), k.c.text_faint),
            FileState::Ready(a) => (human_size(a.size), k.c.text_faint),
            FileState::Failed(e) => (format!("failed: {e}"), k.c.danger.fg),
        };
        r = r.child(
            row()
                .key(format!("file-{lid}"))
                .gap(6.0)
                .center_y()
                .pl(10.0)
                .pr(2.0)
                .py(2.0)
                .radius(k.radius.md)
                .fill(k.c.layer_02)
                .border(1.0, k.c.border_weak)
                .child(text(clip(&f.name, 40)).font(k.ty.small).color(k.c.text_strong))
                .child(text(note).font(k.ty.small).color(color))
                .child(IconButton::new(&CLOSE).variant(IconButtonVariant::GhostMuted).label("Remove").on_press(move |s: &mut App| s.remove_file(lid)).build(k)),
        );
    }
    Some(r)
}

/// A prompt bubble's text and attached file names.
fn prompt_body(p: &Value, k: &Kit) -> Vec<El> {
    let mut v = vec![];
    let t = p["text"].as_str().unwrap_or("");
    if !t.is_empty() {
        v.push(text(t.to_string()).font(k.ty.body).color(k.c.text_strong).wrap().selectable(true));
    }
    for a in p["attachments"].as_array().into_iter().flatten() {
        let name = a["name"].as_str().unwrap_or("file");
        let size = a["size"].as_u64().map(human_size).unwrap_or_default();
        v.push(
            row()
                .gap(6.0)
                .center_y()
                .child(rune::icon(&CLIP, 14.0).color(k.c.text_faint))
                .child(text(format!("{name} · {size}")).font(k.ty.small).color(k.c.text_base).wrap()),
        );
    }
    v
}

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} B"),
    }
}

fn event_row(s: &App, sess: &Session, e: &Event, results: &std::collections::HashMap<&str, &Event>, k: &Kit) -> Option<El> {
    let p = &e.payload;
    let str_of = |v: &Value| v.as_str().unwrap_or("").to_string();
    Some(match e.kind {
        EventType::Prompt => row().w_full().child(el().grow(1.0)).child(
            col()
                .max_w(560.0)
                .px(14.0)
                .py(10.0)
                .radius(k.radius.lg)
                .fill(k.c.layer_02)
                .gap(6.0)
                .children(prompt_body(p, k)),
        ),
        EventType::AgentMessage => reply(p["text"].as_str().unwrap_or(""), false, k),
        EventType::AgentThought => text(clip(&str_of(&p["text"]), 600)).font(k.ty.small).color(k.c.text_faint).wrap(),
        EventType::ToolCall => {
            let cid = str_of(&p["tool_call_id"]);
            let res = results.get(cid.as_str());
            let status = res.map(|r| str_of(&r.payload["status"])).unwrap_or_else(|| "running".into());
            let mark = match status.as_str() {
                "completed" => "✓",
                "running" => "…",
                "denied" => "⊘",
                _ => "✗",
            };
            let mcp = p["source"] == "mcp";
            let name = if mcp { format!("{} / {}", str_of(&p["connector_name"]), str_of(&p["name"])) } else { str_of(&p["name"]) };
            let open = s.expanded.contains(&cid);
            let toggle_id = cid.clone();
            let mut c = col().w_full().radius(k.radius.md).border(1.0, k.c.border_weak).fill(k.c.layer_01);
            c = c.child(
                row()
                    .w_full()
                    .px(12.0)
                    .py(8.0)
                    .gap(8.0)
                    .center_y()
                    .role("button")
                    .label(format!("Tool call {name}, {status}"))
                    .focusable(true)
                    .hover(k.c.hover)
                    .on_press(move |s: &mut App| {
                        if !s.expanded.remove(&toggle_id) {
                            s.expanded.insert(toggle_id.clone());
                        }
                    })
                    .child(text(mark.to_string()).font(k.ty.ui_medium).color(if mark == "✓" { k.c.success.icon } else if mark == "…" { k.c.text_muted } else { k.c.danger.icon }))
                    .child(text(clip(&name, 120)).font(k.ty.ui_medium).color(k.c.text_base).truncate().grow(1.0))
                    .child(if mcp { badge("MCP", true, k) } else { el() })
                    .child(muted(status.clone(), k))
                    .child(muted(if open { "▾" } else { "▸" }, k)),
            );
            if open {
                let args = match &p["arguments"] {
                    Value::Null => String::new(),
                    v => serde_json::to_string_pretty(v).unwrap_or_default(),
                };
                let mut body = col().w_full().px(12.0).pb(10.0).gap(6.0);
                if !args.is_empty() {
                    body = body.child(muted("Arguments", k)).child(mono(clip(&args, 4000), k));
                }
                if let Some(r) = res {
                    body = body.child(muted("Output", k)).child(mono(clip(&str_of(&r.payload["output"]), 8000), k));
                    if r.payload["sandbox_denied"] == true {
                        body = body.child(text("Blocked by the sandbox".to_string()).font(k.ty.small_medium).color(k.c.warning.icon));
                    }
                }
                c = c.child(body);
            }
            c
        }
        EventType::ToolResult => return None, // shown with its call
        EventType::NetDenied => {
            let host = str_of(&p["host"]);
            let ws = sess.workspace_id.clone();
            let h2 = host.clone();
            row()
                .w_full()
                .px(12.0)
                .py(8.0)
                .gap(10.0)
                .center_y()
                .radius(k.radius.md)
                .fill(k.c.warning.bg).border(1.0, k.c.warning.border)
                .child(text(format!("Network: {}:{} denied — {}", host, p["port"], str_of(&p["reason"]))).font(k.ty.small_medium).color(k.c.text_base).wrap().grow(1.0))
                .child(button(&format!("Allow {}", clip(&host, 40)), k, ButtonVariant::Outline, move |s: &mut App| {
                    s.act("Allow host", m::WORKSPACE_UPDATE, json!({"workspace_id": ws, "add_custom_host": h2}), Refresh::Workspaces);
                    s.notice = Some(format!("{h2} was added to this workspace's network profile."));
                }))
        }
        EventType::ApprovalRequested => {
            let aid = str_of(&p["approval_id"]);
            let pending = s.approvals.iter().any(|a| a.approval_id == aid);
            let mut c = card(k)
                .child(row().gap(8.0).center_y().child(badge("Approval", true, k)).child(label(format!("{} / {}", str_of(&p["connector_name"]), str_of(&p["tool"])), k)))
                .child(mono(clip(&serde_json::to_string_pretty(&p["arguments"]).unwrap_or_default(), 3000), k));
            if pending {
                let (a1, a2) = (aid.clone(), aid.clone());
                c = c.child(
                    row()
                        .gap(8.0)
                        .child(button("Approve", k, ButtonVariant::Submit, move |s: &mut App| {
                            s.act("Approve", m::APPROVAL_RESPOND, json!({"approval_id": a1, "decision": "approve"}), Refresh::Approvals)
                        }))
                        .child(button("Deny", k, ButtonVariant::Danger, move |s: &mut App| {
                            s.act("Deny", m::APPROVAL_RESPOND, json!({"approval_id": a2, "decision": "deny"}), Refresh::Approvals)
                        })),
                );
            }
            c
        }
        EventType::ApprovalResolved => muted(format!("Approval {} by {}", str_of(&p["decision"]), str_of(&p["device"])), k),
        EventType::SessionState => {
            let st = str_of(&p["state"]);
            if !matches!(st.as_str(), "failed" | "ended" | "starting") || (st == "starting" && p["reason"] != "resuming") {
                return None;
            }
            let mut c = col().gap(4.0).child(muted(format!("Session {st}{}", p["reason"].as_str().map(|r| format!(": {r}")).unwrap_or_default()), k));
            if let Some(fix) = p["fix"].as_str() {
                c = c.child(row().gap(6.0).child(muted("Fix:", k)).child(mono(fix.to_string(), k)));
            }
            if let Some(err) = p["stderr"].as_str().filter(|x| !x.is_empty()) {
                c = c.child(mono(clip(err, 2000), k));
            }
            c
        }
        EventType::TurnEnd => {
            let sr = str_of(&p["stop_reason"]);
            if sr == "end_turn" {
                return None;
            }
            muted(format!("Turn {sr}{}", p["host_reason"].as_str().map(|r| format!(" — {r}")).unwrap_or_default()), k)
        }
    })
}

// ---------------------------------------------------------------------------
// New session
// ---------------------------------------------------------------------------

fn page(k: &Kit, key: &str) -> El {
    let _ = k;
    col().key(key.to_string()).w_full().gap(14.0).px(24.0).py(16.0)
}

fn scroll_page(inner: El) -> El {
    col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(inner)
}

fn choice(t: String, sub: String, selected: bool, enabled: bool, k: &Kit, f: impl Fn(&mut App) + 'static) -> El {
    let mut r = col()
        .w_full()
        .px(12.0)
        .py(10.0)
        .gap(2.0)
        .radius(k.radius.md)
        .border(1.0, if selected { k.c.border_focus } else { k.c.border_weak })
        .fill(if selected { k.c.layer_02 } else { k.c.layer_01 })
        .child(text(t).font(k.ty.ui_medium).color(if enabled { k.c.text_base } else { k.c.text_faint }))
        .child(text(sub).font(k.ty.small).color(k.c.text_muted).wrap());
    if enabled {
        r = r.role("button").focusable(true).hover(k.c.hover).on_press(f);
    }
    r
}

fn new_session_screen(s: &App, k: &Kit) -> El {
    let mut c = page(k, "new-session").child(heading("Workspace", k));
    if s.workspaces.is_empty() {
        c = c.child(muted("No workspaces yet. Add a folder on the Workspaces screen first.", k));
    }
    for w in &s.workspaces {
        let id = w.workspace_id.clone();
        c = c.child(choice(
            w.name.clone(),
            format!("{} · {} · {}", w.path, if w.kind == WorkspaceKind::Repo { "repo" } else { "folder" }, w.network.label()),
            s.new_ws.as_deref() == Some(w.workspace_id.as_str()),
            true,
            k,
            move |s: &mut App| s.new_ws = Some(id.clone()),
        ));
    }
    c = c.child(heading("Agent", k));
    let agents = s.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
    for a in agents {
        let mut sub = if a.available {
            format!("Ready{}", a.version.as_ref().map(|v| format!(" · {v}")).unwrap_or_default())
        } else {
            format!("Unavailable: {} — fix: {}", a.problem.clone().unwrap_or_default(), a.fix.clone().unwrap_or_default())
        };
        if let Some(dir) = &a.config_dir {
            sub = format!("{sub} · {dir}");
        }
        let key = login_key(&a);
        let selected = s.new_agent.as_deref() == Some(key.as_str());
        c = c.child(choice(a.label(), sub, selected, a.available, k, move |s: &mut App| s.new_agent = Some(key.clone())));
    }
    c = c.child(muted_wrap("Several subscriptions of one agent? Add a profile per config folder under Host status.", k));
    let ready = s.new_ws.is_some() && s.new_agent.is_some();
    let start = Button::new("Start session")
        .variant(if ready { ButtonVariant::Submit } else { ButtonVariant::Neutral })
        .on_press(|s: &mut App| s.start_session())
        .build(k);
    scroll_page(c.child(row().child(start)))
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

fn workspaces_screen(s: &App, k: &Kit) -> El {
    let add = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            col().grow(1.0).child(
                TextInput::new(&s.ws_path).width(None)
                    .placeholder("/path/to/project")
                    .on_edit(|s: &mut App| &mut s.ws_path)
                    .on_submit(|s: &mut App, _: &mut Tx| add_workspace(s))
                    .build(k),
            ),
        )
        .child(button("Add folder", k, ButtonVariant::Contrast, add_workspace));
    let mut c = page(k, "workspaces").child(heading("Add a workspace", k)).child(muted("A repo or a plain folder agents may work in. Agents can read and change anything inside it.", k)).child(add);
    for w in &s.workspaces {
        c = c.child(workspace_card(s, w, k));
    }
    scroll_page(c)
}

fn add_workspace(s: &mut App) {
    let path = s.ws_path.text.trim().to_string();
    if path.is_empty() {
        return;
    }
    s.act("Add workspace", m::WORKSPACE_ADD, json!({"path": path}), Refresh::Workspaces);
    s.ws_path.clear();
}

fn workspace_card(s: &App, w: &Workspace, k: &Kit) -> El {
    let id = w.workspace_id.clone();
    let preset = |p: NetworkPreset, name: &str| {
        let on = w.network.presets.contains(&p);
        let net = w.network.clone();
        let id = id.clone();
        Switch::new(on)
            .label(name.to_string())
            .on_toggle(move |s: &mut App| {
                let mut net = net.clone();
                if on {
                    net.presets.retain(|x| *x != p);
                } else {
                    net.presets.push(p);
                }
                s.act("Change network profile", m::WORKSPACE_UPDATE, json!({"workspace_id": id, "network": net}), Refresh::Workspaces);
            })
            .build(k)
            .key(format!("{}-{:?}", w.workspace_id, p))
    };
    let mut hosts = row().gap(6.0).flex_wrap();
    for h in &w.network.custom_hosts {
        let (id2, h2) = (id.clone(), h.clone());
        let net = w.network.clone();
        hosts = hosts.child(
            row()
                .gap(4.0)
                .px(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .fill(k.c.layer_02)
                .center_y()
                .child(text(h.clone()).font(k.ty.small_medium).color(k.c.text_base))
                .child(
                    text("×".to_string())
                        .font(k.ty.small_medium)
                        .color(k.c.text_muted)
                        .role("button")
                        .label(format!("Remove {h}"))
                        .on_press(move |s: &mut App| {
                            let mut n = net.clone();
                            n.custom_hosts.retain(|x| *x != h2);
                            s.act("Remove host", m::WORKSPACE_UPDATE, json!({"workspace_id": id2, "network": n}), Refresh::Workspaces);
                        }),
                ),
        );
    }
    let editing = s.host_for.as_deref() == Some(w.workspace_id.as_str());
    let add_host = if editing {
        let idh = id.clone();
        row()
            .w_full()
            .gap(8.0)
            .child(
                col().grow(1.0).child(
                    TextInput::new(&s.custom_host).width(None)
                        .placeholder("api.example.com or *.example.com")
                        .on_edit(|s: &mut App| &mut s.custom_host)
                        .on_submit(move |s: &mut App, _: &mut Tx| {
                            let h = s.custom_host.text.trim().to_string();
                            if !h.is_empty() {
                                s.act("Add host", m::WORKSPACE_UPDATE, json!({"workspace_id": idh, "add_custom_host": h}), Refresh::Workspaces);
                            }
                            s.custom_host.clear();
                            s.host_for = None;
                        })
                        .build(k),
                ),
            )
            .child(button("Done", k, ButtonVariant::Ghost, |s: &mut App| s.host_for = None))
    } else {
        let idh = id.clone();
        row().child(button("Add host", k, ButtonVariant::Ghost, move |s: &mut App| {
            s.host_for = Some(idh.clone());
            s.custom_host = rune::TextEdit::new("");
            s.custom_host.focused = true;
        }))
    };
    let mut conns = col().gap(6.0);
    for c in &s.connectors {
        let on = w.connectors.contains(&c.connector_id);
        let (idc, cid) = (id.clone(), c.connector_id.clone());
        let current = w.connectors.clone();
        conns = conns.child(
            Switch::new(on)
                .label(c.name.clone())
                .on_toggle(move |s: &mut App| {
                    let mut list = current.clone();
                    if on {
                        list.retain(|x| *x != cid);
                    } else {
                        list.push(cid.clone());
                    }
                    s.act("Change connectors", m::WORKSPACE_UPDATE, json!({"workspace_id": idc, "connectors": list}), Refresh::Workspaces);
                })
                .build(k)
                .key(format!("{}-{}", w.workspace_id, c.connector_id)),
        );
    }
    if s.connectors.is_empty() {
        conns = conns.child(muted("No connectors. Add one on the Connectors screen.", k));
    }
    let idr = id.clone();
    card(k)
        .key(format!("ws-{}", w.workspace_id))
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(heading(w.name.clone(), k))
                .child(badge(if w.kind == WorkspaceKind::Repo { "repo" } else { "folder" }, false, k))
                .child(el().grow(1.0))
                .child(button("Remove", k, ButtonVariant::Ghost, move |s: &mut App| {
                    s.act("Remove workspace", m::WORKSPACE_REMOVE, json!({"workspace_id": idr}), Refresh::Workspaces)
                })),
        )
        .child(muted(w.path.clone(), k))
        .child(text("Network".to_string()).font(k.ty.small_medium).color(k.c.text_muted))
        .child(muted(format!("Profile: {}. The agent's own provider hosts are always allowed.", w.network.label()), k))
        .child(row().gap(16.0).child(preset(NetworkPreset::Packages, "Package registries")).child(preset(NetworkPreset::Github, "GitHub")))
        .child(hosts)
        .child(add_host)
        .child(text("Connectors".to_string()).font(k.ty.small_medium).color(k.c.text_muted))
        .child(conns)
}

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

fn connectors_screen(s: &App, k: &Kit) -> El {
    let add = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            col().grow(1.0).child(
                TextInput::new(&s.conn_url)
                    .width(None)
                    .placeholder("Server URL, e.g. https://mcp.sentry.dev/mcp")
                    .on_edit(|s: &mut App| &mut s.conn_url)
                    .on_submit(|s: &mut App, _: &mut Tx| s.add_connector())
                    .build(k),
            ),
        )
        .child(button("Add", k, ButtonVariant::Contrast, |s: &mut App| s.add_connector()));
    let mut c = page(k, "connectors")
        .child(muted("Paste a remote MCP server's URL. If it needs an account, your browser opens so you can sign in. New connectors are on in every workspace; switch one off on the Workspaces screen. For two accounts on one service (work and personal), add it twice and rename each.", k).wrap())
        .child(muted("Read tools are allowed and write tools ask you first. Click a tool's setting to cycle Allow, Ask and Off.", k).wrap())
        .child(add);
    if s.connectors.is_empty() {
        c = c.child(muted("No connectors yet.", k));
    }
    for conn in &s.connectors {
        c = c.child(connector_card(s, conn, k));
    }
    scroll_page(c)
}

fn policy_name(p: ToolPolicy) -> &'static str {
    match p {
        ToolPolicy::Auto => "Allow",
        ToolPolicy::Ask => "Ask",
        ToolPolicy::Disabled => "Off",
    }
}

fn next_policy(p: ToolPolicy) -> ToolPolicy {
    match p {
        ToolPolicy::Auto => ToolPolicy::Ask,
        ToolPolicy::Ask => ToolPolicy::Disabled,
        ToolPolicy::Disabled => ToolPolicy::Auto,
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "ok" => "connected",
        "needs_sign_in" => "needs sign-in",
        "error" => "error",
        _ => "checking",
    }
}

fn connector_card(s: &App, c: &Connector, k: &Kit) -> El {
    let id = c.connector_id.clone();
    let signing = s.signing_in.get(&c.connector_id);
    let renaming = s.renaming.as_deref() == Some(c.connector_id.as_str());

    let title: El = if renaming {
        let idn = id.clone();
        col().grow(1.0).child(
            TextInput::new(&s.rename)
                .width(None)
                .placeholder("e.g. Sentry (work)")
                .on_edit(|s: &mut App| &mut s.rename)
                .on_submit(move |s: &mut App, _: &mut Tx| {
                    let n = s.rename.text.trim().to_string();
                    if !n.is_empty() {
                        s.act("Rename connector", m::CONNECTOR_UPDATE, json!({"connector_id": idn, "name": n}), Refresh::Connectors);
                    }
                    s.renaming = None;
                })
                .build(k),
        )
    } else {
        let badge_el = match signing {
            Some(_) => badge("waiting for browser", true, k),
            None => badge(status_label(&c.status), c.status != "ok", k),
        };
        row().grow(1.0).gap(8.0).center_y().child(heading(c.name.clone(), k)).child(badge_el)
    };
    let mut head = row().w_full().center_y().gap(8.0).child(title);

    if renaming {
        head = head.child(button("Cancel", k, ButtonVariant::Ghost, |s: &mut App| s.renaming = None));
    } else {
        match signing {
            Some(link) if !link.is_empty() => {
                let l = link.clone();
                head = head.child(button("Open browser again", k, ButtonVariant::Ghost, move |s: &mut App| s.open_queue.push(l.clone())));
            }
            Some(_) => {}
            None if c.status == "needs_sign_in" || (c.status == "error" && c.auth == ConnectorAuth::Oauth) => {
                let ids = id.clone();
                head = head.child(button("Sign in", k, ButtonVariant::Contrast, move |s: &mut App| s.sign_in(&ids)));
            }
            None if c.status == "error" => {
                let idf = id.clone();
                head = head.child(button("Retry", k, ButtonVariant::Ghost, move |s: &mut App| {
                    s.act("Refresh", m::CONNECTOR_REFRESH, json!({"connector_id": idf}), Refresh::Connectors)
                }));
            }
            None => {}
        }
        let (idr, nm) = (id.clone(), c.name.clone());
        head = head.child(button("Rename", k, ButtonVariant::Ghost, move |s: &mut App| {
            s.renaming = Some(idr.clone());
            s.rename = rune::TextEdit::new(nm.clone());
            s.rename.focused = true;
        }));
        let ide = id.clone();
        let enabled = c.enabled;
        head = head.child(
            Switch::new(enabled)
                .label("On")
                .on_toggle(move |s: &mut App| s.act("Turn connector on or off", m::CONNECTOR_UPDATE, json!({"connector_id": ide, "enabled": !enabled}), Refresh::Connectors))
                .build(k)
                .key(format!("{}-enabled", c.connector_id)),
        );
        let idr = id.clone();
        head = head.child(button("Remove", k, ButtonVariant::Ghost, move |s: &mut App| {
            s.act("Remove connector", m::CONNECTOR_REMOVE, json!({"connector_id": idr}), Refresh::Connectors)
        }));
    }

    let mut card = card(k)
        .key(format!("conn-{}", c.connector_id))
        .child(head)
        .child(muted(format!("{} · agents see its tools as {}__…", c.url, connector_slug(&c.name)), k));
    if let Some(link) = signing.filter(|l| !l.is_empty()) {
        card = card
            .child(muted("Finish signing in in your browser. This updates by itself when you're done. If no browser opened, open this link:", k).wrap())
            .child(mono(link.clone(), k));
    } else if c.status == "error" {
        if let Some(d) = &c.status_detail {
            card = card.child(text(d.clone()).font(k.ty.small).color(k.c.danger.icon).wrap());
        }
    }
    if c.tools.is_empty() {
        return card;
    }
    let mut tools = col().w_full().gap(2.0);
    for t in &c.tools {
        let (idt, name, next) = (id.clone(), t.name.clone(), next_policy(t.policy));
        tools = tools.child(
            row()
                .w_full()
                .py(4.0)
                .gap(10.0)
                .center_y()
                .child(badge(if t.kind == ToolKind::Read { "read" } else { "write" }, t.kind == ToolKind::Write, k))
                .child(text(t.name.clone()).font(k.ty.ui_medium).color(k.c.text_base))
                .child(text(clip(t.description.as_deref().unwrap_or(""), 90)).font(k.ty.small).color(k.c.text_muted).truncate().grow(1.0))
                .child(button(policy_name(t.policy), k, ButtonVariant::Outline, move |s: &mut App| {
                    s.act("Set tool policy", m::CONNECTOR_UPDATE, json!({"connector_id": idt, "tool_policies": {name.clone(): next}}), Refresh::Connectors)
                })),
        );
    }
    card.child(tools)
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

fn qr(payload: &str, k: &Kit) -> El {
    let Ok(code) = qrcode::QrCode::new(payload.as_bytes()) else { return mono(payload.to_string(), k) };
    let w = code.width();
    let colors = code.to_colors();
    let module = (220.0 / (w + 8) as f32).floor().max(2.0);
    let mut grid = col().padding(module * 4.0).fill(Color::WHITE).radius(k.radius.md).key("qr");
    for y in 0..w {
        let mut r = row();
        for x in 0..w {
            let dark = colors[y * w + x] == qrcode::Color::Dark;
            r = r.child(el().size(module, module).fill(if dark { Color::BLACK } else { Color::WHITE }));
        }
        grid = grid.child(r);
    }
    grid
}

fn devices_screen(s: &App, k: &Kit) -> El {
    let mut c = page(k, "devices").child(heading("Paired devices", k));
    if let Some(p) = &s.pairing {
        let mut pc = card(k).key("pairing");
        match (&p.pending, &p.done) {
            (_, Some(done)) => {
                pc = pc.child(label(done.clone(), k)).child(row().child(button("Close", k, ButtonVariant::Ghost, |s: &mut App| s.pairing = None)));
            }
            (Some(pend), None) => {
                let (a, b) = (p.pairing_id.clone(), p.pairing_id.clone());
                pc = pc
                    .child(label(format!("\u{201c}{}\u{201d} wants to pair.", pend.device_name), k))
                    .child(muted("Check that the phone shows the same code:", k))
                    .child(text(pend.short_code.clone()).font(k.ty.body_medium).color(k.c.text_strong).key("short-code"))
                    .child(
                        row()
                            .gap(8.0)
                            .child(button("Codes match, pair", k, ButtonVariant::Submit, move |s: &mut App| {
                                s.act("Confirm pairing", m::DEVICE_PAIR_CONFIRM, json!({"pairing_id": a, "accept": true}), Refresh::Devices)
                            }))
                            .child(button("Refuse", k, ButtonVariant::Danger, move |s: &mut App| {
                                s.act("Refuse pairing", m::DEVICE_PAIR_CONFIRM, json!({"pairing_id": b, "accept": false}), Refresh::Devices)
                            })),
                    );
            }
            (None, None) => {
                pc = pc
                    .child(label("Scan this with the agenthost app on your phone.", k))
                    .child(muted("The code works once and expires in 2 minutes. Only show it on this machine.", k))
                    .child(row().child(qr(&p.qr, k)))
                    .child(row().child(button("Cancel", k, ButtonVariant::Ghost, |s: &mut App| s.pairing = None)));
            }
        }
        c = c.child(pc);
    } else {
        c = c.child(row().child(button("Pair a device", k, ButtonVariant::Contrast, |s: &mut App| {
            s.call(Tag::PairStart, m::DEVICE_PAIR_START, json!({}));
        })));
    }
    if s.devices.is_empty() {
        c = c.child(muted("No paired devices.", k));
    }
    for d in &s.devices {
        let id = d.device_id.clone();
        let renaming = s.renaming.as_deref() == Some(d.device_id.as_str());
        let name: El = if renaming {
            let idn = id.clone();
            col().grow(1.0).child(
                TextInput::new(&s.rename).width(None)
                    .on_edit(|s: &mut App| &mut s.rename)
                    .on_submit(move |s: &mut App, _: &mut Tx| {
                        let n = s.rename.text.trim().to_string();
                        if !n.is_empty() {
                            s.act("Rename device", m::DEVICE_RENAME, json!({"device_id": idn, "name": n}), Refresh::Devices);
                        }
                        s.renaming = None;
                    })
                    .build(k),
            )
        } else {
            col().grow(1.0).gap(2.0).child(label(d.name.clone(), k)).child(muted(format!("Last seen {}", d.last_seen_at.map(ago).unwrap_or_else(|| "never".into())), k))
        };
        let (idr, idv, nm) = (id.clone(), id.clone(), d.name.clone());
        c = c.child(
            card(k).key(format!("dev-{}", d.device_id)).child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(name)
                    .child(button("Rename", k, ButtonVariant::Ghost, move |s: &mut App| {
                        s.renaming = Some(idr.clone());
                        s.rename = rune::TextEdit::new(nm.clone());
                        s.rename.focused = true;
                    }))
                    .child(button("Revoke", k, ButtonVariant::Danger, move |s: &mut App| {
                        s.act("Revoke device", m::DEVICE_REVOKE, json!({"device_id": idv}), Refresh::Devices)
                    })),
            ),
        );
    }
    scroll_page(c)
}

// ---------------------------------------------------------------------------
// Host status
// ---------------------------------------------------------------------------

/// "Add an agent profile": another login of an agent, kept in its own config folder.
fn profile_form(s: &App, info: &HostInfo, k: &Kit) -> El {
    let mut kinds: Vec<AgentKind> = vec![];
    for a in &info.agents {
        if a.agent.profile_env().is_some() && !kinds.contains(&a.agent) {
            kinds.push(a.agent);
        }
    }
    let mut pick = row().gap(6.0).wrap();
    for kind in kinds {
        let on = s.profile_agent == Some(kind);
        pick = pick.child(button(kind.display_name(), k, if on { ButtonVariant::Contrast } else { ButtonVariant::Ghost }, move |s: &mut App| s.profile_agent = Some(kind)));
    }
    let setting = s
        .profile_agent
        .and_then(|a| a.profile_env())
        .map(|v| format!("The folder the agent's {v} setting points at, e.g. ~/.claude-work. Log in there once (the fix shown after adding)."))
        .unwrap_or_else(|| "Pick the agent, then give the profile a name and its config folder.".into());
    col()
        .w_full()
        .gap(8.0)
        .pt(8.0)
        .child(heading("Add a profile", k))
        .child(muted_wrap("Each profile is another login of an agent (a work and a personal subscription, say). New sessions can pick it; its folder stays hidden from every other session.", k))
        .child(pick)
        .child(
            TextInput::new(&s.profile_name).width(None)
                .placeholder("Name, e.g. Work")
                .on_edit(|s: &mut App| &mut s.profile_name)
                .build(k),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    col().grow(1.0).child(
                        TextInput::new(&s.profile_dir).width(None)
                            .placeholder("~/.claude-work")
                            .on_edit(|s: &mut App| &mut s.profile_dir)
                            .on_submit(|s: &mut App, _: &mut Tx| s.add_profile())
                            .build(k),
                    ),
                )
                .child(
                    Button::new("Choose…")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Regular)
                        .on_press_tx(|_: &mut App, tx: &mut Tx| {
                            let options = rune::FileDialogOptions { title: "The agent's config folder".into(), ..Default::default() };
                            tx.open_folder(options, |s: &mut App, result, _| match result {
                                Ok(Some(p)) => s.profile_dir.replace_text(&p.display().to_string()),
                                Ok(None) => {}
                                Err(e) => s.error = Some(format!("Could not open the folder picker: {e}")),
                            });
                        })
                        .build(k),
                )
                .child(button("Add profile", k, ButtonVariant::Contrast, |s: &mut App| s.add_profile())),
        )
        .child(muted_wrap(setting, k))
}

fn host_screen(s: &App, k: &Kit) -> El {
    let Some(info) = &s.info else { return page(k, "host").child(muted("Waiting for the host…", k)) };
    let ok = |b: bool| badge(if b { "ok" } else { "needs attention" }, !b, k);
    let mut sandbox = card(k)
        .child(row().gap(8.0).center_y().child(heading(format!("Sandbox: {}", info.sandbox.backend), k)).child(ok(info.sandbox.ok)))
        .child(muted(format!("sandbox-runtime {}", info.sandbox.runtime_version.clone().unwrap_or_else(|| "not installed".into())), k));
    for p in &info.sandbox.prerequisites {
        let mut r = row().gap(8.0).center_y().child(text(if p.present { "✓" } else { "✗" }.to_string()).color(if p.present { k.c.success.icon } else { k.c.danger.icon })).child(label(p.name.clone(), k));
        if !p.present {
            r = r.child(mono(p.install.clone().unwrap_or_default(), k));
        }
        sandbox = sandbox.child(r);
    }
    let mut agents = card(k).child(heading("Agents", k));
    for a in &info.agents {
        let mut r = row().w_full().gap(8.0).center_y().child(label(a.label(), k)).child(muted(a.version.clone().unwrap_or_default(), k)).child(el().grow(1.0)).child(ok(a.available));
        if !a.available {
            r = r.child(muted(a.problem.clone().unwrap_or_default(), k));
        }
        if let (Some(id), Some(_)) = (a.profile_id.clone(), &a.profile_name) {
            r = r.child(button("Remove", k, ButtonVariant::Ghost, move |s: &mut App| {
                s.act("Remove profile", m::AGENT_PROFILE_REMOVE, json!({"profile_id": id}), Refresh::HostInfo)
            }));
        }
        agents = agents.child(r);
        if let Some(dir) = &a.config_dir {
            agents = agents.child(row().gap(6.0).child(muted("Config folder:", k)).child(mono(dir.clone(), k)));
        }
        if let Some(fix) = a.fix.as_ref().filter(|_| !a.available) {
            agents = agents.child(row().gap(6.0).child(muted("Fix:", k)).child(mono(fix.clone(), k)));
        }
    }
    agents = agents.child(profile_form(s, info, k));
    let r = &info.relay;
    let relay = card(k)
        .child(row().gap(8.0).center_y().child(heading("Relay", k)).child(badge(if !r.configured { "off" } else if r.connected { "connected" } else { "disconnected" }, r.configured && !r.connected, k)))
        .child(muted(r.url.clone().unwrap_or_else(|| "Not configured. Start the host with --relay-url to pair phones.".into()), k))
        .child(muted(r.error.clone().unwrap_or_default(), k));
    let rerun = row().child(button("Re-run checks", k, ButtonVariant::Contrast, |s: &mut App| s.call(Tag::HostInfo, m::HOST_CHECK, json!({}))));
    scroll_page(
        page(k, "host")
            .child(
                row()
                    .gap(12.0)
                    .center_y()
                    .child(el().w(44.0).h(44.0).pixels(crate::brand::logo()))
                    .child(
                        col()
                            .gap(2.0)
                            .child(heading("AgentHost", k))
                            .child(muted(format!("Host {} · version {} · {} · protocol {}", info.host_id, info.version, info.os, info.protocol_version), k)),
                    ),
            )
            .child(rerun)
            .child(sandbox)
            .child(agents)
            .child(relay),
    )
}
