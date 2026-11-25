//! Side questions (`/btw`): the right panel's Side Questions tab ([`crate::panel`]), with a
//! tab per side conversation. Closing a tab hides it here only: the conversation stays in
//! the session's events.

use crate::icons as ic;
use crate::look::Theme;
use crate::model::{App, SideThread};
use crate::view::clip;
use rune::{col, icon, row, text, Color, El, ScrollSpec, TextStyle, Tx};
use rune_kit::Kit;

/// The pending tab (a new question waiting for its id) uses this key.
const PENDING: &str = "pending";

pub fn width(t: &Theme) -> f32 {
    if t.mac() {
        340.0
    } else {
        360.0
    }
}

/// The panel's Side Questions tab for `session`: a tab per conversation, the selected one,
/// and the follow-up field.
pub fn content(s: &App, session: &str, t: &Theme, k: &Kit) -> El {
    let mac = t.mac();
    let tabs = s.side_tabs(session);
    let pending = s.side_pending.get(session);
    let selected = if pending.is_some() { Some(PENDING.to_string()) } else { s.side_selected(session) };

    // Tabs.
    let mut bar = row().key("side-tabs").w_full().gap(if mac { 4.0 } else { 6.0 }).px(if mac { 8.0 } else { 8.0 }).py(if mac { 6.0 } else { 6.0 }).role("tablist").focus_group();
    for th in &tabs {
        let on = selected.as_deref() == Some(th.side_id.as_str());
        bar = bar.child(tab(t, session, &th.side_id, th.title(), on, th.streaming()));
    }
    if let Some(q) = pending {
        bar = bar.child(tab(t, session, PENDING, q, true, true));
    }
    if mac {
        bar = bar.border_sides([0.0, 0.0, 0.5, 0.0], t.sep);
    }

    // The selected conversation.
    let thread = tabs.iter().find(|th| selected.as_deref() == Some(th.side_id.as_str()));
    let body = match (thread, pending) {
        (_, Some(q)) => conversation(t, k, &SideThread { side_id: PENDING.into(), turns: vec![crate::model::SideTurn { question: q.clone(), ..Default::default() }], live: String::new() }),
        (Some(th), None) => conversation(t, k, th),
        (None, None) => col().child(t.small("No side questions.")),
    };
    let streaming = pending.is_some() || thread.is_some_and(|th| th.streaming());
    let content = col().key("side-content").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: true }).child(body.w_full().px(16.0).py(14.0));

    col().key("side-pane").w_full().grow(1.0).min_h(0.0).child(bar).child(content).child(follow_up(s, t, streaming || thread.is_none()))
}

fn tab(t: &Theme, session: &str, side_id: &str, title: &str, on: bool, busy: bool) -> El {
    let mac = t.mac();
    let (sess, id) = (session.to_string(), side_id.to_string());
    let (sess2, id2) = (sess.clone(), id.clone());
    let glyph = if busy {
        icon(&ic::SPINNER, 11.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9))
    } else {
        icon(&ic::BUBBLE, 11.0).color(if on { t.text } else { t.text2 })
    };
    let mut r = row()
        .key(format!("side-tab-{side_id}"))
        .h(if mac { 24.0 } else { 32.0 })
        .min_w(0.0)
        .shrink()
        .max_w(if mac { 170.0 } else { 180.0 })
        .gap(6.0)
        .pl(if mac { 8.0 } else { 10.0 })
        .pr(if mac { 4.0 } else { 4.0 })
        .center_y()
        .radius(t.btn_radius)
        .role("tab")
        .label(title.to_string())
        .help(title.to_string())
        .checked(on)
        .focusable(true)
        .on_press(move |s: &mut App| {
            if id != PENDING {
                s.side_tab.insert(sess.clone(), id.clone());
            }
        })
        .child(glyph)
        .child(text(clip(title, 60)).font(TextStyle::new(if mac { 12.0 } else { 13.5 }, if on && !mac { 700.0 } else { 400.0 })).color(if on { t.text } else { t.text2 }).truncate().shrink().min_w(0.0));
    if side_id != PENDING {
        r = r.child(
            row()
                .key(format!("side-close-{side_id}"))
                .size(18.0, 18.0)
                .center()
                .radius(t.btn_radius)
                .role("button")
                .label(format!("Close {}", clip(title, 40)))
                .help("Close Tab")
                .hover(Color::WHITE.alpha(0.1))
                .on_press(move |s: &mut App| s.close_side_tab(&sess2, &id2))
                .child(icon(&ic::CLOSE, 8.0).color(t.text2)),
        );
    }
    if on {
        r.fill(Color::WHITE.alpha(if mac { 0.10 } else { 0.10 }))
    } else {
        r.hover(Color::WHITE.alpha(0.05))
    }
}

/// The questions and answers of one side conversation.
fn conversation(t: &Theme, k: &Kit, th: &SideThread) -> El {
    let mac = t.mac();
    let mut c = col().gap(12.0);
    let n = th.turns.len();
    for (i, turn) in th.turns.iter().enumerate() {
        if !turn.question.is_empty() {
            c = c.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .items_start()
                    .px(11.0)
                    .py(8.0)
                    .radius(t.box_radius)
                    .fill(Color::WHITE.alpha(0.08))
                    .child(icon(&ic::BUBBLE, 13.0).color(t.text2).mt(2.0))
                    .child(text(turn.question.clone()).font(TextStyle::new(t.fs, 400.0).lh((t.fs * 1.4).round())).color(t.text).wrap().selectable(true).shrink().min_w(0.0)),
            );
        }
        let last = i + 1 == n;
        if !turn.answer.is_empty() {
            c = c.child(crate::session::reply(&turn.answer, false, t, k).key(format!("side-answer-{}-{i}", th.side_id)));
        }
        if last && !turn.ended {
            if th.live.is_empty() {
                c = c.child(row().key("side-thinking").gap(6.0).center_y().child(icon(&ic::SPINNER, 12.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9))).child(t.small("Answering…")));
            } else {
                c = c.child(crate::session::reply(&th.live, true, t, k).key("side-live"));
            }
        }
        if let Some(e) = &turn.error {
            c = c.child(text(e.clone()).font(t.font_small()).color(t.danger).wrap());
        }
    }
    c
}

/// Ask a follow-up in the selected conversation.
fn follow_up(s: &App, t: &Theme, busy: bool) -> El {
    let mac = t.mac();
    let input = rune::text_input(&s.side_prompt, TextStyle::new(t.fs, 400.0).lh((t.fs * 1.4).round()))
        .key("side-prompt")
        .label("Follow-up side question")
        .w_full()
        .color(t.text)
        .placeholder(if busy { "Answering…" } else { "Ask a follow-up…" }, if mac { t.text3 } else { t.text2 })
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.side_prompt)
        .on_submit(|s: &mut App, _: &mut Tx| s.side_follow_up());
    let ready = !busy && !s.side_prompt.text.trim().is_empty();
    let send = row()
        .key("side-send")
        .size(22.0, 22.0)
        .radius(11.0)
        .center()
        .fill(if ready { t.accent } else { Color::WHITE.alpha(0.12) })
        .role("button")
        .label("Ask")
        .disabled(!ready)
        .on_press(|s: &mut App| s.side_follow_up())
        .child(icon(&ic::SEND, 10.0).color(Color::WHITE));
    let mut field = row().w_full().gap(8.0).items_end().pl(12.0).pr(6.0).py(6.0).radius(t.box_radius).child(col().grow(1.0).min_w(0.0).py(2.0).child(rune::auto_grow(input, 1, 4))).child(send);
    field = if mac { field.fill(Color::WHITE.alpha(0.06)).border(0.5, Color::WHITE.alpha(0.16)) } else { field.fill(t.card) };
    if s.side_prompt.focused {
        field = field.box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() });
    }
    let mut c = col().key("side-follow-up").w_full().px(12.0).pt(8.0).pb(12.0).child(field);
    if mac {
        c = c.border_sides([0.5, 0.0, 0.0, 0.0], t.sep);
    }
    c
}
