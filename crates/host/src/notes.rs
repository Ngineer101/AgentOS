//! Notes: plain-text ideas and todos kept with a workspace for later.
//!
//! A note never runs anything by itself: it becomes a prompt only when the
//! user starts a session from it (`session.create` with `note_id`), so phones
//! may manage notes like sessions. Archiving a session started from a note
//! marks the note done.

use crate::bus::BusMsg;
use crate::ctx::Ctx;
use crate::util::now_ms;
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::*;

fn internal(e: anyhow::Error) -> RpcError {
    RpcError::internal(e.to_string())
}

/// The text as stored: trailing whitespace trimmed, never blank, not too long.
fn clean(text: &str) -> Result<String, RpcError> {
    let t = text.trim_end();
    if title_of(t).is_empty() {
        return Err(RpcError::invalid_params("a note needs some text"));
    }
    if t.chars().count() > MAX_NOTE_TEXT {
        return Err(RpcError::invalid_params(format!("a note has at most {MAX_NOTE_TEXT} characters")));
    }
    Ok(t.trim_start_matches(['\n', '\r']).to_string())
}

pub fn get(ctx: &Ctx, note_id: &str) -> Result<Note, RpcError> {
    ctx.store
        .note_get(note_id)
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found(format!("no note {note_id}")))
}

pub fn list(ctx: &Ctx, p: &NoteListParams) -> Result<Vec<Note>, RpcError> {
    ctx.store.note_list(p.workspace_id.as_deref()).map_err(internal)
}

/// Tell every client the note changed (or is gone).
pub fn publish(ctx: &Ctx, note_id: &str) {
    let note = ctx.store.note_get(note_id).ok().flatten();
    let removed = note.is_none();
    ctx.bus.publish(BusMsg::NoteChanged(NoteChanged { note_id: note_id.into(), note, removed }));
}

pub fn create(ctx: &Ctx, p: NoteCreateParams) -> Result<Note, RpcError> {
    let text = clean(&p.text)?;
    ctx.store
        .workspace_get(&p.workspace_id)
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found(format!("no workspace {}", p.workspace_id)))?;
    if ctx.store.note_count(&p.workspace_id).map_err(internal)? >= MAX_NOTES_PER_WORKSPACE {
        return Err(RpcError::invalid_params(format!(
            "a workspace holds at most {MAX_NOTES_PER_WORKSPACE} notes; delete some first"
        )));
    }
    let now = now_ms();
    let note = Note {
        note_id: crate::util::new_id("note"),
        workspace_id: p.workspace_id,
        text,
        done_at: None,
        session_ids: vec![],
        created_at: now,
        updated_at: now,
    };
    ctx.store.note_insert(&note).map_err(internal)?;
    publish(ctx, &note.note_id);
    Ok(note)
}

pub fn update(ctx: &Ctx, p: NoteUpdateParams) -> Result<Note, RpcError> {
    let mut note = get(ctx, &p.note_id)?;
    let before = note.clone();
    if let Some(text) = &p.text {
        note.text = clean(text)?;
    }
    match p.done {
        Some(true) if note.done_at.is_none() => note.done_at = Some(now_ms()),
        Some(false) => note.done_at = None,
        _ => {}
    }
    if note == before {
        return Ok(note);
    }
    note.updated_at = now_ms();
    ctx.store.note_save(&note).map_err(internal)?;
    publish(ctx, &note.note_id);
    Ok(note)
}

pub fn delete(ctx: &Ctx, note_id: &str) -> Result<(), RpcError> {
    if !ctx.store.note_remove(note_id).map_err(internal)? {
        return Err(RpcError::not_found(format!("no note {note_id}")));
    }
    publish(ctx, note_id);
    Ok(())
}

/// Before starting a session from a note: the note must be in the session's
/// workspace.
pub fn check_start(ctx: &Ctx, note_id: &str, workspace_id: &str) -> Result<(), RpcError> {
    let note = get(ctx, note_id)?;
    if note.workspace_id != workspace_id {
        return Err(RpcError::invalid_params("that note belongs to another workspace"));
    }
    Ok(())
}

/// A session was started from a note: list it there, and open the note
/// again if it was done (it's being worked on).
pub fn link_session(ctx: &Ctx, note_id: &str, session_id: &str) -> Result<(), RpcError> {
    ctx.store.note_link_session(note_id, session_id).map_err(internal)?;
    let mut note = get(ctx, note_id)?;
    if note.done_at.is_some() {
        note.done_at = None;
        note.updated_at = now_ms();
        ctx.store.note_save(&note).map_err(internal)?;
    }
    publish(ctx, note_id);
    Ok(())
}

/// A session was archived: the open notes it was started from are done.
pub fn session_archived(ctx: &Ctx, session_id: &str) {
    match ctx.store.notes_done_for_session(session_id) {
        Ok(ids) => {
            for id in ids {
                tracing::info!(session = session_id, note = %id, "note done: its session was archived");
                publish(ctx, &id);
            }
        }
        Err(e) => tracing::warn!(session = session_id, "could not mark its notes done: {e:#}"),
    }
}
