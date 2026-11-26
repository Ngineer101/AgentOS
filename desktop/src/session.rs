//! The open session: the transcript in a readable column (prompts, replies as Markdown,
//! thoughts, tool calls, blocked hosts, approvals inline) and the composer under it.

use crate::look::MEDIUM;
use crate::bridge::Refresh;
use crate::commands::{label as keys, run, Cmd};
use crate::icons as ic;
use crate::look::{Btn, Theme};
use crate::model::{App, FileState, Menu, Mic, Screen, SentImage};
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
    // Subagents started together share a row of cards; what they do stays in their panel.
    let mut agents: Vec<String> = vec![];
    let mut agents_key = String::new();
    let flush = |lines: El, agents: &mut Vec<String>, key: &str| {
        if agents.is_empty() {
            return lines;
        }
        let g = crate::agents::group(s, id, agents, t).key(key.to_string());
        agents.clear();
        lines.child(g)
    };
    // Tool calls and thoughts in a row fold into one line; see `activity`.
    let mut run: Vec<(&Event, Option<i64>)> = vec![];
    let fold = |lines: El, run: &mut Vec<(&Event, Option<i64>)>| {
        if run.is_empty() {
            return lines;
        }
        let rows = activity(s, sess, run, &results, t, k);
        run.clear();
        rows.into_iter().fold(lines, |l, r| l.child(r))
    };
    for (i, e) in events.iter().enumerate() {
        if crate::model::is_child_event(e) {
            continue;
        }
        if e.kind == EventType::ToolCall && crate::model::is_subagent_call(&e.payload) {
            lines = fold(lines, &mut run);
            if agents.is_empty() {
                agents_key = format!("ev-{}", e.seq);
            }
            agents.push(str_of(&e.payload["tool_call_id"]));
            continue;
        }
        let next_ts = events.get(i + 1).map(|n| n.ts);
        if matches!(e.kind, EventType::ToolCall | EventType::AgentThought) {
            lines = flush(lines, &mut agents, &agents_key);
            run.push((e, next_ts));
            continue;
        }
        if let Some(r) = event_row(s, sess, e, next_ts, &results, t, k) {
            lines = flush(lines, &mut agents, &agents_key);
            lines = fold(lines, &mut run);
            lines = lines.child(r.key(format!("ev-{}", e.seq)));
        }
    }
    lines = flush(lines, &mut agents, &agents_key);
    lines = fold(lines, &mut run);
    let live = tr.map(|t| t.live.as_str()).filter(|l| !l.is_empty());
    if let Some(live) = live {
        lines = lines.child(reply(live, true, t, k).key("live"));
    }
    let pending = tr.map(|t| t.pending.as_slice()).unwrap_or(&[]);
    for p in pending {
        lines = lines.child(row().w_full().key(format!("pending-{}", p.prompt_id)).child(el().grow(1.0)).child(prompt_column(s, &p.payload, t)));
    }
    if live.is_none() {
        if let Some(label) = crate::model::quiet_work(sess.state, events, !pending.is_empty()) {
            lines = lines.child(working_row(label, t).key("working"));
        }
    }
    if events.is_empty() && pending.is_empty() {
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
        .placeholder(if matches!(s.mic, Mic::Recording(_)) { "Listening…".to_string() } else { format!("Reply to {agent}…") }, if mac { t.text3 } else { t.text2 })
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
            // Escape throws away a dictation in progress.
            if ev.key == rune::Key::Escape && ev.mods == rune::Mods::default() && matches!(s.mic, Mic::Recording(_)) {
                s.mic_cancel();
                return true;
            }
            // Escape closes the agent panel.
            if ev.key == rune::Key::Escape && ev.mods == rune::Mods::default() && s.agent_visible() && s.menu.is_none() {
                s.close_agent();
                return true;
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
    // One rounded container: files, the text, then the controls. Where the work lands (the
    // workspace and branch) is the header's subtitle, not the composer's.
    let mut boxc = col().key("composer-box").w_full().radius(if mac { 18.0 } else { 12.0 });
    boxc = if mac {
        boxc.fill(Color::WHITE.alpha(0.055)).border(0.5, Color::WHITE.alpha(0.15)).shadow(6.0, 20.0, Color::BLACK.alpha(0.22))
    } else {
        // libadwaita's .card: a raised fill and a 1 px shade, no border line.
        boxc.fill(t.card).box_shadow(rune::Shadow { spread: 1.0, color: Color::BLACK.alpha(0.3), ..Default::default() })
    };
    let pad_x = if mac { 14.0 } else { 16.0 };
    let chips = file_chips(s, id, t);
    let text_top = match (chips.is_some(), mac) {
        (true, true) => 8.0,
        (true, false) => 10.0,
        (false, true) => 12.0,
        (false, false) => 14.0,
    };
    if let Some(chips) = chips {
        boxc = boxc.child(chips.px(pad_x - 4.0));
    }
    boxc = boxc.child(col().w_full().px(pad_x).pt(text_top).pb(if mac { 2.0 } else { 4.0 }).child(rune::auto_grow(input, 1, 8)));
    let chip = model_chip(s, sess, t);
    // Without a model chip the shortcut hint has the room.
    let hint = if matches!(s.mic, Mic::Recording(_)) {
        if mac { "↩ Send · esc Discard" } else { "Enter to send · Esc to discard" }
    } else if chip.is_some() {
        ""
    } else if mac {
        "↩ Send · ⇧↩ New line"
    } else {
        "Enter to send · Shift+Enter for a new line"
    };
    let actions = row()
        .w_full()
        .gap(if mac { 2.0 } else { 4.0 })
        .px(if mac { 8.0 } else { 10.0 })
        .pt(4.0)
        .pb(if mac { 8.0 } else { 10.0 })
        .center_y()
        .child(if s.host_has_media() { media_controls(s, t) } else { row() })
        .child(el().grow(1.0))
        .child(text(hint.to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 400.0)).color(if mac { t.text3 } else { t.text2 }).mr(10.0))
        .child(chip.unwrap_or_else(row))
        .child(send_button(s, sess, t));
    if let Some(sug) = slash_menu(s, sess, t) {
        area = area.child(sug);
    } else if let Some(p) = crate::agents::running_pill(s, id, t) {
        area = area.child(p);
    }
    let area = area.child(boxc.child(actions));
    column(t, area)
}

/// Whether the session can take a setting now (not starting, not over).
fn config_ready(sess: &Session) -> bool {
    !matches!(sess.state, SessionState::Created | SessionState::Starting) && !sess.state.is_terminal()
}

/// The circular send button: high contrast, dimmed with nothing to send. While the agent
/// works and there's nothing to send it stops the turn instead.
fn send_button(s: &App, sess: &Session, t: &Theme) -> El {
    let mac = t.mac();
    let id = sess.session_id.as_str();
    let uploading = s.files.get(id).is_some_and(|f| f.iter().any(|f| matches!(f.state, FileState::Uploading)));
    let has_files = s.files.get(id).is_some_and(|f| !f.is_empty());
    // While dictating there is something to send: the words still coming.
    let empty = s.prompt.text.trim().is_empty() && !has_files && matches!(s.mic, Mic::Idle);
    let working = matches!(sess.state, SessionState::Running | SessionState::WaitingApproval);
    let size = if mac { 28.0 } else { 34.0 };
    let b = row().key("send").size(size, size).radius(size / 2.0).center().role("button").focusable(true);
    if working && empty {
        return b
            .label("Stop the Turn")
            .help(format!("Stop the Turn ({})", keys(s.look, Cmd::Stop)))
            .fill(t.text)
            .hover(Color::BLACK.alpha(0.12))
            .on_press_tx(|s: &mut App, tx: &mut Tx| run(s, Cmd::Stop, tx))
            .child(icon(&ic::STOP_SM, if mac { 10.0 } else { 11.0 }).color(t.content));
    }
    let dim = empty || uploading;
    b.label("Send")
        .shortcut_hint(keys(s.look, Cmd::Send))
        .help(format!("Send ({})", keys(s.look, Cmd::Send)))
        .fill(if dim { Color::WHITE.alpha(if mac { 0.16 } else { 0.18 }) } else { t.text })
        .hover(Color::BLACK.alpha(0.12))
        .on_press(|s: &mut App| s.send_prompt())
        .child(icon(&ic::SEND, if mac { 12.0 } else { 14.0 }).color(if dim { Color::WHITE.alpha(0.45) } else { t.content }))
}

/// "/btw — Ask a side question without interrupting", over the composer while you type "/".
fn slash_menu(s: &App, sess: &Session, t: &Theme) -> Option<El> {
    let list = crate::model::slash_suggestions(&s.prompt.text, sess.side_questions);
    if list.is_empty() {
        return None;
    }
    let mac = t.mac();
    let mut c = col().key("slash-menu").w(if mac { 360.0 } else { 400.0 }).padding(if mac { 5.0 } else { 6.0 }).radius(t.box_radius).role("listbox").label("Commands");
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
                .radius(t.row_radius)
                .role("option")
                .checked(i == 0)
                .label(format!("{cmd}: {what}"))
                .fill(if i == 0 { if mac { t.accent } else { Color::WHITE.alpha(0.08) } } else { Color::TRANSPARENT })
                .on_press(move |s: &mut App| {
                    s.prompt.replace_text(format!("{cmd} "));
                    s.prompt.focused = true;
                })
                .child(text(cmd).font(TextStyle::new(t.fs, MEDIUM).mono()).color(if i == 0 && mac { Color::WHITE } else { t.text }))
                .child(text(what.to_string()).font(t.font_small()).color(if i == 0 && mac { Color::WHITE.alpha(0.8) } else { t.text2 }).truncate().shrink())
                .child(el().grow(1.0))
                .child(text(if mac { "⇥" } else { "Tab" }).font(t.font_small()).color(if i == 0 && mac { Color::WHITE.alpha(0.7) } else { t.text3 })),
        );
    }
    Some(c)
}

/// The name of a setting's current value.
fn current_name(o: &SessionConfigOption) -> String {
    o.options.iter().find(|v| v.value == o.current_value).map(|v| v.name.clone()).unwrap_or_else(|| o.current_value.clone())
}

/// The chip's short name for a model: "Default (recommended)" is "Default". The popover
/// keeps the full name and description.
pub fn chip_label(name: &str) -> String {
    let n = name.trim();
    if let Some(open) = n.strip_suffix(')').and_then(|rest| rest.rfind('(')) {
        let head = n[..open].trim_end();
        if !head.is_empty() {
            return head.to_string();
        }
    }
    n.to_string()
}

/// The agent's mark for the chip, monochrome, as the reference's model glyph: its logo, or its
/// letter in a square.
fn agent_mark(agent: AgentKind, size: f32, ink: Color) -> El {
    if let Some(logo) = ic::agent_logo(agent) {
        return icon(logo, size).color(ink);
    }
    let letter = agent.display_name().chars().next().unwrap_or('?').to_ascii_uppercase();
    row()
        .size(size, size)
        .center()
        .radius((size * 0.28).round())
        .border(1.0, ink.alpha(0.8))
        .child(text(letter.to_string()).font(TextStyle::new((size * 0.6).round(), MEDIUM)).color(ink))
}

/// The composer's model chip: the agent's mark, the model, a bolt with fast mode on. It opens
/// the model popover; None when the session reports none of those settings.
fn model_chip(s: &App, sess: &Session, t: &Theme) -> Option<El> {
    if !App::has_model_settings(sess) {
        return None;
    }
    let mac = t.mac();
    let model = App::config_option(sess, "model");
    let effort = App::config_option(sess, "thought_level");
    let fast = App::fast_option(sess);
    let fast_on = fast.is_some_and(|o| o.current_value == "on");
    let name = model.or(effort).map(current_name).unwrap_or_else(|| "Fast mode".into());
    let ready = config_ready(sess);
    let open = s.menu == Some(Menu::Model);
    let ink = if !ready { t.text3 } else if open { t.text } else { t.text2 };
    let mut what = vec![name.clone()];
    if let Some(e) = effort.filter(|_| model.is_some()) {
        what.push(format!("{} effort", current_name(e)));
    }
    if fast_on {
        what.push("fast mode".into());
    }
    let mut b = row()
        .key("model-chip")
        .h(if mac { 26.0 } else { 32.0 })
        .px(if mac { 8.0 } else { 10.0 })
        .mr(if mac { 6.0 } else { 8.0 })
        .gap(if mac { 5.0 } else { 6.0 })
        .center_y()
        .radius(t.btn_radius)
        .role("button")
        .label(format!("Model: {}", what.join(", ")))
        .help("Model, Effort and Fast Mode")
        .expanded(open)
        .focusable(ready)
        .disabled(!ready)
        .child(agent_mark(sess.agent, if mac { 13.0 } else { 14.0 }, ink))
        .child(text(clip(&chip_label(&name), 28)).font(TextStyle::new(if mac { 12.5 } else { 13.5 }, if mac { 400.0 } else { 500.0 })).color(ink));
    if fast_on {
        b = b.child(icon(&ic::BOLT, if mac { 11.0 } else { 12.0 }).color(ink));
    }
    if open {
        b = b.fill(Color::WHITE.alpha(0.10));
    }
    if !ready {
        return Some(b);
    }
    Some(b.hover(Color::WHITE.alpha(0.07)).press(Color::WHITE.alpha(0.12)).on_press(|s: &mut App| {
        s.effort_drag = None;
        s.menu = if s.menu == Some(Menu::Model) { None } else { Some(Menu::Model) };
    }))
}

/// The popover's width.
fn popover_w(t: &Theme) -> f32 {
    if t.mac() { 300.0 } else { 320.0 }
}

/// The model popover over its chip (NSPopover on macOS, GtkPopover on GNOME, each with its
/// arrow): the models, an effort slider and the fast-mode switch, each only when the session
/// has it. Escape or a click outside closes it (the menu layer).
pub fn model_popover(s: &App, sess: &Session, t: &Theme) -> Vec<El> {
    let mac = t.mac();
    let sid = sess.session_id.clone();
    // The panel's inset, and each section's inside it.
    let (pad, inset) = (6.0, if mac { 8.0 } else { 10.0 });
    let w = popover_w(t);
    let mut sections: Vec<El> = vec![];
    let title = |l: &str| text(l.to_string()).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, MEDIUM)).color(if mac { t.text3 } else { t.text2 });
    if let Some(o) = App::config_option(sess, "model") {
        let mut list = col().key("model-list").w_full().gap(if mac { 0.0 } else { 2.0 }).role("listbox").label(o.name.clone()).focus_group();
        for v in &o.options {
            let on = v.value == o.current_value;
            let (sid, id, val) = (sid.clone(), o.id.clone(), v.value.clone());
            let mut names = col().grow(1.0).min_w(0.0).gap(1.0).child(text(v.name.clone()).font(TextStyle::new(if mac { 13.0 } else { 14.5 }, 400.0)).color(t.text));
            if let Some(d) = &v.description {
                names = names.child(text(clip(d, 48)).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 400.0)).color(t.text2).truncate());
            }
            let mut r = row()
                .key(format!("model-{}", v.value))
                .w_full()
                .min_h(if mac { 30.0 } else { 40.0 })
                .px(if mac { 8.0 } else { 10.0 })
                .py(if mac { 4.0 } else { 5.0 })
                .gap(10.0)
                .center_y()
                .radius(t.row_radius)
                .role("option")
                .checked(on)
                .label(v.name.clone())
                .focusable(true)
                .hover(Color::WHITE.alpha(if mac { 0.07 } else { 0.08 }))
                .press(Color::WHITE.alpha(0.12))
                .on_press(move |s: &mut App| {
                    s.set_config(&sid, &id, &val);
                })
                .child(names);
            r = r.child(if on { icon(&ic::CHECK, 14.0).color(t.accent) } else { el().size(14.0, 14.0) });
            list = list.child(r);
        }
        sections.push(col().w_full().gap(if mac { 4.0 } else { 6.0 }).child(title(&o.name).px(if mac { 8.0 } else { 10.0 })).child(list));
    }
    if let Some(o) = App::config_option(sess, "thought_level") {
        sections.push(effort_section(s, &sid, o, w - 2.0 * (pad + inset), t).px(inset));
    }
    if let Some(o) = App::fast_option(sess) {
        let on = o.current_value == "on";
        let (sid, id) = (sid.clone(), o.id.clone());
        let mut label = col().grow(1.0).min_w(0.0).gap(1.0).child(text("Fast mode").font(TextStyle::new(if mac { 13.0 } else { 14.5 }, 400.0)).color(t.text));
        if let Some(d) = &o.description {
            label = label.child(text(clip(d, 48)).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 400.0)).color(t.text2).truncate());
        }
        sections.push(
            row()
                .key("fast-mode")
                .w_full()
                .px(if mac { 8.0 } else { 10.0 })
                .py(if mac { 2.0 } else { 4.0 })
                .gap(10.0)
                .center_y()
                .child(label)
                .child(t.switch(on, "Fast mode", move |s: &mut App| s.set_config(&sid, &id, if on { "off" } else { "on" })).key("fast-switch")),
        );
    }
    let n = sections.len();
    let mut panel = col()
        .key("model-popover")
        .w(w)
        .padding(pad)
        .radius(t.box_radius)
        .fill(if mac { rune::color::hex("#2f2f31").unwrap_or(Color::BLACK) } else { rune::color::hex("#36363a").unwrap_or(Color::BLACK) })
        .border(if mac { 0.5 } else { 1.0 }, Color::WHITE.alpha(if mac { 0.13 } else { 0.12 }))
        .shadow(10.0, 30.0, Color::BLACK.alpha(0.5))
        .role("dialog")
        .label("Model")
        .focus_return("model-chip");
    for (i, sec) in sections.into_iter().enumerate() {
        panel = panel.child(sec.py(if mac { 6.0 } else { 8.0 }));
        if i + 1 < n {
            let line = el().h(if mac { 0.5 } else { 1.0 }).grow(1.0).fill(Color::WHITE.alpha(if mac { 0.10 } else { 0.08 }));
            panel = panel.child(row().w_full().my(4.0).px(if mac { inset } else { 0.0 }).child(line));
        }
    }
    let arrow_h = if mac { 10.0 } else { 8.0 };
    let gap = 2.0;
    let panel = panel.anchored("model-chip", rune::Side::Top, rune::Align::End, gap + arrow_h - 0.6);
    let arrow = if mac { icon(&ic::POPOVER_ARROW_MAC, 20.0) } else { icon(&ic::POPOVER_ARROW_GNOME, 16.0) };
    let arrow = arrow.key("model-popover-arrow").a11y_hidden().anchored("model-chip", rune::Side::Top, rune::Align::Center, gap);
    vec![panel, arrow]
}

/// "Effort  High" over a stepped slider: one stop per level, tick dots, a neutral track.
/// Dragging moves the knob and sets the level on release; arrow keys step it.
fn effort_section(s: &App, sid: &str, o: &SessionConfigOption, width: f32, t: &Theme) -> El {
    let mac = t.mac();
    let n = o.options.len();
    let at = s.effort_drag.filter(|&i| i < n).unwrap_or_else(|| crate::model::stop_of(o));
    let level = o.options.get(at).map(|v| v.name.clone()).unwrap_or_default();
    let knob = if mac { 18.0 } else { 20.0 };
    let track_h = if mac { 4.0 } else { 6.0 };
    let span = (width - knob).max(1.0);
    let x_of = |i: usize| if n < 2 { 0.0 } else { span * i as f32 / (n - 1) as f32 };
    let top = (knob - track_h) / 2.0;
    let mut slider = col()
        .key("effort-slider")
        .w(width)
        .h(knob)
        .role("slider")
        .label(o.name.clone())
        .a11y_value(level.clone())
        .range_value(at as f64, 0.0, n.saturating_sub(1) as f64)
        .range_step(1.0)
        .focus_ring(2.0, 1.0, t.focus)
        .child(el().pin(Some(knob / 2.0), Some(top), None, None).w(span).h(track_h).radius(track_h / 2.0).fill(Color::WHITE.alpha(if mac { 0.12 } else { 0.15 })))
        .child(el().pin(Some(knob / 2.0 - track_h / 2.0), Some(top), None, None).w(x_of(at) + track_h).h(track_h).radius(track_h / 2.0).fill(Color::WHITE.alpha(if mac { 0.55 } else { 0.6 })));
    let dot = 4.0;
    for i in 0..n {
        let filled = i <= at;
        slider = slider.child(el().pin(Some(knob / 2.0 + x_of(i) - dot / 2.0), Some((knob - dot) / 2.0), None, None).size(dot, dot).radius(dot / 2.0).fill(if filled { Color::BLACK.alpha(0.28) } else { Color::WHITE.alpha(0.3) }));
    }
    slider = slider.child(el().key("effort-knob").pin(Some(x_of(at)), Some(0.0), None, None).size(knob, knob).radius(knob / 2.0).fill(rune::color::hex("#ececec").unwrap_or(Color::WHITE)).shadow(1.0, 3.0, Color::BLACK.alpha(0.35)));
    let values: Vec<String> = o.options.iter().map(|v| v.value.clone()).collect();
    let current = crate::model::stop_of(o);
    let (sid1, id1, v1) = (sid.to_string(), o.id.clone(), values.clone());
    let (sid2, id2, v2) = (sid.to_string(), o.id.clone(), values.clone());
    let (sid3, id3, v3) = (sid.to_string(), o.id.clone(), values);
    let slider = slider
        .on_drag(move |s: &mut App, d: &mut rune::Drag, _: &mut Tx| {
            let i = crate::model::stop_at(v1.len(), d.x - knob / 2.0, span);
            match d.phase {
                rune::Phase::Down | rune::Phase::Move => s.effort_drag = Some(i),
                rune::Phase::Up => {
                    s.effort_drag = None;
                    if !d.cancelled() && i != current {
                        s.set_config(&sid1, &id1, &v1[i]);
                    }
                }
            }
        })
        .on_step(move |s: &mut App, delta: i32, _: &mut Tx| {
            let i = crate::model::step_stop(v2.len(), current, delta);
            if i != current {
                s.set_config(&sid2, &id2, &v2[i]);
            }
        })
        .on_step_limit(move |s: &mut App, max: bool, _: &mut Tx| {
            let i = if max { v3.len().saturating_sub(1) } else { 0 };
            if i != current {
                s.set_config(&sid3, &id3, &v3[i]);
            }
        });
    col()
        .key("effort")
        .w_full()
        .gap(if mac { 8.0 } else { 10.0 })
        .child(
            row()
                .w_full()
                .center_y()
                .child(text(o.name.clone()).font(TextStyle::new(if mac { 13.0 } else { 14.5 }, 400.0)).color(t.text))
                .child(el().grow(1.0))
                .child(text(level).font(TextStyle::new(if mac { 13.0 } else { 14.5 }, 400.0)).color(t.text2)),
        )
        .child(slider)
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
                .child(t.icon_button(&ic::STOP, 14.0, &format!("{} ({})", if s.live.is_some() { "Stop Dictating" } else { "Stop and Transcribe" }, keys(s.look, Cmd::Dictate)), |s, _| s.mic_press()))
                .child(t.dot(t.danger, 8.0))
                .child(text(format!("{}:{:02}", secs / 60, secs % 60)).font(TextStyle::new(t.fs, MEDIUM)).color(t.text))
                .child(row().w(60.0).h(4.0).radius(2.0).fill(Color::WHITE.alpha(0.12)).child(el().w(60.0 * level).h(4.0).radius(2.0).fill(t.text)))
                .child(t.small_button("Discard", |s| s.mic_cancel()))
        }
        Mic::Transcribing => row().key("transcribing").gap(8.0).px(6.0).center_y().child(t.small(if s.live.is_some() { "Finishing…" } else { "Transcribing…" })),
        Mic::Idle => {
            let mic_help = match &s.voice {
                Some(v) if v.ready && v.streaming => format!("Dictate ({}): talk, then press {} to send", keys(s.look, Cmd::Dictate), if mac { "Return" } else { "Enter" }),
                Some(v) if v.ready => format!("Dictate ({}): click, talk, click again", keys(s.look, Cmd::Dictate)),
                Some(v) if v.progress.is_some() => "Dictate: the speech model is downloading".to_string(),
                _ => format!("Dictate ({}; downloads a speech model once)", keys(s.look, Cmd::Dictate)),
            };
            row()
                .key("media")
                .gap(if mac { 2.0 } else { 4.0 })
                .center_y()
                // macOS composers attach with a plus (Messages); GNOME's with a paperclip.
                .child(t.icon_button(if mac { &ic::PLUS } else { &ic::CLIP }, if mac { 14.0 } else { 16.0 }, &format!("Attach Files ({})", keys(s.look, Cmd::Attach)), |s, tx| run(s, Cmd::Attach, tx)).key("attach"))
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

/// The widest an attached image's picture gets in its chip (a wide screenshot), in points.
const PREVIEW_MAX_W: f32 = 56.0;

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
        // An image shows a small picture of itself (once decoded), so pasted images can be
        // told apart; other files, and images still decoding, show the file mark.
        let pic = f.preview.as_ref().and_then(|img| Some((img, img.size()?)));
        let mut chip = row()
            .key(format!("file-{lid}"))
            .h(if pic.is_some() { 44.0 } else { 30.0 })
            .gap(7.0)
            .center_y()
            .pl(if pic.is_some() { 6.0 } else { 8.0 })
            .pr(6.0)
            .radius(t.btn_radius)
            .fill(Color::WHITE.alpha(0.08));
        chip = match pic {
            Some((img, [w, h])) => {
                let (bw, bh) = (PREVIEW_MAX_W, crate::model::PREVIEW_PT);
                let scale = (bw / w.max(1) as f32).min(bh / h.max(1) as f32).min(1.0);
                let (w, h) = ((w as f32 * scale).round().max(12.0), (h as f32 * scale).round().max(12.0));
                chip.child(
                    row().w(bw.min(w.max(bh))).h(bh).center().child(
                        el().key(format!("file-preview-{lid}"))
                            .size(w, h)
                            .radius(4.0)
                            .clip_rounded(4.0)
                            .border(0.5, Color::WHITE.alpha(0.12))
                            .pixels_fit(img.pixels(), rune::SurfaceFit::Fill),
                    ),
                )
            }
            None => chip.child(icon(&ic::FILE_SM, 13.0).color(t.text2)),
        };
        r = r.child(
            chip
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

pub(crate) fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// A prompt bubble's text and attached file names (images too, unless they show as pictures).
fn prompt_body(p: &Value, with_images: bool, t: &Theme) -> Vec<El> {
    let mut v = vec![];
    let tx = p["text"].as_str().unwrap_or("");
    if !tx.is_empty() {
        v.push(text(tx.to_string()).font(TextStyle::new(t.fs, 400.0).lh((t.fs * 1.45).round())).color(t.text).wrap().selectable(true));
    }
    let is_image = |a: &Value| a["mime"].as_str().is_some_and(|m| m.starts_with("image/"));
    for a in p["attachments"].as_array().into_iter().flatten().filter(|a| with_images || !is_image(a)) {
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

/// The user's message: its pictures, then the bubble with its words and other files
/// (no bubble for pictures alone), right-aligned.
fn prompt_column(s: &App, p: &Value, t: &Theme) -> El {
    let pictures = s.host_keeps_images().then(|| image_strip(s, p, t)).flatten();
    let body = prompt_body(p, pictures.is_none(), t);
    let mut c = col().items_end().gap(6.0).max_w(520.0);
    if let Some(strip) = pictures {
        c = c.child(strip);
    }
    if !body.is_empty() {
        c = c.child(col().px(13.0).py(9.0).radius(16.0).fill(t.bubble).gap(6.0).children(body));
    }
    c
}

/// Largest thumbnail of a sent image, in points.
const THUMB_W: f32 = 240.0;
const THUMB_H: f32 = 180.0;

/// A message's images as thumbnails; clicking one opens it in the viewer.
fn image_strip(s: &App, p: &Value, t: &Theme) -> Option<El> {
    let images = crate::model::sent_images(p);
    if images.is_empty() {
        return None;
    }
    let mut r = row().gap(6.0).flex_wrap().justify_end();
    for (id, name) in images {
        r = r.child(thumbnail(s, &id, &name, t));
    }
    Some(r)
}

fn thumbnail(s: &App, id: &str, name: &str, t: &Theme) -> El {
    let shown = match s.images.get(id) {
        Some(SentImage::Ready(img)) => img.size().map(|sz| (img, sz)),
        _ => None,
    };
    let base = el().key(format!("img-{id}")).radius(12.0).clip_rounded(12.0).border(0.5, Color::WHITE.alpha(0.12));
    if let Some((img, [w, h])) = shown {
        // Fit the box, never larger than the picture itself.
        let scale = (THUMB_W / w as f32).min(THUMB_H / h as f32).min(1.0);
        let (w, h) = ((w as f32 * scale).round().max(24.0), (h as f32 * scale).round().max(24.0));
        let open = id.to_string();
        return base
            .size(w, h)
            .pixels_fit(img.pixels(), rune::SurfaceFit::Fill)
            .role("button")
            .label(format!("Show {name}"))
            .help(name.to_string())
            .focusable(true)
            .cursor(rune::Cursor::Pointer)
            .on_press(move |s: &mut App| s.view_image(Some(open.clone())));
    }
    let why = match s.images.get(id) {
        Some(SentImage::Failed(e)) => Some(e.clone()),
        Some(SentImage::Ready(img)) => img.error().map(|e| e.to_string()),
        _ => None,
    };
    let note = if why.is_some() { "Can't show this image" } else { "Loading…" };
    let mut b = base.size(160.0, 110.0).fill(Color::WHITE.alpha(0.05)).padding(10.0).center();
    if let Some(why) = why {
        b = b.help(why);
    }
    b = b.child(
        col()
            .items_center()
            .gap(4.0)
            .child(icon(&ic::FILE_SM, 16.0).color(t.text2))
            .child(text(clip(name, 40)).font(TextStyle::new(12.0, 400.0)).color(t.text2).wrap().align(rune::Align::Center))
            .child(text(note).font(TextStyle::new(11.0, 400.0)).color(t.text3)),
    );
    b.label(format!("{name}, {note}"))
}

/// The image being viewed, as large as the window allows, over everything. Esc, the close
/// button or a click beside it closes it.
pub fn image_viewer(s: &App, t: &Theme) -> Option<El> {
    let id = s.viewing.as_ref()?;
    let Screen::Session(sid) = &s.screen else { return None };
    let SentImage::Ready(img) = s.images.get(id)? else { return None };
    let [w, h] = img.size()?;
    let name = s
        .transcripts
        .get(sid)
        .and_then(|tr| tr.events.iter().filter(|e| e.kind == EventType::Prompt).flat_map(|e| crate::model::sent_images(&e.payload)).find(|(a, _)| a == id))
        .map(|(_, n)| n)
        .unwrap_or_else(|| "Image".into());
    let close = |s: &mut App| s.view_image(None);
    let bar = row()
        .w_full()
        .h(44.0)
        .px(12.0)
        .gap(8.0)
        .center_y()
        .child(text(format!("{name} · {w} × {h}")).font(TextStyle::new(13.0, MEDIUM)).color(Color::WHITE.alpha(0.9)).grow(1.0))
        .child(t.icon_button(&ic::CLOSE, 11.0, "Close (Esc)", move |s: &mut App, _: &mut Tx| close(s)).key("viewer-close"));
    let picture = el()
        .key("viewer-image")
        .aspect_fit(w as f32 / h as f32)
        .max_w(w as f32)
        .max_h(h as f32)
        .pixels_fit(img.pixels(), rune::SurfaceFit::Fit)
        .role("img")
        .label(name.clone());
    let stage = col().w_full().grow(1.0).min_h(0.0).px(24.0).pb(24.0).items_center().justify_center().child(picture);
    Some(
        crate::sheet::scrim("image-viewer", 0.88, close)
            .child(col().key("viewer").w_full().h_full().modal(true, close).child(bar).child(stage)),
    )
}

/// "Working…" with a spinner, while the agent is quiet.
fn working_row(label: &'static str, t: &Theme) -> El {
    row()
        .gap(if t.mac() { 6.0 } else { 8.0 })
        .center_y()
        .role("status")
        .label(label)
        .child(icon(&ic::SPINNER, 13.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)))
        .child(text(label).font(TextStyle::new(if t.mac() { 12.0 } else { 13.5 }, 400.0)).color(t.text2))
}

/// Seconds between two event times, "8.4 s" style.
pub(crate) fn secs(from: i64, to: i64) -> Option<String> {
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

pub(crate) fn pre(t: &Theme, body: String) -> El {
    text(body).font(TextStyle::new(t.fs_mono_small, 400.0).mono().lh((t.fs_mono_small * 1.55).round())).color(t.text2).wrap().selectable(true)
}

fn event_row(s: &App, sess: &Session, e: &Event, next_ts: Option<i64>, results: &HashMap<&str, &Event>, t: &Theme, k: &Kit) -> Option<El> {
    let mac = t.mac();
    let p = &e.payload;
    Some(match e.kind {
        EventType::Prompt => {
            let device = agenthost_proto::device_display_name(&str_of(&p["device"])).to_string();
            let mut c = col().items_end().gap(4.0).max_w(520.0).child(prompt_column(s, p, t));
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
        // A subagent's words are in its panel, never in the main reply.
        EventType::SubagentMessage => return None,
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
        EventType::ApprovalResolved if p["withdrawn"] == true => t.faint("Request withdrawn by the agent".to_string()),
        EventType::ApprovalResolved => {
            let decision = str_of(&p["decision"]);
            let by = agenthost_proto::device_display_name(&str_of(&p["device"])).to_string();
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
            // Only an end or a failure is news; the host starting or resuming the agent isn't.
            if !matches!(st.as_str(), "failed" | "ended") {
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

/// A tool call's title on one line: a heredoc or a multi-line script shows its first line.
pub(crate) fn one_line(name: &str) -> String {
    let mut lines = name.trim().lines();
    let first = lines.next().unwrap_or("").trim_end();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

/// "Ran 3 commands, read 2 files, used 1 tool".
pub(crate) fn activity_title(calls: &[&Event]) -> String {
    let count = |kinds: &[&str]| calls.iter().filter(|e| e.payload["source"] != "mcp" && kinds.contains(&e.payload["kind"].as_str().unwrap_or(""))).count();
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let mut parts = vec![];
    let mut known = 0;
    for (kinds, verb, one, many) in [
        (&["execute"][..], "ran", "command", "commands"),
        (&["read"][..], "read", "file", "files"),
        (&["edit", "delete", "move"][..], "edited", "file", "files"),
        (&["search"][..], "ran", "search", "searches"),
        (&["fetch"][..], "opened", "page", "pages"),
    ] {
        let n = count(kinds);
        known += n;
        if n > 0 {
            parts.push(format!("{verb} {}", plural(n, one, many)));
        }
    }
    let other = calls.len() - known;
    if other > 0 {
        parts.push(format!("used {}", plural(other, "tool", "tools")));
    }
    let s = parts.join(", ");
    let mut cs = s.chars();
    cs.next().map(|c| c.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_else(|| "Thought".into())
}

/// A run of tool calls and thoughts. Two or more, with at least one call,
/// fold into a single line ("Ran 7 commands, edited 2 files") that opens to
/// show each step, as the iPhone app does; while a call runs, the line says
/// what it is doing. A lone call or thought shows as it is.
fn activity(s: &App, sess: &Session, run: &[(&Event, Option<i64>)], results: &HashMap<&str, &Event>, t: &Theme, k: &Kit) -> Vec<El> {
    let each = |&(e, next): &(&Event, Option<i64>)| event_row(s, sess, e, next, results, t, k).map(|r| r.key(format!("ev-{}", e.seq)));
    let calls: Vec<&Event> = run.iter().map(|(e, _)| *e).filter(|e| e.kind == EventType::ToolCall).collect();
    if run.len() < 2 || calls.is_empty() {
        return run.iter().filter_map(each).collect();
    }
    let mac = t.mac();
    let key = format!("act-{}", run[0].0.seq);
    let open = s.expanded.contains(&key);
    let title = activity_title(&calls);
    let running = calls.iter().rev().find(|e| !results.contains_key(e.payload["tool_call_id"].as_str().unwrap_or("")));
    let size = if mac { 12.0 } else { 13.5 };
    let (lead, label) = match running {
        Some(e) => (
            icon(&ic::SPINNER, 13.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)),
            text(clip(&one_line(&str_of(&e.payload["name"])), 140)).font(TextStyle::new(t.fs_mono * 0.9, 400.0).mono()),
        ),
        None => (icon(&ic::STEPS, 14.0).color(t.text2), text(title.clone()).font(TextStyle::new(size, 400.0))),
    };
    let k2 = key.clone();
    let head = row()
        .w_full()
        .gap(if mac { 6.0 } else { 8.0 })
        .center_y()
        .py(2.0)
        .role("button")
        .label(title)
        .help(if open { "Hide the steps" } else { "Show each step" })
        .focusable(true)
        .on_press(move |s: &mut App| {
            if !s.expanded.remove(&k2) {
                s.expanded.insert(k2.clone());
            }
        })
        .child(lead)
        .child(label.color(t.text2).truncate().shrink().min_w(0.0))
        .child(icon(if open { &ic::CHEVRON_D } else { &ic::CHEVRON_R }, 10.0).color(t.text3));
    let mut c = col().key(key).w_full().gap(6.0).child(head);
    if open {
        let steps = run.iter().filter_map(each).fold(col().w_full().gap(2.0), |c, r| c.child(r));
        c = c.child(steps.ml(6.0).pl(10.0).border_sides([0.0, 0.0, 0.0, 1.0], Color::WHITE.alpha(0.1)));
    }
    vec![c]
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
    let mono_name = text(clip(&one_line(&name), 140)).font(TextStyle::new(t.fs_mono, 400.0).mono()).color(t.text).truncate();
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
            .py(if open { 7.0 } else { 5.0 })
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
        // A quiet line, not a card: a long session reads as prompts and replies at a glance.
        // Only a denial stands out: a failed call (a grep with no match) is routine.
        let tone = if word == "Denied" { t.warn } else { t.text3 };
        head = head
            .gap(8.0)
            .px(10.0)
            .py(if open { 6.0 } else { 3.0 })
            .radius(t.row_radius)
            .child(icon(kind_icon, 13.0).color(t.text3))
            .child(mono_name.font(TextStyle::new(t.fs_mono * 0.9, 400.0).mono()).color(t.text2).shrink().min_w(0.0))
            .child(text(sub).font(t.font_small()).color(tone))
            .child(el().grow(1.0))
            .child(icon(if open { &ic::CHEVRON_UP } else { &ic::CHEVRON_DOWN }, 11.0).color(t.text3));
    }
    let mut c = col().w_full().radius(t.card_radius).clip_rounded(t.card_radius).child(head);
    if mac || open {
        c = c.fill(t.card);
    }
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
            body = body.child(text("Blocked by the sandbox").font(TextStyle::new(t.fs_small, MEDIUM)).color(t.warn));
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
    let title = pending.summary.clone().unwrap_or_else(|| format!("Run {command}"));
    // The host says what the button does and what to watch for; older
    // hosts only ever asked to install.
    let approve_label = a["approve_label"].as_str().unwrap_or("Install").to_string();
    let note = a["note"].as_str().unwrap_or("The package manager may run the package's install scripts with your permissions. Install only what you expected.").to_string();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let mins = ((pending.expires_at - now) / 60_000).max(0);
    let expires = if mins >= 1 { format!("expires in {mins} min") } else { "expires soon".into() };
    let sub = format!("{agent} asks · runs as you, outside the sandbox · {expires}");
    let aid = pending.approval_id.clone();
    let (a1, a2) = (aid.clone(), aid);
    let deny = t.button("Deny", Btn::Default, Some(&keys(s.look, Cmd::Deny)), move |s| s.respond(&a2, false)).key("deny");
    let approve = t.button(&approve_label, Btn::Primary, Some(&keys(s.look, Cmd::Approve)), move |s| s.respond(&a1, true)).key("approve");
    let (deny, approve) = if mac { (deny, approve) } else { (deny.px(18.0), approve.px(18.0)) };
    let mut head = row().w_full().gap(10.0).center_y();
    if mac {
        head = head.child(row().size(28.0, 28.0).radius(7.0).center().fill(t.warn.alpha(0.18)).child(icon(&ic::TERMINAL, 15.0).color(t.warn)));
    }
    head = head.child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(if mac { 1.0 } else { 2.0 })
            .child(text(title).font(TextStyle::new(t.fs, MEDIUM)).color(t.text).wrap())
            .child(text(sub).font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 400.0)).color(t.text2).wrap()),
    );
    let mut card = col()
        .key("approval-card")
        .w_full()
        .gap(if mac { 10.0 } else { 12.0 })
        .radius(t.card_radius)
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
    // What will be posted (a pull request's description), as the user reads it.
    if let Some(details) = a["details"].as_str().map(str::trim).filter(|d| !d.is_empty()) {
        card = card.child(
            col()
                .w_full()
                .px(11.0)
                .py(9.0)
                .radius(t.box_radius)
                .border(0.5, t.text2.alpha(0.3))
                .child(text(details.to_string()).font(t.font()).color(t.text).wrap()),
        );
    }
    card = card.child(col().w_full().px(11.0).py(9.0).radius(t.box_radius).fill(Color::BLACK.alpha(0.25)).child(pre(t, format!("$ {command}"))));
    card = card.child(
        text(note)
            .font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 400.0))
            .color(t.text2)
            .wrap(),
    );
    let mut buttons = row().w_full().gap(if mac { 8.0 } else { 10.0 }).center_y();
    // Settings can let this tool run without asking: offer that here, set apart from the
    // primary at the leading end, as a plain button (a flat one on GNOME).
    if s.can_stop_asking(pending) {
        let a3 = pending.approval_id.clone();
        // A command names what it stops asking for ("gh pr edit"): that prefix, not every command.
        let rule = crate::model::allow_rule(pending).filter(|_| pending.tool == "run_command").map(|r| r.join(" "));
        let label = match &rule {
            Some(r) => format!("Always Allow \u{201c}{r}\u{201d}"),
            None => "Approve and Don't Ask Again".to_string(),
        };
        let label = label.as_str();
        let help = match (pending.tool.as_str(), &rule) {
            ("run_command", Some(r)) => format!("Run this, and let agents run {r} commands without asking"),
            ("create_pull_request", _) => "Open this pull request, and let agents open pull requests without asking".into(),
            ("git_push", _) => "Push this branch, and let agents push without asking (a force-push still asks)".into(),
            _ => "Approve this, and don't ask again".into(),
        };
        let b = if mac {
            t.button(label, Btn::Default, None, move |s| s.approve_and_stop_asking(&a3))
        } else {
            row()
                .h(34.0)
                .px(14.0)
                .center_y()
                .radius(t.btn_radius)
                .role("button")
                .label(label)
                .focusable(true)
                .hover(Color::WHITE.alpha(0.07))
                .press(Color::WHITE.alpha(0.12))
                .on_press(move |s: &mut App| s.approve_and_stop_asking(&a3))
                .child(text(label).font(TextStyle::new(14.0, MEDIUM)).color(t.text))
        };
        buttons = buttons.child(b.key("approve-always").help(help));
    }
    card.child(buttons.child(el().grow(1.0)).child(deny).child(approve))
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
    let (deny, approve) = if mac { (deny, approve) } else { (deny.px(18.0), approve.px(18.0)) };
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
            .child(text(title).font(TextStyle::new(t.fs, MEDIUM)).color(t.text).wrap())
            .child(text(sub).font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 400.0)).color(t.text2).wrap()),
    );
    let mut card = col()
        .key("approval-card")
        .w_full()
        .gap(if mac { 10.0 } else { 12.0 })
        .radius(t.card_radius)
        .padding(if mac { 14.0 } else { 16.0 })
        .label(format!("{agent} asks to use {conn} {tool}"))
        .child(head);
    card = if mac {
        card.px(16.0).fill(Color::WHITE.alpha(0.05)).border(0.5, t.warn.alpha(0.45)).box_shadow(rune::Shadow { spread: 3.0, color: t.warn.alpha(0.08), ..Default::default() })
    } else {
        card.fill(t.card).box_shadow(rune::Shadow { spread: 1.0, inset: true, color: t.warn.alpha(0.6), ..Default::default() })
    };
    if !args.is_empty() {
        card = card.child(col().w_full().px(11.0).py(9.0).radius(t.box_radius).fill(Color::BLACK.alpha(0.25)).child(pre(t, clip(&args, 3000))));
    }
    Some(card.child(row().w_full().gap(if mac { 8.0 } else { 10.0 }).child(el().grow(1.0)).child(deny).child(approve)))
}
