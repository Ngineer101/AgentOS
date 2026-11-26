//! Files agents send the user: `agenthost__send_file`, and the browser's
//! recordings (`agenthost__browser_record_stop`).
//!
//! A sent file is kept with the session's attachments (outside every
//! sandbox) and recorded as an `agent.file` event, so every client the user
//! has, paired ones too, shows it in the transcript and reads it with
//! `attachment.read`. Sending needs no approval: the file only goes to the
//! user. It must come from the session's own folders, so an agent can't
//! pick up files elsewhere on the computer.

use super::*;
use crate::cron::LocalTime;

/// Send a file from the session's folder to the user.
pub const SEND_FILE_TOOL: &str = "agenthost__send_file";

pub(super) fn send_file_def() -> Value {
    json!({
        "name": SEND_FILE_TOOL,
        "description": format!(
            "[AgentOS] Send the user a file from your session's folder or your temp folder ($TMPDIR): a screenshot, a report, a log, a video. \
             It shows in this conversation on their desktop and phone, where they can open it; images and videos show inline. \
             Use it when a file shows your result better than words. At most {} MB. To record the browser, use browser_record_start and \
             browser_record_stop instead (that sends the video itself).",
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "The file: absolute, or relative to your session's folder."},
                "caption": {"type": "string", "description": "A sentence for the user about what it shows (optional)."},
                "name": {"type": "string", "description": "The name the user sees (optional; the file's own name by default)."}
            },
            "required": ["path"]
        },
        "annotations": {"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false, "title": "Send the user a file"}
    })
}

/// A file on its way to the user.
pub(super) struct Outgoing<'a> {
    pub name: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
    pub kind: AgentFileKind,
    pub caption: Option<String>,
    /// A JPEG still, for a video.
    pub poster: Option<&'a [u8]>,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub tool_call_id: &'a str,
}

/// A caption as sent: one paragraph's worth, trimmed.
fn caption(v: &Value) -> Option<String> {
    let c = v.as_str()?.trim();
    (!c.is_empty()).then(|| truncate(c, 2000).0)
}

/// The size in words.
fn size_text(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// A recording's file name, by local time: "Browser recording 2026-11-26 14.05.mp4".
fn recording_name(now_secs: i64, tz: &dyn LocalTime) -> String {
    let t = tz.local(now_secs);
    format!("Browser recording {}-{:02}-{:02} {:02}.{:02}.mp4", t.year, t.month, t.mday, t.hour, t.min)
}

/// `path` if it is a file inside one of `roots` (after following links).
pub(super) fn resolve_in(path: &str, base: &std::path::Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    let p = std::path::Path::new(path);
    let p = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let real = std::fs::canonicalize(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let inside = roots.iter().filter_map(|r| std::fs::canonicalize(r).ok()).any(|r| real.starts_with(&r));
    if !inside {
        let shown: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
        return Err(format!("{} is outside your session's folders; send a file from {}", p.display(), shown.join(" or ")));
    }
    let meta = std::fs::metadata(&real).map_err(|e| format!("{}: {e}", p.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", p.display()));
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!("{} is {}: files can be up to {} MB", p.display(), size_text(meta.len()), MAX_ATTACHMENT_BYTES / (1024 * 1024)));
    }
    Ok(real)
}

impl Gateway {
    /// Keep a file with the session's attachments and put it in the
    /// transcript.
    pub(super) fn deliver(&self, session_id: &str, o: Outgoing) -> Result<AgentFile, String> {
        let up = &self.ctx.uploads;
        let keep = |name: &str, mime: &str, bytes: &[u8]| -> Result<Attachment, String> {
            let a = up.put(name, mime, bytes).map_err(|e| e.message)?;
            up.keep(session_id, &a.attachment_id).map_err(|e| format!("could not keep {name}: {e}"))?;
            Ok(a)
        };
        let attachment = keep(o.name, o.mime, o.bytes)?;
        let poster = match o.poster {
            Some(p) => keep("poster.jpg", "image/jpeg", p).ok(),
            None => None,
        };
        let file = AgentFile {
            attachment,
            kind: o.kind,
            caption: o.caption,
            poster,
            duration_ms: o.duration_ms,
            width: o.width,
            height: o.height,
            tool_call_id: Some(o.tool_call_id.to_string()),
        };
        self.ctx
            .append(session_id, EventType::AgentFile, serde_json::to_value(&file).unwrap())
            .map_err(|e| format!("could not add it to the session: {e}"))?;
        self.ctx.audit.log(
            "agent.file",
            json!({"session_id": session_id, "attachment_id": file.attachment.attachment_id, "name": file.attachment.name, "size": file.attachment.size, "kind": file.kind}),
        );
        Ok(file)
    }

    /// The folders a session may send files from: its own folder, its temp
    /// folder, and a quick task's scratch folder.
    fn send_roots(&self, session_id: &str) -> (Option<PathBuf>, Vec<PathBuf>) {
        let folder = self.session_git_dir(session_id);
        let mut roots: Vec<PathBuf> = folder.iter().cloned().collect();
        roots.push(crate::session::temp_dir_for(&self.ctx.cfg, session_id));
        roots.push(self.ctx.cfg.scratch_dir().join(session_id));
        (folder, roots)
    }

    /// `agenthost__send_file`.
    pub(super) async fn send_file(self: &Arc<Self>, session_id: &str, args: Value) -> Value {
        let call_id = new_id("mcp");
        let tool = "send_file";
        let _ = self.ctx.append(
            session_id,
            EventType::ToolCall,
            json!({
                "tool_call_id": call_id, "name": tool, "exposed_name": SEND_FILE_TOOL, "arguments": args,
                "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID, "connector_name": "AgentOS",
                "tool_kind": ToolKind::Read, "policy": ToolPolicy::Auto
            }),
        );
        let finish = |status: &str, output: &str| {
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": tool, "status": status, "output": output, "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID}),
            );
        };
        let fail = |msg: String| {
            finish("failed", &msg);
            text_result(&format!("AgentOS: {msg}"), true)
        };
        if self.rate_limited(session_id) {
            return fail("too many calls this minute; try again shortly".into());
        }
        let Some(path) = args["path"].as_str().filter(|p| !p.trim().is_empty()) else {
            return fail("give the file's `path`".into());
        };
        let (folder, roots) = self.send_roots(session_id);
        let base = folder.clone().unwrap_or_else(|| crate::session::temp_dir_for(&self.ctx.cfg, session_id));
        let real = match resolve_in(path, &base, &roots) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let bytes = match tokio::fs::read(&real).await {
            Ok(b) if b.len() as u64 <= MAX_ATTACHMENT_BYTES => b,
            Ok(_) => return fail(format!("{path} is larger than {} MB", MAX_ATTACHMENT_BYTES / (1024 * 1024))),
            Err(e) => return fail(format!("{path}: {e}")),
        };
        let own = real.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
        let name = crate::uploads::safe_name(args["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(&own));
        let out = Outgoing {
            name: &name,
            mime: guess_mime(&name),
            bytes: &bytes,
            kind: AgentFileKind::File,
            caption: caption(&args["caption"]),
            poster: None,
            duration_ms: None,
            width: None,
            height: None,
            tool_call_id: &call_id,
        };
        match self.deliver(session_id, out) {
            Ok(f) => {
                let msg = format!("Sent {} ({}) to the user; they see it in this conversation.", f.attachment.name, size_text(f.attachment.size));
                finish("completed", &msg);
                text_result(&msg, false)
            }
            Err(e) => fail(e),
        }
    }

    /// `agenthost__browser_record_stop`: finish the recording and send it.
    pub(super) async fn send_recording(self: &Arc<Self>, session_id: &str, call_id: &str, args: &Value) -> Result<crate::browser::AgentOut> {
        let f = self.browsers.agent_record_stop(session_id).await?;
        let name = recording_name(now_ms() / 1000, &crate::cron::SystemTz);
        let secs = f.duration_ms.div_ceil(1000);
        let out = Outgoing {
            name: &name,
            mime: "video/mp4",
            bytes: &f.mp4,
            kind: AgentFileKind::Recording,
            caption: caption(&args["caption"]),
            poster: f.poster.as_deref(),
            duration_ms: Some(f.duration_ms),
            width: Some(f.width),
            height: Some(f.height),
            tool_call_id: call_id,
        };
        let sent = self.deliver(session_id, out).map_err(|e| anyhow!(e))?;
        let cut = if f.cut {
            format!(" It stopped at {} minutes (or the size limit), so the end is missing.", crate::browser::record::MAX_LENGTH.as_secs() / 60)
        } else {
            String::new()
        };
        Ok(crate::browser::AgentOut {
            text: format!("Sent the recording ({}:{:02}, {}) to the user; they see it in this conversation.{cut}", secs / 60, secs % 60, size_text(sent.attachment.size)),
            image: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_in_the_sessions_folders_go() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        let other = dir.path().join("secret");
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join("out/shot.png"), b"png").unwrap();
        std::fs::write(other.join("id_rsa"), b"key").unwrap();
        std::os::unix::fs::symlink(other.join("id_rsa"), root.join("link")).unwrap();
        let roots = vec![root.clone()];

        assert_eq!(resolve_in("out/shot.png", &root, &roots).unwrap(), std::fs::canonicalize(root.join("out/shot.png")).unwrap());
        assert!(resolve_in(root.join("out/shot.png").to_str().unwrap(), &root, &roots).is_ok());
        for bad in ["../secret/id_rsa", "link", other.join("id_rsa").to_str().unwrap()] {
            assert!(resolve_in(bad, &root, &roots).unwrap_err().contains("outside"), "{bad}");
        }
        assert!(resolve_in("out", &root, &roots).unwrap_err().contains("not a file"));
        assert!(resolve_in("nope.txt", &root, &roots).is_err());
    }

    #[test]
    fn names_and_sizes_read_well() {
        // 2026-11-26 14:05 UTC.
        assert_eq!(recording_name(1_795_701_900, &crate::cron::FixedOffset(0)), "Browser recording 2026-11-26 14.05.mp4");
        assert_eq!(size_text(512), "512 bytes");
        assert_eq!(size_text(1500), "2 KB");
        assert_eq!(size_text(3 * 1024 * 1024 + 100_000), "3.1 MB");
        assert_eq!(caption(&json!("  ")), None);
        assert_eq!(caption(&json!(" The form works. ")).as_deref(), Some("The form works."));
    }
}
