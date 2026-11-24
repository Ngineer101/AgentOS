//! Notes driven in process, without the host's socket: editing them, starting
//! sessions from them, and archiving those sessions marking them done.

mod common;

use agenthost_host::bus::BusMsg;
use agenthost_host::{notes, Host};
use agenthost_proto::*;
use std::sync::Arc;

/// A host that isn't serving: no socket, no gateway, no relay.
fn open(root: &std::path::Path) -> Arc<Host> {
    let cfg = common::test_config(root);
    let host = Host::open(cfg).unwrap();
    for id in ["ws_a", "ws_b"] {
        let work = root.join(id);
        std::fs::create_dir_all(&work).unwrap();
        host.ctx
            .store
            .workspace_insert(&Workspace {
                workspace_id: id.into(),
                name: id.into(),
                path: work.display().to_string(),
                kind: WorkspaceKind::Folder,
                network: NetworkProfile::default(),
                connectors: vec![],
                created_at: 0,
                default_agent: None,
                default_profile_id: None,
            })
            .unwrap();
    }
    host
}

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

fn create(host: &Host, ws: &str, text: &str) -> Result<Note, jsonrpc::RpcError> {
    notes::create(&host.ctx, NoteCreateParams { workspace_id: ws.into(), text: text.into() })
}

fn update(host: &Host, id: &str, text: Option<&str>, done: Option<bool>) -> Note {
    notes::update(&host.ctx, NoteUpdateParams { note_id: id.into(), text: text.map(str::to_string), done }).unwrap()
}

fn start(note_id: Option<&str>, ws: &str) -> SessionCreateParams {
    SessionCreateParams { workspace_id: ws.into(), agent: AgentKind::Fake, profile_id: None, title: None, note_id: note_id.map(str::to_string) }
}

/// The note changes published since `rx` subscribed.
fn changes(rx: &mut tokio::sync::broadcast::Receiver<BusMsg>) -> Vec<NoteChanged> {
    let mut out = vec![];
    while let Ok(m) = rx.try_recv() {
        if let BusMsg::NoteChanged(c) = m {
            out.push(c);
        }
    }
    out
}

#[tokio::test]
async fn editing_notes() {
    let dir = tempdir();
    let host = open(dir.path());
    let mut rx = host.ctx.bus.subscribe();

    // Blank text and unknown workspaces are refused.
    assert!(create(&host, "ws_a", "  \n\t\n").is_err());
    assert!(create(&host, "ws_nope", "Idea").is_err());
    assert!(create(&host, "ws_a", &"x".repeat(MAX_NOTE_TEXT + 1)).is_err());

    let a = create(&host, "ws_a", "\n\nRework the settings page  \nGroup by topic\nand add search\n\n").unwrap();
    assert_eq!(a.text, "Rework the settings page  \nGroup by topic\nand add search");
    assert_eq!(a.title(), "Rework the settings page");
    assert_eq!(a.body(), "Group by topic\nand add search");
    let b = create(&host, "ws_a", "Try a dark icon").unwrap();
    let c = create(&host, "ws_b", "Elsewhere").unwrap();

    let ch = changes(&mut rx);
    assert_eq!(ch.iter().map(|c| c.note_id.as_str()).collect::<Vec<_>>(), [&a.note_id, &b.note_id, &c.note_id]);
    assert!(ch.iter().all(|c| c.note.is_some() && !c.removed));

    // One workspace's notes, open ones newest first.
    let ws_a = notes::list(&host.ctx, &NoteListParams { workspace_id: Some("ws_a".into()) }).unwrap();
    assert_eq!(ws_a.iter().map(|n| &n.note_id).collect::<Vec<_>>(), [&b.note_id, &a.note_id]);
    assert_eq!(notes::list(&host.ctx, &NoteListParams::default()).unwrap().len(), 3);

    // Done notes come after the open ones; opening one again moves it back.
    let a2 = update(&host, &a.note_id, None, Some(true));
    assert!(a2.done_at.is_some());
    let done_at = a2.done_at;
    // Marking it done again keeps when it was done, and publishes nothing.
    changes(&mut rx);
    assert_eq!(update(&host, &a.note_id, None, Some(true)).done_at, done_at);
    assert!(changes(&mut rx).is_empty());
    let b2 = update(&host, &b.note_id, Some("Try a dark icon\nwith a glass rim"), None);
    assert_eq!(b2.body(), "with a glass rim");
    assert!(b2.updated_at >= b.updated_at);
    let ws_a = notes::list(&host.ctx, &NoteListParams { workspace_id: Some("ws_a".into()) }).unwrap();
    assert_eq!(ws_a.iter().map(|n| &n.note_id).collect::<Vec<_>>(), [&b.note_id, &a.note_id]);
    assert!(notes::update(&host.ctx, NoteUpdateParams { note_id: b.note_id.clone(), text: Some(" ".into()), done: None }).is_err());
    assert!(update(&host, &a.note_id, None, Some(false)).done_at.is_none());

    // Deleting tells the clients; a second delete fails.
    changes(&mut rx);
    notes::delete(&host.ctx, &c.note_id).unwrap();
    assert_eq!(changes(&mut rx), [NoteChanged { note_id: c.note_id.clone(), note: None, removed: true }]);
    assert!(notes::delete(&host.ctx, &c.note_id).is_err());

    // A removed workspace's notes aren't listed (and come back with it).
    host.ctx.store.workspace_remove("ws_a").unwrap();
    assert!(notes::list(&host.ctx, &NoteListParams::default()).unwrap().is_empty());
}

#[tokio::test]
async fn sessions_from_notes() {
    let dir = tempdir();
    let host = open(dir.path());
    let note = create(&host, "ws_a", "Add CSV export\nFor the reports screen").unwrap();
    let other = create(&host, "ws_a", "Unrelated").unwrap();

    // Only in the note's own workspace, and only for a note that exists.
    assert!(host.sessions.create(start(Some(&note.note_id), "ws_b"), None).await.is_err());
    assert!(host.sessions.create(start(Some("note_nope"), "ws_a"), None).await.is_err());

    // Starting a session lists it on the note, and opens a done note again.
    update(&host, &note.note_id, None, Some(true));
    let mut rx = host.ctx.bus.subscribe();
    let s1 = host.sessions.create(start(Some(&note.note_id), "ws_a"), None).await.unwrap();
    let n = notes::get(&host.ctx, &note.note_id).unwrap();
    assert_eq!(n.session_ids, [s1.session_id.clone()]);
    assert!(n.done_at.is_none());
    assert!(changes(&mut rx).iter().any(|c| c.note.as_ref() == Some(&n)));

    // Archiving a session not started from a note changes no note.
    let plain = host.sessions.create(start(None, "ws_a"), None).await.unwrap();
    host.sessions.set_archived(&plain.session_id, true).await.unwrap();
    assert!(notes::get(&host.ctx, &note.note_id).unwrap().done_at.is_none());

    // Archiving the note's session marks the note done, and only that note.
    let s2 = host.sessions.create(start(Some(&note.note_id), "ws_a"), None).await.unwrap();
    changes(&mut rx);
    host.sessions.set_archived(&s1.session_id, true).await.unwrap();
    let n = notes::get(&host.ctx, &note.note_id).unwrap();
    assert!(n.done_at.is_some());
    assert_eq!(n.session_ids, [s1.session_id.clone(), s2.session_id.clone()]);
    assert!(notes::get(&host.ctx, &other.note_id).unwrap().done_at.is_none());
    assert!(changes(&mut rx).iter().any(|c| c.note_id == note.note_id && c.note.as_ref().is_some_and(|n| n.done_at.is_some())));

    // Opened again by hand, archiving the second session marks it done again;
    // unarchiving leaves it as it is.
    update(&host, &note.note_id, None, Some(false));
    host.sessions.set_archived(&s2.session_id, true).await.unwrap();
    assert!(notes::get(&host.ctx, &note.note_id).unwrap().done_at.is_some());
    host.sessions.set_archived(&s2.session_id, false).await.unwrap();
    assert!(notes::get(&host.ctx, &note.note_id).unwrap().done_at.is_some());

    // Deleting the note leaves its sessions alone.
    notes::delete(&host.ctx, &note.note_id).unwrap();
    assert!(host.ctx.store.session_get(&s2.session_id).unwrap().is_some());
}
