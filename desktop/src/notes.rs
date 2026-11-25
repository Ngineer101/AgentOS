//! Notes: plain-text ideas and todos kept with a workspace for later. The screen lists every
//! workspace's open notes (done ones on request) beside the chosen one's editor; the first
//! line is the title. Start Session opens the New Session sheet in the note's workspace and
//! puts the note in the new session's composer; archiving that session marks the note done.

use crate::automations::{pick_menu, picker, Opt};
use crate::icons as ic;
use crate::look::{Btn, Theme};
use crate::model::{App, Status};
use crate::settings::footnote;
use crate::sheet::{scrim, tilde};
use crate::view::{clip, now_ms, status_glyph, when_label};
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, ScrollSpec, TextStyle, Tx};

const WS_PICKER: &str = "note-ws";

/// The toolbar's subtitle on this screen.
pub fn summary(s: &App) -> String {
    let open = s.notes.iter().filter(|n| n.done_at.is_none()).count();
    match open {
        0 => "No open notes".into(),
        n => format!("{n} open note{}", if n == 1 { "" } else { "s" }),
    }
}

fn ws_name(s: &App, id: &str) -> String {
    match s.workspace(id) {
        Some(w) if w.kind == WorkspaceKind::Scratch => "Quick tasks".into(),
        Some(w) => w.name.clone(),
        None => "A removed workspace".into(),
    }
}

/// The note's first line after the title, for the list.
fn preview(n: &Note) -> Option<String> {
    n.body().lines().map(str::trim).find(|l| !l.is_empty()).map(|l| clip(l, 140))
}

pub fn screen(s: &App, t: &Theme) -> El {
    if s.info.is_some() && !s.host_has_notes() {
        return col()
            .key("notes")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .center()
            .gap(10.0)
            .px(24.0)
            .child(icon(&ic::NOTE, 40.0).color(t.text3))
            .child(text("Update AgentOS").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
            .child(t.small("This host has no notes. Update agenthostd to keep ideas and todos with your workspaces.").wrap().align(rune::Align::Center).max_w(420.0));
    }
    let mac = t.mac();
    let head = row()
        .w_full()
        .h(if mac { 40.0 } else { 46.0 })
        .px(if mac { 16.0 } else { 14.0 })
        .gap(8.0)
        .center_y()
        .child(t.section_title(if mac { "All notes" } else { "All Notes" }))
        .child(el().grow(1.0))
        .child(t.icon_button(&ic::PLUS, if mac { 14.0 } else { 16.0 }, "New Note", |s: &mut App, _: &mut Tx| s.new_note(None)).key("new-note"));
    let mut left = col()
        .key("note-pane")
        .w(if mac { 300.0 } else { 320.0 })
        .h_full()
        .min_h(0.0)
        .child(head)
        .child(col().w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list(s, t)));
    let done = s.notes.iter().filter(|n| n.done_at.is_some()).count();
    if done > 0 {
        let on = s.notes_show_done;
        left = left.child(
            row()
                .key("show-done")
                .w_full()
                .px(if mac { 16.0 } else { 14.0 })
                .py(if mac { 10.0 } else { 12.0 })
                .gap(8.0)
                .center_y()
                .border_sides([0.5, 0.0, 0.0, 0.0], t.sep)
                .child(t.checkbox(on, "Show done notes", move |s: &mut App| s.notes_show_done = !on).key("show-done-box"))
                .child(t.small(format!("Show done ({done})"))),
        );
    }
    left = if mac { left.fill(Color::WHITE.alpha(0.02)).border_sides([0.0, 0.5, 0.0, 0.0], t.sep) } else { left.border_sides([0.0, 1.0, 0.0, 0.0], Color::BLACK.alpha(0.35)) };
    let detail = col()
        .key("note-detail")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(row().w_full().px(24.0).pt(20.0).pb(28.0).justify_center().child(editor(s, t).w_full().max_w(t.column)));
    let mut screen = row().key("notes").w_full().grow(1.0).min_h(0.0).child(left).child(detail);
    if let Some(m) = pick_menu(s, t, WS_PICKER, workspace_options(s), App::pick_note_ws) {
        screen = screen.child(m);
    }
    screen
}

/// The notes by workspace (in the sidebar's order), each workspace under its name with a
/// "+" for a new note there, shown on hover.
fn list(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let now = now_ms();
    let mut list = col().key("note-list").w_full().px(if mac { 10.0 } else { 6.0 }).pb(8.0).gap(if mac { 2.0 } else { 4.0 }).role("list").focus_group();
    let mut any = false;
    for w in &s.workspaces {
        let notes: Vec<&Note> = s.notes.iter().filter(|n| n.workspace_id == w.workspace_id && (s.notes_show_done || n.done_at.is_none())).collect();
        let writing = s.new_note_ws.as_deref() == Some(w.workspace_id.as_str());
        if notes.is_empty() && !writing {
            continue;
        }
        any = true;
        list = list.child(group_head(s, t, &w.workspace_id));
        if writing {
            list = list.child(new_row(s, t));
        }
        for n in notes {
            list = list.child(note_row(s, t, n, now));
        }
    }
    if !any {
        let msg = if s.notes.is_empty() { "No notes yet. Keep ideas and todos here until they're ready to start." } else { "No open notes." };
        list = list.child(t.small(msg).wrap().px(10.0).pt(8.0));
    }
    list
}

fn group_head(s: &App, t: &Theme, ws: &str) -> El {
    let mac = t.mac();
    let name = ws_name(s, ws);
    let id = ws.to_string();
    let size = if mac { 20.0 } else { 26.0 };
    row()
        .key(format!("note-group-{ws}"))
        .w_full()
        .h(if mac { 28.0 } else { 34.0 })
        .pl(10.0)
        .pr(if mac { 4.0 } else { 3.0 })
        .pt(6.0)
        .center_y()
        // The "+" shows while the pointer is on the heading: the pane's own "+" is the one
        // always in view.
        .hover_group()
        .child(text(name.clone()).font(if mac { TextStyle::new(11.0, 600.0) } else { TextStyle::new(12.5, 700.0) }).color(if mac { t.text3 } else { t.text2 }).truncate().grow(1.0).min_w(0.0))
        .child(
            row()
                .key(format!("note-new-{ws}"))
                .size(size, size)
                .center()
                .radius(if mac { 5.0 } else { size / 2.0 })
                .role("button")
                .label(format!("New Note in {name}"))
                .help(format!("New Note in {name}"))
                .hover(Color::WHITE.alpha(0.10))
                .group_opacity(0.0, 1.0)
                .on_press(move |s: &mut App| s.new_note(Some(&id)))
                .child(icon(&ic::PLUS_SMALL, if mac { 11.0 } else { 12.0 }).color(if mac { t.text2 } else { t.text })),
        )
}

/// The new note being written, at the top of its workspace's notes.
fn new_row(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let title = title_of(&s.note_edit.text);
    let label = if title.is_empty() { "New Note".to_string() } else { clip(title, 120) };
    row()
        .key("note-row-new")
        .w_full()
        .gap(if mac { 9.0 } else { 10.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .py(if mac { 7.0 } else { 9.0 })
        .center_y()
        .radius(t.row_radius)
        .role("listitem")
        .label(label.clone())
        .fill(if mac { t.accent.alpha(0.22) } else { t.row_selected })
        .child(row().size(16.0, 16.0).center().child(icon(&ic::COMPOSE, 13.0).color(t.text2)))
        .child(text(label).font(t.font()).color(if title.is_empty() { t.text2 } else { t.text }).truncate().grow(1.0).min_w(0.0))
}

fn note_row(s: &App, t: &Theme, n: &Note, now: i64) -> El {
    let mac = t.mac();
    let selected = s.selected_note().is_some_and(|x| x.note_id == n.note_id);
    let done = n.done_at.is_some();
    let (ink, sub) = if done { (t.text2, t.text3) } else { (t.text, t.text2) };
    let mut names = col().grow(1.0).min_w(0.0).gap(1.0).child(text(clip(n.title(), 120)).font(TextStyle { weight: if selected && !mac { 700.0 } else { 400.0 }, ..t.font() }).color(ink).truncate());
    if let Some(p) = preview(n) {
        names = names.child(text(p).font(t.font_small()).color(sub).truncate());
    }
    let mut meta = vec![when_label(n.done_at.unwrap_or(n.created_at), now, s.look)];
    if done {
        meta[0] = format!("Done {}", meta[0]);
    }
    if !n.session_ids.is_empty() {
        meta.push(format!("{} session{}", n.session_ids.len(), if n.session_ids.len() == 1 { "" } else { "s" }));
    }
    names = names.child(text(meta.join(" · ")).font(t.font_small()).color(t.text3).truncate());
    let (ids, idd) = (n.note_id.clone(), n.note_id.clone());
    let mut r = row()
        .key(format!("note-{}", n.note_id))
        .w_full()
        .gap(if mac { 9.0 } else { 10.0 })
        .px(if mac { 10.0 } else { 12.0 })
        .py(if mac { 7.0 } else { 9.0 })
        .items_start()
        .radius(t.row_radius)
        .role("listitem")
        .label(format!("{}{}", n.title(), if done { ", done" } else { "" }))
        .focusable(true)
        .on_press(move |s: &mut App| s.select_note(&ids))
        .child(t.checkbox(done, &format!("{} done", n.title()), move |s: &mut App| s.set_note_done(&idd, !done)).key(format!("note-done-{}", n.note_id)).mt(2.0))
        .child(names);
    r = if selected { r.fill(if mac { t.accent.alpha(0.22) } else { t.row_selected }) } else { r.hover(t.row_hover) };
    r
}

fn workspace_options(s: &App) -> Vec<Opt> {
    let mut ws: Vec<&Workspace> = s.workspaces.iter().collect();
    ws.sort_by_key(|w| w.kind == WorkspaceKind::Scratch);
    ws.into_iter()
        .map(|w| Opt {
            id: w.workspace_id.clone(),
            title: if w.kind == WorkspaceKind::Scratch { "Quick tasks".into() } else { format!("{} · {}", w.name, tilde(&w.path)) },
            checked: s.new_note_ws.as_deref() == Some(w.workspace_id.as_str()),
            enabled: true,
        })
        .collect()
}

/// The editor: the shown note (or the new one), its actions, and the sessions started from it.
fn editor(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let note = s.selected_note();
    let new_ws = s.new_note_ws.clone();
    if note.is_none() && new_ws.is_none() {
        return col()
            .key("note-none")
            .w_full()
            .pt(60.0)
            .center()
            .gap(10.0)
            .child(icon(&ic::NOTE, 40.0).color(t.text3))
            .child(text("No Note Selected").font(TextStyle::new(15.0, t.w_bold)).color(t.text))
            .child(t.small("A note keeps an idea or a todo with its workspace until it's ready: the first line is its title. Start a session from it when you are.").wrap().align(rune::Align::Center).max_w(420.0))
            .child(row().pt(6.0).child(t.button("New Note", Btn::Primary, None, |s| s.new_note(None)).key("empty-new-note")));
    }
    let dirty = s.note_dirty();
    // The header: where it lives, then what can be done with it.
    let head = match (note, &new_ws) {
        (Some(n), _) => {
            let (idd, idg, idx) = (n.note_id.clone(), n.note_id.clone(), n.note_id.clone());
            let done = n.done_at.is_some();
            let now = now_ms();
            let when = match n.done_at {
                Some(d) => format!("Done {}", when_label(d, now, s.look)),
                None => format!("Added {}", when_label(n.created_at, now, s.look)),
            };
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(t.small(format!("{} · {when}", ws_name(s, &n.workspace_id))).truncate().grow(1.0).min_w(0.0))
                .gap(if mac { 2.0 } else { 4.0 })
                .child(t.icon_button(if done { &ic::REOPEN } else { &ic::CHECK }, if mac { 14.0 } else { 16.0 }, if done { "Reopen" } else { "Mark Done" }, move |s: &mut App, _: &mut Tx| s.set_note_done(&idd, !done)).key("note-toggle-done"))
                .child(t.icon_button_tinted(&ic::TRASH, if mac { 14.0 } else { 16.0 }, "Delete…", t.danger, move |s: &mut App, _: &mut Tx| s.delete_note(&idx)).key("delete-note"))
                .child(t.icon_button_tinted(&ic::PLAY, if mac { 13.0 } else { 15.0 }, "Start Session: this note becomes its first message", t.accent_text, move |s: &mut App, _: &mut Tx| s.start_from_note(&idg)).key("note-start"))
        }
        (None, Some(ws)) => row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(t.small("New note in"))
            .child(picker(t, WS_PICKER, &ws_name(s, ws), workspace_options(s), App::pick_note_ws))
            .child(el().grow(1.0)),
        (None, None) => el(),
    };
    let input = rune::text_input(&s.note_edit, TextStyle::new(t.fs, 400.0).lh((t.fs * 1.5).round()))
        .key("note-text")
        .label("Note")
        .w_full()
        .color(t.text)
        .placeholder("What's the idea? The first line is the title.", t.text3)
        .caret(t.text, t.accent.alpha(0.35))
        .on_edit(|s: &mut App| &mut s.note_edit)
        // Return starts a new line, as in any editor (a field submits on it by default).
        .on_submit(|s: &mut App, _: &mut Tx| s.note_edit.insert("\n"))
        // ⌘↩ / Ctrl+Enter saves; the window's other shortcuts still work here.
        .on_input_key(|s: &mut App, ev: &rune::KeyEvent, tx: &mut Tx| (ev.mods.alt || ev.mods.cmd || ev.mods.ctrl) && crate::commands::on_key(s, ev, tx));
    let mut field = row().w_full().px(if mac { 10.0 } else { 12.0 }).py(if mac { 8.0 } else { 10.0 }).items_start().radius(if mac { 6.0 } else { 8.0 }).fill(t.field).child(rune::auto_grow(input, 10, 40));
    if mac {
        field = field.border(0.5, t.field_border);
    }
    if s.note_edit.focused {
        field = field.box_shadow(rune::Shadow { spread: 3.0, color: t.focus, ..Default::default() });
    }
    let save_label = if new_ws.is_some() { "Add Note" } else { "Save" };
    let keys = if s.look.mac() { "⌘↩" } else { "Ctrl+Enter" };
    let save = t
        .button(save_label, if dirty { Btn::Primary } else { Btn::Default }, None, |s| s.save_note())
        .key("note-save")
        .help(format!("{save_label} ({keys})"))
        .disabled(!dirty)
        .opacity(if dirty { 1.0 } else { 0.5 });
    let status = if s.note_saving {
        "Saving…".to_string()
    } else if dirty {
        "Not saved: it saves when you pick another note too.".to_string()
    } else if note.is_some() {
        "Saved".to_string()
    } else {
        String::new()
    };
    let foot = row().w_full().gap(10.0).center_y().child(t.faint(status).grow(1.0).min_w(0.0)).child(save);
    let mut c = col().key("note").w_full().gap(if mac { 10.0 } else { 12.0 }).child(head).child(field).child(foot);
    if let Some(n) = note {
        c = c.child(el().h(8.0)).child(sessions(s, t, n));
    }
    c.child(footnote(t, "Start Session opens the note in a new session's message box, to edit before you send it. Archiving that session marks the note done."))
}

/// The sessions started from the note, newest first; each opens on click.
fn sessions(s: &App, t: &Theme, n: &Note) -> El {
    let mac = t.mac();
    let mut rows = vec![];
    for id in n.session_ids.iter().rev() {
        let Some(x) = s.session(id) else {
            rows.push(row().w_full().px(if mac { 12.0 } else { 14.0 }).py(if mac { 8.0 } else { 11.0 }).child(t.small("A session that's no longer listed")));
            continue;
        };
        let archived = x.archived_at.is_some();
        let sid = id.clone();
        let st = Status::of(x.state);
        let detail = if archived { "Archived".to_string() } else { s.session_context(x) };
        rows.push(
            row()
                .key(format!("note-session-{id}"))
                .w_full()
                .gap(if mac { 10.0 } else { 12.0 })
                .px(if mac { 12.0 } else { 14.0 })
                .py(if mac { 8.0 } else { 11.0 })
                .center_y()
                .role("button")
                .label(format!("Open {}", s.session_title(x)))
                .focusable(true)
                .hover(Color::WHITE.alpha(0.04))
                .on_press(move |s: &mut App| s.open_session(&sid))
                .child(row().size(14.0, 14.0).center().child(status_glyph(t, st, false)))
                .child(col().grow(1.0).min_w(0.0).gap(1.0).child(t.label(s.session_title(x)).truncate()).child(t.small(detail).truncate()))
                .child(icon(&ic::CHEVRON_R, 10.0).color(t.text3)),
        );
    }
    if rows.is_empty() {
        rows.push(row().w_full().px(if mac { 12.0 } else { 14.0 }).py(if mac { 8.0 } else { 11.0 }).child(t.small("None yet. Start Session begins one with this note.")));
    }
    col().key("note-sessions").w_full().gap(if mac { 6.0 } else { 10.0 }).child(t.group_title("Sessions")).child(t.group(rows, if mac { 34.0 } else { 0.0 }))
}

/// Delete… asks first.
pub fn confirm(s: &App, t: &Theme) -> Option<El> {
    let id = s.confirm_delete_note.clone()?;
    let mac = t.mac();
    let title = s.note(&id).map(|n| clip(n.title(), 60)).unwrap_or_default();
    let title = format!("Delete \u{201c}{title}\u{201d}?");
    let cancel = |s: &mut App| s.confirm_delete_note = None;
    let go = t.button("Delete", Btn::Destructive, None, move |s| s.delete_note(&id)).key("confirm-delete-note").default_button();
    let buttons = row().w_full().gap(8.0).child(el().grow(1.0)).child(t.button("Cancel", Btn::Default, None, cancel).key("cancel-delete-note")).child(go);
    let mut panel = col()
        .key("delete-note-alert")
        .role("alertdialog")
        .label(title.clone())
        .w(if mac { 340.0 } else { 380.0 })
        .gap(if mac { 12.0 } else { 16.0 })
        .padding(if mac { 20.0 } else { 24.0 })
        .fill(t.sheet)
        .shadow(20.0, 50.0, Color::BLACK.alpha(0.5))
        .modal(true, cancel)
        .child(text(title).font(TextStyle::new(if mac { 13.0 } else { 16.0 }, 700.0)).color(t.text).wrap())
        .child(t.small("The note is gone for good. Sessions started from it stay.").wrap())
        .child(buttons);
    panel = if mac { panel.radius(12.0).border(0.5, Color::WHITE.alpha(0.18)) } else { panel.radius(14.0).border(1.0, Color::BLACK.alpha(0.6)) };
    Some(scrim("confirm-note-layer", 0.32, cancel).center().child(panel))
}
