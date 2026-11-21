//! Attachments driven in process, without the host's socket: a prompt with
//! only an image, and the image read back afterwards (`attachment.read`).

mod common;

use agenthost_host::Host;
use agenthost_proto::*;
use base64::Engine;
use std::sync::Arc;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

fn open(root: &std::path::Path) -> (Arc<Host>, String) {
    let cfg = common::test_config(root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let host = Host::open(cfg).unwrap();
    let ws = Workspace {
        workspace_id: "ws_test".into(),
        name: "work".into(),
        path: work.display().to_string(),
        kind: WorkspaceKind::Folder,
        network: NetworkProfile::default(),
        connectors: vec![],
        created_at: 0,
        default_agent: None,
        default_profile_id: None,
    };
    host.ctx.store.workspace_insert(&ws).unwrap();
    (host, ws.workspace_id)
}

fn upload(host: &Host, name: &str, mime: &str, bytes: &[u8]) -> Attachment {
    host.ctx.uploads.put(name, mime, bytes).unwrap()
}

fn b64(b: &[u8]) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(b).unwrap()
}

async fn idle_after(host: &Host, sid: &str, kind: EventType, n: usize) -> Vec<Event> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let ev = host.ctx.store.events_after(sid, 0, 1000).unwrap();
        let s = host.ctx.store.session_get(sid).unwrap().unwrap();
        if s.state == SessionState::Idle && ev.iter().filter(|e| e.kind == kind).count() >= n {
            return ev;
        }
        assert!(tokio::time::Instant::now() < deadline, "{ev:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_image_alone_is_sent_and_kept() {
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let s = host
        .sessions
        .create(SessionCreateParams { workspace_id: ws, agent: AgentKind::Fake, profile_id: None, title: None }, None)
        .await
        .unwrap();
    let sid = s.session_id.clone();
    // Bigger than one piece, so reading it back takes more than one call.
    let png: Vec<u8> = b"\x89PNG\r\n\x1a\n".iter().copied().chain((0..UPLOAD_CHUNK_BYTES + 1000).map(|i| (i % 251) as u8)).collect();
    let img = upload(&host, "Pasted image.png", "image/png", &png);
    let doc = upload(&host, "notes.txt", "text/plain", b"hello");
    let prompt = |id: &str, text: &str, atts: Vec<String>| SessionPromptParams {
        session_id: sid.clone(),
        prompt_id: id.into(),
        text: text.into(),
        attachments: atts,
    };
    let r = host.sessions.prompt(prompt("p1", "", vec![img.attachment_id.clone(), doc.attachment_id.clone()]), "local").unwrap();
    assert!(r.accepted);
    // The same prompt again (a retry): recorded once, though its files have moved.
    let again = host.sessions.prompt(prompt("p1", "", vec![img.attachment_id.clone(), doc.attachment_id.clone()]), "local").unwrap();
    assert!(!again.accepted);

    let ev = idle_after(&host, &sid, EventType::TurnEnd, 1).await;
    let reply = ev.iter().find(|e| e.kind == EventType::AgentMessage).expect("the agent answered");
    let text = reply.payload["text"].as_str().unwrap();
    assert!(text.contains("Pasted image.png (image/png") && text.contains("notes.txt (text/plain"), "{text}");
    assert!(!text.contains("no longer available"), "{text}");
    // Named by the image until a message has words.
    assert_eq!(host.ctx.store.session_get(&sid).unwrap().unwrap().title.as_deref(), Some("Pasted image.png"));

    // The image reads back in pieces; the text file was not kept.
    let read = |id: &str, offset: u64| host.ctx.uploads.read(&AttachmentReadParams { session_id: sid.clone(), attachment_id: id.into(), offset });
    let mut got = vec![];
    loop {
        let r = read(&img.attachment_id, got.len() as u64).unwrap();
        assert_eq!((r.name.as_str(), r.mime.as_str(), r.size), ("Pasted image.png", "image/png", png.len() as u64));
        let piece = b64(r.data.as_bytes());
        if piece.is_empty() {
            break;
        }
        assert!(piece.len() <= UPLOAD_CHUNK_BYTES);
        got.extend(piece);
    }
    assert_eq!(got, png);
    assert!(read(&doc.attachment_id, 0).is_err(), "only images are kept");
    assert!(read(&img.attachment_id, png.len() as u64 + 1).is_err());
    // Only through the session that sent it, and never a path.
    let other = host.ctx.uploads.read(&AttachmentReadParams { session_id: "ses_other".into(), attachment_id: img.attachment_id.clone(), offset: 0 });
    assert!(other.is_err());
    let sneaky = host.ctx.uploads.read(&AttachmentReadParams { session_id: format!("../uploads/{sid}"), attachment_id: img.attachment_id.clone(), offset: 0 });
    assert!(sneaky.is_err());

    // A second image-only message still reaches the agent (it reads the kept copy).
    let img2 = upload(&host, "Pasted image 2.png", "image/png", b"\x89PNG\r\n\x1a\nxx");
    host.sessions.prompt(prompt("p2", "  ", vec![img2.attachment_id.clone()]), "local").unwrap();
    let ev = idle_after(&host, &sid, EventType::TurnEnd, 2).await;
    let last = ev.iter().rev().find(|e| e.kind == EventType::AgentMessage).unwrap();
    assert!(last.payload["text"].as_str().unwrap().contains("Pasted image 2.png (image/png"), "{last:?}");
    assert!(read(&img2.attachment_id, 0).is_ok());
    host.sessions.shutdown();
}
