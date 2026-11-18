//! Subagents: a card in the transcript for each `Agent`/`Task` call (what it's doing, or the
//! start of its report), and the agent panel beside the session with its task, its steps and
//! messages as they happen, and its whole report. The panel stands in for the side-question
//! pane while it's open (same place, same chrome); closing it brings the side pane back.

use crate::icons as ic;
use crate::look::Theme;
use crate::model::{plain_lines, span_label, App, SubItem, Subagent};
use crate::session::{pre, secs, str_of};
use crate::view::clip;
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, ScrollSpec, TextStyle, Tx};
use rune_kit::Kit;
use serde_json::Value;
use std::collections::HashMap;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// How long it ran, or has been running.
fn elapsed(a: &Subagent) -> String {
    span_label(a.ended.unwrap_or_else(now) - a.started)
}

fn status_mark(t: &Theme, status: &str, size: f32) -> El {
    match status {
        "running" | "pending" | "in_progress" => icon(&ic::SPINNER, size - 1.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)),
        "completed" => icon(&ic::CHECK, size).color(t.ok),
        "stopped" => icon(&ic::STOP_SM, size - 3.0).color(t.text3),
        _ => icon(&ic::CROSS, size - 1.0).color(t.danger),
    }
}

fn status_word(status: &str) -> &'static str {
    match status {
        "running" | "pending" | "in_progress" => "running",
        "completed" => "done",
        "stopped" => "stopped",
        "denied" => "denied",
        _ => "failed",
    }
}

/// The calls and results of a session's events, by call id.
fn by_call(events: &[Event]) -> (HashMap<&str, &Event>, HashMap<&str, &Event>) {
    let (mut calls, mut results) = (HashMap::new(), HashMap::new());
    for e in events {
        if let Some(c) = e.payload["tool_call_id"].as_str() {
            match e.kind {
                EventType::ToolCall => {
                    calls.insert(c, e);
                }
                EventType::ToolResult => {
                    results.insert(c, e);
                }
                _ => {}
            }
        }
    }
    (calls, results)
}

/// A step's one-line summary: its title, else its tool and first argument.
fn step_title(p: &Value) -> String {
    let name = str_of(&p["name"]);
    if !name.is_empty() {
        return name;
    }
    let tool = str_of(&p["tool_name"]);
    let first = p["arguments"].as_object().and_then(|o| o.values().find_map(|v| v.as_str().map(String::from))).unwrap_or_default();
    format!("{tool} {first}").trim().to_string()
}

// ---------------------------------------------------------------------------
// The card in the transcript
// ---------------------------------------------------------------------------

/// Subagents started together: side by side when there's room for two, one per row otherwise.
pub fn group(s: &App, session: &str, call_ids: &[String], t: &Theme) -> El {
    let agents = s.subagents(session);
    let tr = s.transcripts.get(session);
    let events: &[Event] = tr.map(|t| t.events.as_slice()).unwrap_or(&[]);
    let (calls, _) = by_call(events);
    let mut r = row().w_full().gap(10.0).flex_wrap().items_stretch();
    for id in call_ids {
        if let Some(a) = agents.iter().find(|a| &a.call_id == id) {
            // Grown from nothing with a floor: two fit side by side from ~570 pt.
            r = r.child(card(s, session, a, &calls, t).grow(1.0).min_w(280.0));
        }
    }
    r
}

fn card(s: &App, session: &str, a: &Subagent, calls: &HashMap<&str, &Event>, t: &Theme) -> El {
    let mac = t.mac();
    let open = s.agent_open.as_ref().is_some_and(|(sid, c)| sid == session && *c == a.call_id);
    let n = a.steps();
    let kind = if a.agent_type.is_empty() { "agent".to_string() } else { a.agent_type.clone() };
    let sub = format!("{kind} · {n} step{} · {}", if n == 1 { "" } else { "s" }, elapsed(a));
    let title = a.title().to_string();
    let head = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(icon(&ic::SPARKLE, 14.0).color(t.text))
        .child(text(clip(&title, 120)).font(TextStyle::new(t.fs, t.w_bold)).color(t.text).truncate().shrink().min_w(0.0))
        .child(el().grow(1.0))
        .child(status_mark(t, &a.status, 14.0));
    let mut c = col()
        .key(format!("agent-card-{}", a.call_id))
        .gap(4.0)
        .px(if mac { 12.0 } else { 14.0 })
        .py(if mac { 11.0 } else { 12.0 })
        .radius(t.card_radius)
        .fill(t.card)
        .role("button")
        .label(format!("Subagent {title}, {}, {sub}", status_word(&a.status)))
        .help("Show the agent")
        .focusable(true)
        .hover(Color::WHITE.alpha(0.03))
        .child(head)
        .child(text(sub).font(t.font_small()).color(t.text2).truncate());
    let preview: El = if a.running() {
        let last = a.items.iter().rev().find_map(|i| match i {
            SubItem::Step { call_id, .. } => calls.get(call_id.as_str()).map(|e| step_title(&e.payload)),
            _ => None,
        });
        let line = match last {
            Some(l) => format!("▸ {l}"),
            None => "Starting…".to_string(),
        };
        text(clip(&line, 200)).font(TextStyle::new(t.fs_mono_small, 400.0).mono()).color(t.text2).truncate()
    } else {
        // The report's opening paragraph, as plain text.
        let first = a.report.trim().split("\n\n").find(|p| !plain_lines(p).is_empty() && !p.trim_start().starts_with('#')).unwrap_or("");
        let lines = plain_lines(first);
        let body = if lines.is_empty() {
            match a.status.as_str() {
                "stopped" => "Stopped before it reported.".to_string(),
                "completed" => "No report.".to_string(),
                _ => "It failed before it reported.".to_string(),
            }
        } else {
            lines.into_iter().take(3).collect::<Vec<_>>().join(" ")
        };
        text(clip(&body, 400)).font(TextStyle::new(if mac { 12.0 } else { 13.0 }, 400.0).lh(if mac { 17.0 } else { 18.0 })).color(t.text).wrap().max_lines(3)
    };
    c = c.child(preview.mt(4.0));
    let (sid, cid) = (session.to_string(), a.call_id.clone());
    c = c.on_press(move |s: &mut App| s.open_agent(&sid, &cid));
    if open {
        c = c.border(if mac { 1.0 } else { 2.0 }, t.accent);
    } else if mac {
        c = c.border(0.5, t.card_border);
    }
    c
}

/// "1 agent running" over the composer: opens the first running agent. `None` when idle.
pub fn running_pill(s: &App, session: &str, t: &Theme) -> Option<El> {
    let (agents, commands) = s.running_work(session);
    let label = crate::model::running_label(agents, commands)?;
    let first = s.subagents(session).into_iter().find(|a| a.running()).map(|a| a.call_id);
    let mac = t.mac();
    let mut p = row()
        .key("agents-running")
        .h(if mac { 22.0 } else { 26.0 })
        .px(if mac { 9.0 } else { 11.0 })
        .gap(6.0)
        .center_y()
        .radius(if mac { 11.0 } else { 13.0 })
        .fill(Color::WHITE.alpha(0.07))
        .child(icon(&ic::SPINNER, 10.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)))
        .child(text(label.clone()).font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 500.0)).color(t.text2));
    if let Some(cid) = first {
        let sid = session.to_string();
        p = p.role("button").label(format!("{label}: show the agent")).focusable(true).hover(Color::WHITE.alpha(0.11)).on_press(move |s: &mut App| s.open_agent(&sid, &cid));
    }
    Some(row().w_full().child(p))
}

// ---------------------------------------------------------------------------
// The agent panel
// ---------------------------------------------------------------------------

fn heading(t: &Theme, label: &str) -> El {
    if t.mac() {
        text(label.to_uppercase()).font(TextStyle::new(10.5, 600.0)).color(t.text3)
    } else {
        text(label.to_string()).font(TextStyle::new(13.0, 700.0)).color(t.text2)
    }
}

pub fn pane(s: &App, session: &str, call_id: &str, t: &Theme, k: &Kit) -> El {
    let mac = t.mac();
    let a = s.subagent(session, call_id);
    let title_s = a.as_ref().map(|a| a.title().to_string()).unwrap_or_else(|| "Subagent".into());
    let title = text(clip(&title_s, 80)).font(TextStyle::new(t.fs, if mac { 600.0 } else { 700.0 })).color(t.text).truncate().shrink().min_w(0.0);
    let hide = t.icon_button(&ic::CLOSE, 10.0, "Close (Esc)", |s: &mut App, _: &mut Tx| s.close_agent()).key("agent-close");
    let header = if mac {
        row()
            .key("agent-header")
            .w_full()
            .h(t.toolbar_h)
            .pl(16.0)
            .pr(10.0)
            .gap(8.0)
            .center_y()
            .fill(t.toolbar)
            .border_sides([0.0, 0.0, 0.5, 0.0], Color::BLACK.alpha(0.6))
            .window_drag()
            .child(title)
            .child(el().grow(1.0))
            .child(hide)
    } else {
        row()
            .key("agent-header")
            .w_full()
            .h(t.toolbar_h)
            .px(6.0)
            .center_y()
            .window_drag()
            .child(el().w(34.0))
            .child(row().grow(1.0).min_w(0.0).justify_center().child(title))
            .child(hide)
    };
    let body = match &a {
        Some(a) => detail(s, session, a, t, k),
        None => col().child(t.small("This agent is no longer in the session.")),
    };
    let content = col().key("agent-content").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: a.as_ref().is_some_and(|a| a.running()) }).child(body.w_full().px(16.0).py(14.0));
    let mut pane = col().key("agent-pane").w(crate::side::width(t)).h_full().min_h(0.0).role("complementary").label(format!("Agent: {title_s}")).child(header).child(content);
    if a.as_ref().is_some_and(|a| a.running()) {
        let mut foot = row().key("agent-foot").w_full().justify_center().px(16.0).pt(10.0).pb(14.0).child(text("Stopping the turn stops its agents too.").font(TextStyle::new(if mac { 11.0 } else { 12.5 }, 400.0)).color(t.text3));
        if mac {
            foot = foot.border_sides([0.5, 0.0, 0.0, 0.0], t.sep);
        }
        pane = pane.child(foot);
    }
    if mac {
        pane.fill(t.content).border_sides([0.0, 0.0, 0.0, 0.5], Color::BLACK.alpha(0.6))
    } else {
        pane.fill(t.sidebar_top).border_sides([0.0, 0.0, 0.0, 1.0], Color::BLACK.alpha(0.35))
    }
}

/// The task, the steps and messages, and the report.
fn detail(s: &App, session: &str, a: &Subagent, t: &Theme, k: &Kit) -> El {
    let mac = t.mac();
    let tr = s.transcripts.get(session);
    let events: &[Event] = tr.map(|t| t.events.as_slice()).unwrap_or(&[]);
    let (calls, results) = by_call(events);
    let kind = if a.agent_type.is_empty() { "Subagent".to_string() } else { format!("{} agent", a.agent_type) };
    let mut c = col().gap(14.0).child(text(format!("{kind} · {} · {}", status_word(&a.status), elapsed(a))).key("agent-status").font(t.font_small()).color(t.text2).wrap());

    // The task, folded when long.
    if !a.prompt.trim().is_empty() {
        let long = a.prompt.len() > 280 || a.prompt.lines().count() > 5;
        let open = s.agent_task_open;
        let shown = if long && !open { clip(a.prompt.trim(), 240) } else { a.prompt.trim().to_string() };
        let mut task = col()
            .key("agent-task")
            .w_full()
            .gap(4.0)
            .px(10.0)
            .py(8.0)
            .radius(if mac { 7.0 } else { 9.0 })
            .fill(Color::WHITE.alpha(if mac { 0.06 } else { 0.07 }))
            .child(heading(t, "Task"))
            .child(text(shown).font(TextStyle::new(if mac { 12.0 } else { 13.5 }, 400.0).lh(if mac { 17.0 } else { 19.0 })).color(t.text).wrap().selectable(true));
        if long {
            task = task.child(
                text(if open { "Show less" } else { "Show more" })
                    .key("agent-task-toggle")
                    .font(TextStyle::new(if mac { 11.5 } else { 13.0 }, 500.0))
                    .color(t.accent_text)
                    .role("button")
                    .focusable(true)
                    .on_press(|s: &mut App| s.agent_task_open = !s.agent_task_open),
            );
        }
        c = c.child(task);
    }

    // Steps, with its messages where they came.
    let mut steps = col().key("agent-steps").w_full().gap(if mac { 2.0 } else { 4.0 });
    for item in &a.items {
        match item {
            SubItem::Step { call_id, .. } => {
                if let Some(e) = calls.get(call_id.as_str()) {
                    steps = steps.child(step_row(s, e, results.get(call_id.as_str()).copied(), a.running(), t));
                }
            }
            SubItem::Message(m) => {
                steps = steps.child(text(m.clone()).font(TextStyle::new(if mac { 12.0 } else { 13.5 }, 400.0).lh(if mac { 17.0 } else { 19.0 })).color(t.text2).wrap().selectable(true).py(4.0));
            }
        }
    }
    if a.items.is_empty() {
        steps = steps.child(t.faint(if a.running() { "No steps yet." } else { "No steps." }));
    }
    c = c.child(col().w_full().gap(6.0).child(heading(t, "Steps")).child(steps));

    if !a.running() && !a.report.trim().is_empty() {
        c = c.child(col().key("agent-report").w_full().gap(6.0).child(heading(t, "Report")).child(crate::session::reply(&a.report, false, t, k)));
    }
    c
}

/// One step: its status, a one-line summary, its time; opens like a tool row to show the
/// arguments and output.
fn step_row(s: &App, e: &Event, res: Option<&Event>, live: bool, t: &Theme) -> El {
    let mac = t.mac();
    let p = &e.payload;
    let cid = str_of(&p["tool_call_id"]);
    let status = match res {
        Some(r) => str_of(&r.payload["status"]),
        None if live => "running".into(),
        None => "stopped".into(),
    };
    let open = s.expanded.contains(&cid);
    let toggle = cid.clone();
    let title = step_title(p);
    let running = res.is_none() && live;
    let mut head = row()
        .key(format!("agent-step-{cid}"))
        .w_full()
        .gap(8.0)
        .px(6.0)
        .py(if mac { 4.0 } else { 6.0 })
        .center_y()
        .radius(if mac { 5.0 } else { 6.0 })
        .role("button")
        .label(format!("{title}, {}", status_word(&status)))
        .focusable(true)
        .hover(Color::WHITE.alpha(0.05))
        .on_press(move |s: &mut App| {
            if !s.expanded.remove(&toggle) {
                s.expanded.insert(toggle.clone());
            }
        })
        .child(row().w(14.0).justify_center().child(status_mark(t, &status, 12.0)))
        .child(text(clip(&title, 160)).font(TextStyle::new(t.fs_mono_small, 400.0).mono()).color(if running { t.text } else { t.text2 }).truncate().shrink().min_w(0.0))
        .child(el().grow(1.0));
    if let Some(d) = res.and_then(|r| secs(e.ts, r.ts)) {
        head = head.child(text(d).font(TextStyle::new(if mac { 10.5 } else { 12.0 }, 400.0)).color(t.text3));
    }
    if open {
        head = head.fill(Color::WHITE.alpha(0.05));
    }
    let mut c = col().w_full().child(head);
    if open {
        let args = match &p["arguments"] {
            Value::Null => String::new(),
            v => serde_json::to_string_pretty(v).unwrap_or_default(),
        };
        let output = res.map(|r| str_of(&r.payload["output"])).unwrap_or_default();
        let mut body = col().w_full().gap(8.0).px(10.0).py(8.0).radius(6.0).fill(t.code_bg);
        if !args.is_empty() && !(p["kind"] == "execute" && !output.is_empty()) {
            body = body.child(pre(t, clip(&args, 3000)));
        }
        if !output.is_empty() {
            body = body.child(pre(t, clip(&output, 6000)));
        }
        if args.is_empty() && output.is_empty() {
            body = body.child(t.faint(if running { "Running…" } else { "No output." }));
        }
        c = c.child(col().w_full().pl(28.0).pt(2.0).pb(4.0).child(body));
    }
    c
}
