//! The open session: the transcript in a readable column (prompts, replies as Markdown,
//! thoughts, tool calls, blocked hosts, approvals inline) and the composer under it.

use crate::bridge::Refresh;
use crate::commands::{label as keys, run, Cmd};
use crate::icons as ic;
use crate::look::{Btn, Theme};
use crate::model::{App, FileState, Menu, Mic};
use crate::view::{clip, human_size, is_web_link};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, ScrollSpec, TextStyle, Tx};
use rune_kit::{Kit, Markdown, MarkdownStyle};
use serde_json::{json, Value};
use std::collections::HashMap;

/// "create_issue" → "create issue".
pub fn humanize(tool: &str) -> String {
    tool.replace(['_', '-'], " ")
}

/// An agent's reply rendered as Markdown. `streaming` is for the reply still arriving.
/// Links open in the browser, but only web and mail links: the text comes from the agent.
pub(crate) fn reply(src: &str, streaming: bool, t: &Theme, k: &Kit) -> El {
    let st = MarkdownStyle::from_kit(k)
        .text(t.text)
        .body_font(TextStyle::new(t.fs_msg, 400.0).lh((t.fs_msg * 1.6).round()))
        .inline_code_bg(Color::WHITE.alpha(if t.mac() { 0.08 } else { 0.10 }))
        .code_block(Color::BLACK.alpha(0.22), t.card_border)
        .code_block_font(TextStyle::new(t.fs_mono, 400.0).mono().lh((t.fs_mono * 1.55).round()));
    Markdown::new(src)
        .style(st)
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

/// The readable column, centred.
fn column(t: &Theme, inner: El) -> El {
    row().w_full().px(20.0).justify_center().child(inner.w_full().max_w(t.column))
}

pub fn screen(s: &App, id: &str, t: &Theme, k: &Kit) -> El {
    let Some(sess) = s.session(id) else { return el() };
    let tr = s.transcripts.get(id);
    let events: &[Event] = tr.map(|t| t.events.as_slice()).unwrap_or(&[]);
    let mut results: HashMap<&str, &Event> = HashMap::new();
    for e in events {
        if e.kind == EventType::ToolResult {
            if let Some(cid) = e.payload["tool_call_id"].as_str() {
                results.insert(cid, e);
            }
        }
    }
    let mut lines = col().key("transcript-rows").gap(if t.mac() { 16.0 } else { 14.0 }).py(20.0);
    for (i, e) in events.iter().enumerate() {
        let next_ts = events.get(i + 1).map(|n| n.ts);
        if let Some(r) = event_row(s, sess, e, next_ts, &results, t, k) {
            lines = lines.child(r.key(format!("ev-{}", e.seq)));
        }
    }
    if let Some(live) = tr.map(|t| t.live.as_str()).filter(|l| !l.is_empty()) {
        lines = lines.child(reply(live, true, t, k).key("live"));
    }
    if events.is_empty() {
        lines = lines.child(t.small("No messages yet.").pt(20.0));
    }
    let transcript = col().key("transcript").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: true }).child(column(t, lines));
    col()
        .key("session")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .on_drop_files(|s: &mut App, files: &[std::path::PathBuf], _: &mut Tx| s.attach(files))
        .child(transcript)
        .child(composer(s, sess, t))
}

// ---------------------------------------------------------------------------
// Composer
// ---------------------------------------------------------------------------

fn composer(s: &App, sess: &Session, t: &Theme) -> El {
    let mac = t.mac();
    let id = sess.session_id.as_str();
    let mut area = col().key("composer").w_full().gap(8.0).pb(if mac { 14.0 } else { 16.0 });
    if let Some(r) = sess.state_reason.as_ref().filter(|_| !sess.state.is_terminal()) {
        area = area.child(t.faint(r.clone()));
    }
    if sess.archived_at.is_some() {
        return el();
    }
    if sess.state == SessionState::Failed {
        let reason = sess.state_reason.clone().unwrap_or_else(|| "The agent stopped unexpectedly.".into());
        area = area.child(
            row()
                .key("failed-reason")
                .w_full()
                .gap(10.0)
                .px(12.0)
                .py(10.0)
                .center_y()
                .radius(t.card_radius)
                .fill(t.danger.alpha(0.10))
                .border(if t.mac() { 0.5 } else { 1.0 }, t.danger.alpha(0.35))
                .child(t.dot(t.danger, 8.0))
                .child(col().grow(1.0).min_w(0.0).gap(1.0).child(t.strong("The session failed")).child(t.small(reason).wrap())),
        );
    }
    if sess.state.is_terminal() {
        let msg = format!("This session has {}. Start another with {}.", if sess.state == SessionState::Failed { "failed" } else { "ended" }, keys(s.look, Cmd::NewSession));
        return column(t, area.child(row().w_full().justify_center().py(12.0).child(t.small(msg))));
    }
    if let Some(v) = voice_offer(s, t) {
        area = area.child(v);
    }
    let agent = sess.agent.display_name();
    let input = rune::text_input(&s.prompt, TextStyle::new(t.fs_msg, 400.0).lh((t.fs_msg * 1.45).round()))
        .key("prompt")
        .label(format!("Message to {agent}"))
        .w_full()
        .color(t.text)
        .placeholder(format!("Reply to {agent}…"), if mac { t.text3 } else { t.text2 })
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.prompt)
        .on_submit(|s: &mut App, _: &mut Tx| s.send_prompt())
        // ⌘V / Ctrl+V: copied files and images become attachments; text pastes as text.
        .on_input_paste(|s: &mut App, content: &rune::ClipboardContent, _: &mut Tx| s.host_has_media() && s.paste_into_composer(content))
        // Window shortcuts that a field would otherwise type (Alt+A on Linux).
        .on_input_key(|s: &mut App, ev: &rune::KeyEvent, tx: &mut Tx| {
            // ↑ in an empty composer brings back your last message to edit.
            if ev.key == rune::Key::Up && ev.mods == rune::Mods::default() && s.prompt.text.is_empty() {
                return s.recall_last_prompt();
            }
            // Tab completes the slash command on offer.
            if ev.key == rune::Key::Tab && ev.mods == rune::Mods::default() {
                let side = s.current().is_some_and(|x| x.side_questions);
                if let Some((cmd, _)) = crate::model::slash_suggestions(&s.prompt.text, side).first() {
                    s.prompt.replace_text(format!("{cmd} "));
                    return true;
                }
            }
            (ev.mods.alt || ev.mods.cmd || ev.mods.ctrl) && crate::commands::on_key(s, ev, tx)
        });
    let mut boxc = col().w_full().radius(14.0);
    boxc = if mac {
        boxc.fill(Color::WHITE.alpha(0.06)).border(0.5, Color::WHITE.alpha(0.16)).shadow(6.0, 20.0, Color::BLACK.alpha(0.25))
    } else {
        boxc.fill(t.card).box_shadow(rune::Shadow { spread: 1.0, color: Color::WHITE.alpha(0.06), ..Default::default() })
    };
    if let Some(chips) = file_chips(s, id, t) {
        boxc = boxc.child(chips);
    }
    boxc = boxc.child(col().w_full().px(if mac { 14.0 } else { 16.0 }).pt(if mac { 10.0 } else { 12.0 }).pb(if mac { 4.0 } else { 6.0 }).child(rune::auto_grow(input, 1, 8)));
    let uploading = s.files.get(id).is_some_and(|f| f.iter().any(|f| matches!(f.state, FileState::Uploading)));
    let pickers = config_pickers(s, sess, t);
    // With pickers in the bar the hint makes way for them.
    let hint = if pickers.is_some() { "" } else if mac { "↩ Send · ⇧↩ New line" } else { "Enter to send · Shift+Enter for a new line" };
    let send_size = if mac { 28.0 } else { 34.0 };
    let send = row()
        .key("send")
        .size(send_size, send_size)
        .radius(send_size / 2.0)
        .center()
        .fill(if uploading { t.accent.alpha(0.4) } else { t.accent })
        .role("button")
        .label("Send")
        .shortcut_hint(keys(s.look, Cmd::Send))
        .help(format!("Send ({})", keys(s.look, Cmd::Send)))
        .focusable(true)
        .hover(Color::WHITE.alpha(0.1))
        .on_press(|s: &mut App| s.send_prompt())
        .child(icon(&ic::SEND, if mac { 12.0 } else { 13.0 }).color(Color::WHITE));
    let actions = row()
        .w_full()
        .gap(if mac { 2.0 } else { 4.0 })
        .px(8.0)
        .pt(6.0)
        .pb(8.0)
        .center_y()
        .child(if s.host_has_media() { media_controls(s, t) } else { row() })
        .child(pickers.unwrap_or_else(row))
        .child(el().grow(1.0))
        .child(text(hint.to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 400.0)).color(if mac { t.text3 } else { t.text2 }).mr(10.0))
        .child(send);
    if let Some(sug) = slash_menu(s, sess, t) {
        area = area.child(sug);
    }
    let area = area.child(boxc.child(actions));
    column(t, area)
}

/// "/btw — Ask a side question without interrupting", over the composer while you type "/".
fn slash_menu(s: &App, sess: &Session, t: &Theme) -> Option<El> {
    let list = crate::model::slash_suggestions(&s.prompt.text, sess.side_questions);
    if list.is_empty() {
        return None;
    }
    let mac = t.mac();
    let mut c = col().key("slash-menu").w(if mac { 360.0 } else { 400.0 }).padding(if mac { 5.0 } else { 6.0 }).radius(if mac { 8.0 } else { 12.0 }).role("listbox").label("Commands");
    c = if mac { c.fill(rune::color::hex("#2c2c2e").unwrap_or(Color::BLACK)).border(0.5, Color::WHITE.alpha(0.12)).shadow(6.0, 20.0, Color::BLACK.alpha(0.4)) } else { c.fill(rune::color::hex("#36363a").unwrap_or(Color::BLACK)).shadow(6.0, 20.0, Color::BLACK.alpha(0.4)) };
    for (i, (cmd, what)) in list.iter().enumerate() {
        let cmd = *cmd;
        c = c.child(
            row()
                .key(format!("slash-{}", cmd.trim_start_matches('/')))
                .w_full()
                .h(if mac { 28.0 } else { 36.0 })
                .px(if mac { 8.0 } else { 10.0 })
                .gap(10.0)
                .center_y()
                .radius(if mac { 5.0 } else { 7.0 })
                .role("option")
                .checked(i == 0)
                .label(format!("{cmd}: {what}"))
                .fill(if i == 0 { if mac { t.accent } else { Color::WHITE.alpha(0.08) } } else { Color::TRANSPARENT })
                .on_press(move |s: &mut App| {
                    s.prompt.replace_text(format!("{cmd} "));
                    s.prompt.focused = true;
                })
                .child(text(cmd).font(TextStyle::new(t.fs, 600.0).mono()).color(if i == 0 && mac { Color::WHITE } else { t.text }))
                .child(text(what.to_string()).font(t.font_small()).color(if i == 0 && mac { Color::WHITE.alpha(0.8) } else { t.text2 }).truncate().shrink())
                .child(el().grow(1.0))
                .child(text(if mac { "⇥" } else { "Tab" }).font(t.font_small()).color(if i == 0 && mac { Color::WHITE.alpha(0.7) } else { t.text3 })),
        );
    }
    Some(c)
}

/// The composer's model and effort pickers, from the session's settings; None without any.
fn config_pickers(s: &App, sess: &Session, t: &Theme) -> Option<El> {
    let model = App::config_option(sess, "model");
    let effort = App::config_option(sess, "thought_level");
    if model.is_none() && effort.is_none() {
        return None;
    }
    let ready = !matches!(sess.state, SessionState::Created | SessionState::Starting) && !sess.state.is_terminal();
    let mut r = row().key("config-pickers").gap(if t.mac() { 0.0 } else { 2.0 }).ml(if t.mac() { 4.0 } else { 6.0 }).center_y();
    for (opt, glyph) in [(model, None), (effort, Some(&ic::GAUGE))] {
        if let Some(o) = opt {
            r = r.child(config_button(s, sess, o, glyph, ready, t));
        }
    }
    Some(r)
}

fn config_button(s: &App, sess: &Session, o: &SessionConfigOption, glyph: Option<&'static rune::Icon>, ready: bool, t: &Theme) -> El {
    let mac = t.mac();
    let current = o.options.iter().find(|v| v.value == o.current_value).map(|v| v.name.clone()).unwrap_or_else(|| o.current_value.clone());
    let key = format!("config-{}", o.id);
    let open = s.menu == Some(Menu::Config(o.id.clone()));
    let ink = if ready { if mac { t.text2 } else { t.text } } else { t.text3 };
    let mut b = row()
        .key(key.clone())
        .h(if mac { 24.0 } else { 30.0 })
        .pl(if mac { 7.0 } else { 9.0 })
        .pr(if mac { 5.0 } else { 7.0 })
        .gap(5.0)
        .center_y()
        .radius(if mac { 6.0 } else { 7.0 })
        .role("combobox")
        .label(format!("{}: {current}", o.name))
        .help(o.description.clone().map(|d| format!("{} · {d}", o.name)).unwrap_or_else(|| o.name.clone()))
        .expanded(open)
        .focusable(ready)
        .disabled(!ready);
    if let Some(g) = glyph {
        b = b.child(icon(g, if mac { 12.0 } else { 13.0 }).color(ink));
    }
    b = b.child(text(clip(&current, 28)).font(TextStyle::new(if mac { 12.0 } else { 13.0 }, if mac { 400.0 } else { 600.0 })).color(ink)).child(icon(if mac { &ic::CHEVRON_D } else { &ic::CHEVRON_DOWN }, if mac { 8.0 } else { 11.0 }).color(ink));
    if open {
        b = b.fill(Color::WHITE.alpha(0.1));
    }
    if !ready {
        return b;
    }
    let (sid, opt) = (sess.session_id.clone(), o.clone());
    b.hover(Color::WHITE.alpha(0.08)).press(Color::WHITE.alpha(0.12)).on_press_tx(move |s: &mut App, tx: &mut Tx| {
        if s.look.mac() && rune::popup_menu_supported() {
            let mut menu = rune::PopupMenu::new();
            for v in &opt.options {
                menu = menu.item(rune::PopupItem::new(v.value.clone(), v.name.clone()).checked(v.value == opt.current_value));
            }
            let (sid, id) = (sid.clone(), opt.id.clone());
            tx.popup_menu(menu, key.clone(), move |s: &mut App, picked, _| {
                if let Some(v) = picked {
                    s.set_config(&sid, &id, v);
                }
            });
        } else {
            let m = Menu::Config(opt.id.clone());
            s.menu = if s.menu.as_ref() == Some(&m) { None } else { Some(m) };
        }
    })
}

/// The in-app list for a setting (GNOME, and where there's no native menu), over its button.
pub fn config_menu(t: &Theme, session: &str, o: &SessionConfigOption) -> El {
    let items = o
        .options
        .iter()
        .map(|v| {
            let (sid, id, val) = (session.to_string(), o.id.clone(), v.value.clone());
            let note = v.description.clone().map(|d| clip(&d, 34)).unwrap_or_default();
            Some(crate::view::Item::new(&v.name, note, move |s, _| s.set_config(&sid, &id, &val)).checked(v.value == o.current_value))
        })
        .collect();
    crate::view::menu_box(t, items, 320.0).key("config-menu").anchored(format!("config-{}", o.id), rune::Side::Top, rune::Align::Start, 6.0)
}

/// Attach and microphone buttons, or the recording strip while recording.
fn media_controls(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    match &s.mic {
        Mic::Recording(r) => {
            let secs = r.seconds();
            let level = (s.mic_level * 4.0).clamp(0.05, 1.0);
            row()
                .key("recording")
                .gap(8.0)
                .center_y()
                .child(t.icon_button(&ic::STOP, 14.0, &format!("Stop and Transcribe ({})", keys(s.look, Cmd::Dictate)), |s, _| s.mic_press()))
                .child(t.dot(t.danger, 8.0))
                .child(text(format!("{}:{:02}", secs / 60, secs % 60)).font(TextStyle::new(t.fs, 600.0)).color(t.text))
                .child(row().w(60.0).h(4.0).radius(2.0).fill(Color::WHITE.alpha(0.12)).child(el().w(60.0 * level).h(4.0).radius(2.0).fill(t.text)))
                .child(t.small_button("Discard", |s| s.mic_cancel()))
        }
        Mic::Transcribing => row().key("transcribing").gap(8.0).px(6.0).center_y().child(t.small("Transcribing…")),
        Mic::Idle => {
            let mic_help = match &s.voice {
                Some(v) if v.ready => format!("Dictate ({}): click, talk, click again", keys(s.look, Cmd::Dictate)),
                Some(v) if v.progress.is_some() => "Dictate: the speech model is downloading".to_string(),
                _ => format!("Dictate ({}; downloads a speech model once)", keys(s.look, Cmd::Dictate)),
            };
            row()
                .key("media")
                .gap(if mac { 2.0 } else { 4.0 })
                .center_y()
                .child(t.icon_button(&ic::CLIP, if mac { 15.0 } else { 16.0 }, &format!("Attach Files ({})", keys(s.look, Cmd::Attach)), |s, tx| run(s, Cmd::Attach, tx)).key("attach"))
                .child(t.icon_button(&ic::MIC, if mac { 15.0 } else { 16.0 }, &mic_help, |s, _| s.mic_press()).key("mic"))
        }
    }
}

/// The one-time question before the speech model downloads, then its progress.
fn voice_offer(s: &App, t: &Theme) -> Option<El> {
    let v = s.voice.as_ref()?;
    if let Some(p) = &v.progress {
        let pct = p.received * 100 / p.total.max(1);
        return Some(t.small(format!("Downloading the speech model for voice input… {pct}% of {} MB", p.total / 1_000_000)).key("voice-progress"));
    }
    if !s.ask_voice_download {
        return None;
    }
    Some(
        col()
            .key("voice-offer")
            .w_full()
            .gap(8.0)
            .padding(14.0)
            .radius(t.card_radius)
            .fill(t.card)
            .border(0.5, t.card_border)
            .child(t.strong("Download the speech model?"))
            .child(
                t.small(format!(
                    "Voice input turns speech into text on this computer with {} ({} MB, downloaded once). Your recordings never leave your machines. Phones paired with this host use it too.",
                    v.model,
                    v.download_bytes / 1_000_000
                ))
                .wrap(),
            )
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(el().grow(1.0))
                    .child(t.button("Not Now", Btn::Default, None, |s| s.ask_voice_download = false))
                    .child(t.button("Download", Btn::Primary, None, |s| s.download_voice()).default_button()),
            ),
    )
}

/// Files waiting to go with the next message.
fn file_chips(s: &App, id: &str, t: &Theme) -> Option<El> {
    let files = s.files.get(id).filter(|f| !f.is_empty())?;
    let mut r = row().key("files").w_full().gap(6.0).px(10.0).pt(10.0).flex_wrap();
    for f in files {
        let lid = f.local_id;
        let (note, color) = match &f.state {
            FileState::Uploading => ("uploading…".to_string(), t.text3),
            FileState::Ready(a) => (human_size(a.size), t.text3),
            FileState::Failed(e) => (format!("failed: {e}"), t.danger),
        };
        r = r.child(
            row()
                .key(format!("file-{lid}"))
                .h(30.0)
                .gap(7.0)
                .center_y()
                .pl(8.0)
                .pr(6.0)
                .radius(8.0)
                .fill(Color::WHITE.alpha(0.08))
                .child(icon(&ic::FILE_SM, 13.0).color(t.text2))
                .child(text(clip(&f.name, 40)).font(TextStyle::new(12.0, 400.0)).color(t.text))
                .child(text(note).font(TextStyle::new(12.0, 400.0)).color(color))
                .child(
                    row()
                        .role("button")
                        .label(format!("Remove {}", f.name))
                        .on_press(move |s: &mut App| s.remove_file(lid))
                        .child(icon(&ic::XMARK_CIRCLE, 14.0).color(Color::WHITE.alpha(0.18))),
                ),
        );
    }
    Some(r)
}

// ---------------------------------------------------------------------------
// Transcript rows
// ---------------------------------------------------------------------------

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// A prompt bubble's text and attached file names.
fn prompt_body(p: &Value, t: &Theme) -> Vec<El> {
    let mut v = vec![];
    let tx = p["text"].as_str().unwrap_or("");
    if !tx.is_empty() {
        v.push(text(tx.to_string()).font(TextStyle::new(t.fs, 400.0).lh((t.fs * 1.45).round())).color(t.text).wrap().selectable(true));
    }
    for a in p["attachments"].as_array().into_iter().flatten() {
        let name = a["name"].as_str().unwrap_or("file");
        let size = a["size"].as_u64().map(human_size).unwrap_or_default();
        v.push(
            row()
                .gap(6.0)
                .center_y()
                .child(icon(&ic::FILE_SM, 13.0).color(t.text2))
                .child(text(format!("{name} · {size}")).font(TextStyle::new(12.0, 400.0)).color(t.text2).wrap()),
        );
    }
    v
}

/// Seconds between two event times, "8.4 s" style.
fn secs(from: i64, to: i64) -> Option<String> {
    let ms = to - from;
    (ms > 0).then(|| if ms < 10_000 { format!("{:.1} s", ms as f64 / 1000.0) } else { format!("{} s", ms / 1000) })
}

/// Arguments as aligned `key  value` lines, as the approval card shows them.
fn arg_lines(args: &Value) -> String {
    match args {
        Value::Object(o) if !o.is_empty() => {
            let w = o.keys().map(|k| k.chars().count()).max().unwrap_or(0);
            o.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(_) => clip(&v.to_string(), 300),
                        _ => clip(&serde_json::to_string(v).unwrap_or_default(), 300),
                    };
                    format!("{k:w$}  {v}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Value::Null => String::new(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    }
}

fn pre(t: &Theme, body: String) -> El {
    text(body).font(TextStyle::new(t.fs_mono_small, 400.0).mono().lh((t.fs_mono_small * 1.55).round())).color(t.text2).wrap().selectable(true)
}

fn event_row(s: &App, sess: &Session, e: &Event, next_ts: Option<i64>, results: &HashMap<&str, &Event>, t: &Theme, k: &Kit) -> Option<El> {
    let mac = t.mac();
    let p = &e.payload;
    Some(match e.kind {
        EventType::Prompt => {
            let device = str_of(&p["device"]);
            let mut c = col().items_end().gap(4.0).max_w(520.0).child(
                col()
                    .px(13.0)
                    .py(9.0)
                    .radius(16.0)
                    .fill(Color::WHITE.alpha(0.09))
                    .gap(6.0)
                    .children(prompt_body(p, t)),
            );
            if !device.is_empty() && device != "local" {
                c = c.child(text(format!("From {device} · {}", crate::view::short_age(e.ts, s.look))).font(TextStyle::new(t.fs_small, 400.0)).color(t.text3));
            }
            row().w_full().child(el().grow(1.0)).child(c)
        }
        EventType::AgentMessage => reply(p["text"].as_str().unwrap_or(""), false, t, k),
        EventType::AgentThought => {
            let key = format!("th-{}", e.seq);
            let open = s.expanded.contains(&key);
            let label = match next_ts.and_then(|n| ((n - e.ts) / 1000 > 0).then_some((n - e.ts) / 1000)) {
                Some(n) => format!("Thought for {n} second{}", if n == 1 { "" } else { "s" }),
                None => "Thought".to_string(),
            };
            let k2 = key.clone();
            let mut c = col().w_full().gap(6.0).child(
                row()
                    .gap(if mac { 6.0 } else { 8.0 })
                    .center_y()
                    .role("button")
                    .label(label.clone())
                    .focusable(true)
                    .on_press(move |s: &mut App| {
                        if !s.expanded.remove(&k2) {
                            s.expanded.insert(k2.clone());
                        }
                    })
                    .child(icon(if open { &ic::CHEVRON_D } else { &ic::CHEVRON_R }, 10.0).color(t.text2))
                    .child(text(label).font(TextStyle::new(if mac { 12.0 } else { 13.5 }, 400.0)).color(t.text2)),
            );
            if open {
                c = c.child(text(clip(&str_of(&p["text"]), 4000)).font(t.font_small()).color(t.text2).wrap().selectable(true).pl(16.0));
            }
            c
        }
        EventType::ToolCall => tool_call(s, e, results, t),
        EventType::ToolResult => return None, // shown with its call
        // Side questions live in their own pane.
        EventType::SideQuestion | EventType::SideMessage | EventType::SideEnd => return None,
        EventType::NetDenied => {
            let host = str_of(&p["host"]);
            let ws = sess.workspace_id.clone();
            let ws_name = s.workspace(&sess.workspace_id).map(|w| w.name.clone()).unwrap_or_else(|| "this workspace".into());
            let allowed = s.workspace(&sess.workspace_id).is_some_and(|w| w.network.custom_hosts.contains(&host) || w.network.presets.contains(&NetworkPreset::Web));
            let h2 = host.clone();
            let mut r = row()
                .w_full()
                .gap(if mac { 10.0 } else { 12.0 })
                .center_y()
                .radius(t.card_radius)
                .px(if mac { 12.0 } else { 14.0 })
                .py(if mac { 9.0 } else { 10.0 })
                .fill(t.warn_tint)
                .child(icon(&ic::WARN, 16.0).color(if mac { t.warn } else { rune::color::hex("#f8e45c").unwrap_or(t.warn) }))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(1.0)
                        .child(text(format!("Blocked {host}")).font(TextStyle::new(t.fs, if mac { 500.0 } else { 400.0 })).color(t.text).wrap())
                        .child(t.small(if mac { "This workspace's network rules don't include it." } else { "Not in this workspace's network rules" }).wrap()),
                );
            if mac {
                r = r.border(0.5, t.warn_border);
            }
            if allowed {
                r.child(t.small("Allowed"))
            } else {
                r.child(t.small_button(&format!("Allow for {}", clip(&ws_name, 24)), move |s: &mut App| {
                    s.act("Allow host", m::WORKSPACE_UPDATE, json!({"workspace_id": ws, "add_custom_host": h2}), Refresh::Workspaces);
                    s.notice = Some(format!("{h2} is now allowed in {ws_name}."));
                }))
            }
        }
        EventType::ApprovalRequested => approval_card(s, sess, e, t)?,
        EventType::ApprovalResolved => {
            let decision = str_of(&p["decision"]);
            let by = str_of(&p["device"]);
            let word = match decision.as_str() {
                "approve" | "approved" => "Approved",
                "deny" | "denied" => "Denied",
                d => return Some(t.faint(format!("Request {d}"))),
            };
            let by = if p["timed_out"] == true { " (timed out)".to_string() } else if by.is_empty() || by == "local" { String::new() } else { format!(" from {by}") };
            t.faint(format!("{word}{by}"))
        }
        EventType::SessionState => {
            let st = str_of(&p["state"]);
            if !matches!(st.as_str(), "failed" | "ended" | "starting") || (st == "starting" && p["reason"] != "resuming") {
                return None;
            }
            let mut c = col().w_full().items_center().gap(4.0).child(t.faint(format!("Session {st}{}", p["reason"].as_str().map(|r| format!(": {r}")).unwrap_or_default())));
            if let Some(fix) = p["fix"].as_str() {
                c = c.child(row().gap(6.0).child(t.faint("Fix:")).child(t.mono(fix.to_string())));
            }
            if let Some(err) = p["stderr"].as_str().filter(|x| !x.is_empty()) {
                c = c.child(pre(t, clip(err, 2000)));
            }
            c
        }
        EventType::TurnEnd => {
            let sr = str_of(&p["stop_reason"]);
            if sr == "end_turn" {
                return None;
            }
            t.faint(format!("Turn {sr}{}", p["host_reason"].as_str().map(|r| format!(" — {r}")).unwrap_or_default()))
        }
    })
}

fn tool_call(s: &App, e: &Event, results: &HashMap<&str, &Event>, t: &Theme) -> El {
    let mac = t.mac();
    let p = &e.payload;
    let cid = str_of(&p["tool_call_id"]);
    let res = results.get(cid.as_str());
    let status = res.map(|r| str_of(&r.payload["status"])).unwrap_or_else(|| "running".into());
    let mcp = p["source"] == "mcp";
    let name = if mcp { format!("{} · {}", str_of(&p["connector_name"]), str_of(&p["name"])) } else { str_of(&p["name"]) };
    let kind_icon: &'static rune::Icon = match p["kind"].as_str().unwrap_or("") {
        _ if mcp => &ic::PLUG_SM,
        "edit" | "delete" | "move" | "read" => &ic::DOC,
        "fetch" => &ic::GLOBE,
        "search" => &ic::SEARCH,
        _ => &ic::TERMINAL,
    };
    let dur = res.and_then(|r| secs(e.ts, r.ts));
    let open = s.expanded.contains(&cid);
    let toggle = cid.clone();
    let (mark, mark_c): (El, Color) = match status.as_str() {
        "completed" => (icon(&ic::CHECK, 14.0), t.ok),
        "running" | "pending" | "in_progress" => (icon(&ic::SPINNER, 13.0).looping(rune::LoopSpec::rotation(0.9)), t.text2),
        _ => (icon(&ic::CROSS, 13.0), t.danger),
    };
    let word = match status.as_str() {
        "completed" => "Passed",
        "running" | "pending" | "in_progress" => "Running",
        "denied" => "Denied",
        _ => "Failed",
    };
    let mono_name = text(clip(&name, 140)).font(TextStyle::new(t.fs_mono, 400.0).mono()).color(t.text).truncate();
    let mut head = row()
        .w_full()
        .center_y()
        .role("button")
        .label(format!("Tool call {name}, {status}"))
        .focusable(true)
        .hover(Color::WHITE.alpha(0.03))
        .on_press(move |s: &mut App| {
            if !s.expanded.remove(&toggle) {
                s.expanded.insert(toggle.clone());
            }
        });
    if mac {
        head = head
            .gap(9.0)
            .px(12.0)
            .py(if open { 8.0 } else { 7.0 })
            .child(icon(if open { &ic::CHEVRON_D } else { &ic::CHEVRON_R }, 10.0).color(t.text2))
            .child(icon(kind_icon, 14.0).color(t.text2))
            .child(mono_name.shrink().min_w(0.0))
            .child(el().grow(1.0));
        if let Some(d) = &dur {
            head = head.child(text(d.clone()).font(TextStyle::new(11.0, 400.0)).color(t.text3));
        }
        head = head.child(mark.color(mark_c));
    } else {
        let sub = match &dur {
            Some(d) => format!("{word} · {d}"),
            None => word.to_string(),
        };
        head = head
            .gap(12.0)
            .px(14.0)
            .py(if open { 12.0 } else { 10.0 })
            .child(icon(kind_icon, 16.0).color(t.text))
            .child(col().grow(1.0).min_w(0.0).child(mono_name).child(text(sub).font(t.font_small()).color(if word == "Failed" || word == "Denied" { t.danger } else { t.text2 })))
            .child(icon(if open { &ic::CHEVRON_UP } else { &ic::CHEVRON_DOWN }, 14.0).color(t.text));
    }
    let mut c = col().w_full().radius(t.card_radius).fill(t.card).clip_rounded(t.card_radius).child(head);
    if mac {
        c = c.border(0.5, t.card_border);
    }
    if open {
        let args = match &p["arguments"] {
            Value::Null => String::new(),
            v => serde_json::to_string_pretty(v).unwrap_or_default(),
        };
        let mut body = col().w_full().gap(8.0).pl(if mac { 42.0 } else { 42.0 }).pr(14.0).pt(10.0).pb(12.0).fill(t.code_bg).border_sides([if mac { 0.5 } else { 1.0 }, 0.0, 0.0, 0.0], if mac { t.sep } else { Color::BLACK.alpha(0.25) });
        let output = res.map(|r| str_of(&r.payload["output"])).unwrap_or_default();
        let is_command = p["kind"] == "execute";
        if !args.is_empty() && !(is_command && !output.is_empty()) {
            body = body.child(pre(t, clip(&args, 4000)));
        }
        if !output.is_empty() {
            body = body.child(pre(t, clip(&output, 8000)));
        }
        if res.is_some_and(|r| r.payload["sandbox_denied"] == true) {
            body = body.child(text("Blocked by the sandbox").font(TextStyle::new(t.fs_small, 600.0)).color(t.warn));
        }
        c = c.child(body);
    }
    c
}

/// An agent asking to install a tool on this computer (the host's own
/// `install_tool`): the summary, why, the exact command, and where it runs.
fn install_card(s: &App, agent: &str, pending: &PendingApproval, t: &Theme) -> El {
    let mac = t.mac();
    let a = &pending.arguments;
    let command = a["command"].as_str().unwrap_or_default().to_string();
    let reason = a["reason"].as_str().unwrap_or_default().trim().to_string();
    let title = pending.summary.clone().unwrap_or_else(|| format!("Install a tool: {command}"));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let mins = ((pending.expires_at - now) / 60_000).max(0);
    let expires = if mins >= 1 { format!("expires in {mins} min") } else { "expires soon".into() };
    let sub = format!("{agent} asks · runs as you, outside the sandbox · {expires}");
    let aid = pending.approval_id.clone();
    let (a1, a2) = (aid.clone(), aid);
    let deny = t.button("Deny", Btn::Default, Some(&keys(s.look, Cmd::Deny)), move |s| s.respond(&a2, false)).key("deny");
    let approve = t.button("Install", Btn::Primary, Some(&keys(s.look, Cmd::Approve)), move |s| s.respond(&a1, true)).key("approve");
    let (deny, approve) = if mac { (deny, approve) } else { (deny.radius(17.0).px(18.0), approve.radius(17.0).px(18.0)) };
    let mut head = row().w_full().gap(10.0).center_y();
    if mac {
        head = head.child(row().size(28.0, 28.0).radius(7.0).center().fill(t.warn.alpha(0.18)).child(icon(&ic::TERMINAL, 15.0).color(t.warn)));
    }
    head = head.child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(if mac { 1.0 } else { 2.0 })
            .child(text(title).font(TextStyle::new(t.fs, t.w_bold)).color(t.text).wrap())
            .child(text(sub).font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 400.0)).color(t.text2).wrap()),
    );
    let mut card = col()
        .key("approval-card")
        .w_full()
        .gap(if mac { 10.0 } else { 12.0 })
        .radius(12.0)
        .padding(if mac { 14.0 } else { 16.0 })
        .label(format!("{agent} asks to run {command} on this computer"))
        .child(head);
    card = if mac {
        card.px(16.0).fill(Color::WHITE.alpha(0.05)).border(0.5, t.warn.alpha(0.45)).box_shadow(rune::Shadow { spread: 3.0, color: t.warn.alpha(0.08), ..Default::default() })
    } else {
        card.fill(t.card).box_shadow(rune::Shadow { spread: 1.0, inset: true, color: t.warn.alpha(0.6), ..Default::default() })
    };
    if !reason.is_empty() {
        card = card.child(text(format!("“{reason}”")).font(t.font()).color(t.text).wrap());
    }
    card = card.child(col().w_full().px(11.0).py(9.0).radius(if mac { 7.0 } else { 8.0 }).fill(Color::BLACK.alpha(0.25)).child(pre(t, format!("$ {command}"))));
    card = card.child(
        text("The package manager may run the package's install scripts with your permissions. Install only what you expected.")
            .font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 400.0))
            .color(t.text2)
            .wrap(),
    );
    card.child(row().w_full().gap(if mac { 8.0 } else { 10.0 }).child(el().grow(1.0)).child(deny).child(approve))
}

/// An approval waiting inline: what the agent wants, its arguments, Deny and Approve.
fn approval_card(s: &App, sess: &Session, e: &Event, t: &Theme) -> Option<El> {
    let mac = t.mac();
    let p = &e.payload;
    let aid = str_of(&p["approval_id"]);
    let pending = s.approvals.iter().find(|a| a.approval_id == aid);
    let conn = str_of(&p["connector_name"]);
    let tool = str_of(&p["tool"]);
    let Some(pending) = pending else {
        return Some(t.faint(format!("Asked to use {conn} · {tool}")));
    };
    let agent = s.login_label(sess);
    let agent_short = sess.agent.display_name();
    if pending.connector_id == BUILTIN_CONNECTOR_ID {
        return Some(install_card(s, &agent, pending, t));
    }
    // What the connector says about the tool.
    let info = s.connectors.iter().find(|c| c.connector_id == pending.connector_id).and_then(|c| c.tools.iter().find(|x| x.name == tool));
    let kind = match info.map(|x| x.kind) {
        Some(ToolKind::Read) => if mac { "read tool" } else { "reads" },
        _ => if mac { "write tool" } else { "changes things" },
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let mins = ((pending.expires_at - now) / 60_000).max(0);
    let expires = if mins >= 1 { format!("expires in {mins} min") } else { "expires soon".into() };
    let sub = if mac { format!("{tool} · {kind} · asks every time · {expires}") } else { format!("{tool} · {kind} · {expires}") };
    let args = arg_lines(if p["arguments"].is_null() { &pending.arguments } else { &p["arguments"] });
    let (a1, a2) = (aid.clone(), aid.clone());
    let deny = t.button("Deny", Btn::Default, Some(&keys(s.look, Cmd::Deny)), move |s| s.respond(&a2, false)).key("deny");
    let approve = t.button("Approve", Btn::Primary, Some(&keys(s.look, Cmd::Approve)), move |s| s.respond(&a1, true)).key("approve");
    let (deny, approve) = if mac { (deny, approve) } else { (deny.radius(17.0).px(18.0), approve.radius(17.0).px(18.0)) };
    let title = if mac { format!("{agent_short} wants to {} on {conn}", humanize(&tool)) } else {
        let h = humanize(&tool);
        let mut cs = h.chars();
        let cap = cs.next().map(|c| c.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default();
        format!("{cap} on {conn}?")
    };
    let mut head = row().w_full().gap(10.0).center_y();
    if mac {
        let logo = if conn.to_lowercase().contains("github") {
            row().size(28.0, 28.0).radius(7.0).center().fill(rune::color::hex("#24292f").unwrap_or(Color::BLACK)).child(icon(&ic::GITHUB, 16.0).color(Color::WHITE))
        } else {
            t.badge_letter(conn.chars().next().unwrap_or('?').to_ascii_uppercase(), rune::color::hex("#3a3a3c").unwrap_or(Color::BLACK), Color::WHITE, 28.0)
        };
        head = head.child(logo);
    }
    head = head.child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(if mac { 1.0 } else { 2.0 })
            .child(text(title).font(TextStyle::new(t.fs, t.w_bold)).color(t.text).wrap())
            .child(text(sub).font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 400.0)).color(t.text2).wrap()),
    );
    let mut card = col()
        .key("approval-card")
        .w_full()
        .gap(if mac { 10.0 } else { 12.0 })
        .radius(12.0)
        .padding(if mac { 14.0 } else { 16.0 })
        .label(format!("{agent} asks to use {conn} {tool}"))
        .child(head);
    card = if mac {
        card.px(16.0).fill(Color::WHITE.alpha(0.05)).border(0.5, t.warn.alpha(0.45)).box_shadow(rune::Shadow { spread: 3.0, color: t.warn.alpha(0.08), ..Default::default() })
    } else {
        card.fill(t.card).box_shadow(rune::Shadow { spread: 1.0, inset: true, color: t.warn.alpha(0.6), ..Default::default() })
    };
    if !args.is_empty() {
        card = card.child(col().w_full().px(11.0).py(9.0).radius(if mac { 7.0 } else { 8.0 }).fill(Color::BLACK.alpha(0.25)).child(pre(t, clip(&args, 3000))));
    }
    Some(card.child(row().w_full().gap(if mac { 8.0 } else { 10.0 }).child(el().grow(1.0)).child(deny).child(approve)))
}
