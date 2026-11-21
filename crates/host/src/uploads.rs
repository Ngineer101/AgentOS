//! Files clients send to the host: attachments for prompts and voice
//! recordings for transcription.
//!
//! A client sends a file in pieces (`upload.begin`, `upload.append`,
//! `upload.finish`) so it fits the relay's frames and can show progress.
//! Finished files live in `<data>/uploads/<id>/` (owner-only, outside every
//! sandbox) until a prompt claims them: the session copies them into its own
//! temp folder, which its agent can read.
//!
//! Images sent with a prompt are also kept, in `<data>/attachments/<session>/<id>/`,
//! so clients can show them again in the transcript (`attachment.read`).
//! Sessions are never deleted, so neither are these.

use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::*;
use base64::Engine;
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// Unfinished uploads idle this long are dropped.
const IDLE: Duration = Duration::from_secs(30 * 60);
/// Finished but never used uploads are deleted at startup after this long.
const UNCLAIMED: Duration = Duration::from_secs(24 * 60 * 60);
/// Most unfinished uploads at once (all clients together).
const MAX_LIVE: usize = 32;

struct Live {
    name: String,
    mime: String,
    size: u64,
    received: u64,
    file: std::fs::File,
    touched: Instant,
}

pub struct Uploads {
    root: PathBuf,
    /// Images sent with prompts, by session.
    kept: PathBuf,
    live: Mutex<HashMap<String, Live>>,
}

impl Uploads {
    /// Open the upload folder and delete stale leftovers.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let root = data_dir.join("uploads");
        crate::config::create_private_dir(&root)?;
        let kept = data_dir.join("attachments");
        crate::config::create_private_dir(&kept)?;
        let u = Uploads {
            root,
            kept,
            live: Mutex::new(HashMap::new()),
        };
        u.sweep();
        Ok(u)
    }

    fn sweep(&self) {
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return;
        };
        let now = SystemTime::now();
        for e in rd.flatten() {
            let p = e.path();
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| now.duration_since(t).unwrap_or_default() > UNCLAIMED)
                .unwrap_or(true);
            // Pieces of an upload the host was stopped during never finish.
            let partial = p.extension().is_some_and(|x| x == "part");
            if old || partial {
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
            }
        }
    }

    pub fn begin(&self, p: UploadBeginParams) -> Result<UploadBeginResult, RpcError> {
        if p.size > MAX_ATTACHMENT_BYTES {
            return Err(RpcError::invalid_params(format!(
                "{} is too large: files can be up to {} MB",
                p.name,
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        let name = safe_name(&p.name);
        let mime = p
            .mime
            .filter(|m| m.contains('/') && m.len() < 128 && !m.contains(['\n', '\r']))
            .unwrap_or_else(|| guess_mime(&name).to_string());
        let mut live = self.live.lock().unwrap();
        live.retain(|id, l| {
            let keep = l.touched.elapsed() < IDLE;
            if !keep {
                let _ = std::fs::remove_file(self.part_path(id));
            }
            keep
        });
        if live.len() >= MAX_LIVE {
            return Err(RpcError::invalid_state("too many uploads in progress"));
        }
        let id = crate::util::new_id("att");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode_private()
            .open(self.part_path(&id))
            .map_err(|e| RpcError::internal(format!("upload: {e}")))?;
        live.insert(
            id.clone(),
            Live {
                name,
                mime,
                size: p.size,
                received: 0,
                file,
                touched: Instant::now(),
            },
        );
        Ok(UploadBeginResult {
            upload_id: id,
            chunk_bytes: UPLOAD_CHUNK_BYTES,
        })
    }

    pub fn append(&self, p: UploadAppendParams) -> Result<UploadAppendResult, RpcError> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(p.data.as_bytes())
            .map_err(|_| RpcError::invalid_params("data is not base64"))?;
        if data.len() > UPLOAD_CHUNK_BYTES {
            return Err(RpcError::invalid_params("piece is larger than chunk_bytes"));
        }
        let mut live = self.live.lock().unwrap();
        let l = live
            .get_mut(&p.upload_id)
            .ok_or_else(|| RpcError::not_found("no such upload (it may have expired)"))?;
        if p.offset != l.received {
            // A retried piece that already arrived: say where we are.
            if p.offset + data.len() as u64 == l.received {
                return Ok(UploadAppendResult {
                    received: l.received,
                });
            }
            return Err(RpcError::invalid_params(format!(
                "offset {} does not match the {} bytes received",
                p.offset, l.received
            ))
            .with_data(json!({"received": l.received})));
        }
        if l.received + data.len() as u64 > l.size {
            return Err(RpcError::invalid_params("more bytes than the declared size"));
        }
        l.file
            .write_all(&data)
            .map_err(|e| RpcError::internal(format!("upload: {e}")))?;
        l.received += data.len() as u64;
        l.touched = Instant::now();
        Ok(UploadAppendResult {
            received: l.received,
        })
    }

    pub fn finish(&self, id: &str) -> Result<Attachment, RpcError> {
        let l = {
            let mut live = self.live.lock().unwrap();
            let l = live
                .get(id)
                .ok_or_else(|| RpcError::not_found("no such upload (it may have expired)"))?;
            if l.received != l.size {
                return Err(RpcError::invalid_state(format!(
                    "only {} of {} bytes arrived",
                    l.received, l.size
                )));
            }
            live.remove(id).unwrap()
        };
        let io = |e: std::io::Error| RpcError::internal(format!("upload: {e}"));
        l.file.sync_all().map_err(io)?;
        drop(l.file);
        let dir = self.root.join(id);
        crate::config::create_private_dir(&dir).map_err(|e| RpcError::internal(e.to_string()))?;
        std::fs::rename(self.part_path(id), dir.join("data")).map_err(io)?;
        let att = Attachment {
            attachment_id: id.to_string(),
            name: l.name,
            mime: l.mime,
            size: l.size,
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&att).unwrap()).map_err(io)?;
        Ok(att)
    }

    /// Store bytes the host has itself (a webhook's payload) as an upload.
    pub fn put(&self, name: &str, mime: &str, bytes: &[u8]) -> Result<Attachment, RpcError> {
        use base64::Engine as _;
        let b = self.begin(UploadBeginParams { name: name.into(), size: bytes.len() as u64, mime: Some(mime.into()) })?;
        let mut offset = 0u64;
        for piece in bytes.chunks(UPLOAD_CHUNK_BYTES) {
            self.append(UploadAppendParams {
                upload_id: b.upload_id.clone(),
                offset,
                data: base64::engine::general_purpose::STANDARD.encode(piece),
            })?;
            offset += piece.len() as u64;
        }
        self.finish(&b.upload_id)
    }

    /// A finished upload and the path of its bytes.
    pub fn get(&self, id: &str) -> Result<(Attachment, PathBuf), RpcError> {
        if !valid_id(id) {
            return Err(RpcError::not_found("no such attachment"));
        }
        let dir = self.root.join(id);
        let meta = std::fs::read(dir.join("meta.json"))
            .map_err(|_| RpcError::not_found(format!("no such attachment {id} (it may have expired)")))?;
        let att: Attachment =
            serde_json::from_slice(&meta).map_err(|e| RpcError::internal(e.to_string()))?;
        Ok((att, dir.join("data")))
    }

    /// Delete a finished upload.
    pub fn remove(&self, id: &str) {
        if valid_id(id) {
            let _ = std::fs::remove_dir_all(self.root.join(id));
        }
    }

    /// Move a finished upload into a session's kept attachments, once a prompt
    /// of the session has recorded it.
    pub fn keep(&self, session_id: &str, id: &str) -> std::io::Result<()> {
        if !valid_id(id) || !valid_session(session_id) {
            return Err(std::io::Error::other("bad id"));
        }
        let dir = self.kept.join(session_id);
        crate::config::create_private_dir(&dir).map_err(std::io::Error::other)?;
        std::fs::rename(self.root.join(id), dir.join(id))
    }

    /// A kept attachment of a session and the path of its bytes.
    pub fn kept(&self, session_id: &str, id: &str) -> Result<(Attachment, PathBuf), RpcError> {
        if !valid_id(id) || !valid_session(session_id) {
            return Err(RpcError::not_found("no such attachment"));
        }
        let dir = self.kept.join(session_id).join(id);
        let meta = std::fs::read(dir.join("meta.json"))
            .map_err(|_| RpcError::not_found(format!("no such attachment {id} in this session")))?;
        let att: Attachment =
            serde_json::from_slice(&meta).map_err(|e| RpcError::internal(e.to_string()))?;
        Ok((att, dir.join("data")))
    }

    /// The upload if no prompt has claimed it yet, else the session's kept copy.
    pub fn get_for(&self, session_id: &str, id: &str) -> Result<(Attachment, PathBuf), RpcError> {
        self.get(id).or_else(|_| self.kept(session_id, id))
    }

    /// A piece of a kept attachment, for `attachment.read`.
    pub fn read(&self, p: &AttachmentReadParams) -> Result<AttachmentReadResult, RpcError> {
        use std::io::{Read, Seek, SeekFrom};
        let (att, path) = self.kept(&p.session_id, &p.attachment_id)?;
        let io = |e: std::io::Error| RpcError::internal(format!("attachment: {e}"));
        let mut f = std::fs::File::open(&path).map_err(io)?;
        let size = f.metadata().map_err(io)?.len();
        if p.offset > size {
            return Err(RpcError::invalid_params(format!("offset {} is past the end ({size} bytes)", p.offset)));
        }
        f.seek(SeekFrom::Start(p.offset)).map_err(io)?;
        let mut buf = Vec::with_capacity(UPLOAD_CHUNK_BYTES.min((size - p.offset) as usize));
        f.take(UPLOAD_CHUNK_BYTES as u64).read_to_end(&mut buf).map_err(io)?;
        Ok(AttachmentReadResult {
            name: att.name,
            mime: att.mime,
            size,
            offset: p.offset,
            data: base64::engine::general_purpose::STANDARD.encode(&buf),
        })
    }

    fn part_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.part"))
    }
}

trait PrivateOpen {
    fn mode_private(&mut self) -> &mut Self;
}
impl PrivateOpen for std::fs::OpenOptions {
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(0o600)
    }
}

fn valid_session(id: &str) -> bool {
    id.starts_with("ses_")
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn valid_id(id: &str) -> bool {
    id.starts_with("att_")
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The last path part of a client-given name, without control characters,
/// never empty, never `.` or `..`, at most 120 bytes.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if s.len() > 120 {
        // Keep the extension when shortening.
        let ext = s
            .rsplit_once('.')
            .map(|(_, e)| e.to_string())
            .filter(|e| e.len() <= 10);
        let mut cut = 110;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        if let Some(e) = ext {
            s = format!("{s}.{e}");
        }
    }
    if s.is_empty() || s == "." || s == ".." || s.starts_with('.') && s.trim_matches('.').is_empty() {
        s = "file".into();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\Users\\me\\notes.txt"), "notes.txt");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("a\nb.png"), "ab.png");
        let long = format!("{}.png", "x".repeat(300));
        let s = safe_name(&long);
        assert!(s.len() <= 120 && s.ends_with(".png"), "{s}");
    }

    #[test]
    fn upload_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let u = Uploads::open(dir.path()).unwrap();
        let b = u
            .begin(UploadBeginParams {
                name: "dir/notes.md".into(),
                size: 11,
                mime: None,
            })
            .unwrap();
        let id = b.upload_id;
        u.append(UploadAppendParams {
            upload_id: id.clone(),
            offset: 0,
            data: b64(b"hello "),
        })
        .unwrap();
        // A retried piece is fine; a gap is not.
        assert_eq!(
            u.append(UploadAppendParams {
                upload_id: id.clone(),
                offset: 0,
                data: b64(b"hello "),
            })
            .unwrap()
            .received,
            6
        );
        assert!(u
            .append(UploadAppendParams {
                upload_id: id.clone(),
                offset: 9,
                data: b64(b"x"),
            })
            .is_err());
        assert!(u.finish(&id).is_err(), "incomplete");
        u.append(UploadAppendParams {
            upload_id: id.clone(),
            offset: 6,
            data: b64(b"world"),
        })
        .unwrap();
        assert!(u
            .append(UploadAppendParams {
                upload_id: id.clone(),
                offset: 11,
                data: b64(b"!"),
            })
            .is_err(), "past the size");
        let a = u.finish(&id).unwrap();
        assert_eq!(a.name, "notes.md");
        assert_eq!(a.mime, "text/markdown");
        let (a2, path) = u.get(&id).unwrap();
        assert_eq!(a, a2);
        assert_eq!(std::fs::read(path).unwrap(), b"hello world");
        assert!(u.get("att_../../x").is_err());
        u.remove(&id);
        assert!(u.get(&id).is_err());
    }

    #[test]
    fn too_large_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let u = Uploads::open(dir.path()).unwrap();
        assert!(u
            .begin(UploadBeginParams {
                name: "big.bin".into(),
                size: MAX_ATTACHMENT_BYTES + 1,
                mime: None,
            })
            .is_err());
    }
}
