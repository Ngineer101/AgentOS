//! Files agents send you (`agent.file`, protocol 13): a screenshot, a report, a recording of
//! the agent's browser tab. Each shows in the transcript where the agent sent it, with what
//! it said about it: a picture inline, a video as its still with a play button, anything else
//! as a chip with its name and size.
//!
//! Clicking one fetches it with `attachment.read` (the bridge, off the UI thread), saves it
//! in Downloads › AgentOS under its own name and opens it with the system's app; the toolkit
//! plays no video, so recordings open in the system's player. A second click opens the saved
//! copy. Files that could run a program are saved and shown in their folder, never opened.

use crate::icons as ic;
use crate::look::{Theme, MEDIUM};
use crate::model::{App, SentImage};
use crate::view::{clip, human_size};
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, TextStyle};
use rune_kit::Kit;
use std::path::{Path, PathBuf};

/// Where fetching one of the files stands, by attachment id. Kept for the whole run, so a
/// second click opens what was saved.
#[derive(Debug, Clone, PartialEq)]
pub enum Download {
    Fetching { name: String, done: u64, size: u64 },
    Saved(PathBuf),
    Failed(String),
}

/// Something to hand the system's opener on the next poll.
#[derive(Debug, Clone, PartialEq)]
pub enum Opening {
    /// Open the file in its app.
    Open(PathBuf),
    /// Show the file in its folder.
    Reveal(PathBuf),
}

/// `open` on macOS, `xdg-open` elsewhere.
pub fn opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Run the system's opener. Its process is waited for on a thread of its own, so none is
/// left behind.
pub fn system_open(o: &Opening) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(opener());
    match o {
        Opening::Open(p) => cmd.arg(p),
        // Finder selects the file; a Linux file manager can only be asked for the folder.
        Opening::Reveal(p) if cfg!(target_os = "macos") => cmd.arg("-R").arg(p),
        Opening::Reveal(p) => cmd.arg(p.parent().unwrap_or(p)),
    };
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// How a file shows in the transcript.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shown {
    /// Inline, as a picture.
    Picture,
    /// Its still with a play button.
    Video,
    /// A chip.
    File,
}

pub fn shown_as(f: &AgentFile) -> Shown {
    let mime = mime_of(&f.attachment);
    if f.kind == AgentFileKind::Recording || mime.starts_with("video/") {
        Shown::Video
    } else if crate::model::mime_decodes(mime) {
        Shown::Picture
    } else {
        Shown::File
    }
}

/// The attachment's type, or one guessed from its name when the host had none.
fn mime_of(a: &Attachment) -> &str {
    if a.mime.is_empty() || a.mime == "application/octet-stream" {
        guess_mime(&a.name)
    } else {
        &a.mime
    }
}

/// The agent files in an event, if it is one.
pub fn parse(e: &Event) -> Option<AgentFile> {
    (e.kind == EventType::AgentFile).then(|| serde_json::from_value(e.payload.clone()).ok()).flatten()
}

/// The pictures the transcript shows for these events: the images agents sent and the
/// stills of their videos, as attachment ids.
pub fn pictures(events: &[Event]) -> Vec<String> {
    let mut out = vec![];
    for f in events.iter().filter_map(parse) {
        match shown_as(&f) {
            Shown::Picture => out.push(f.attachment.attachment_id),
            Shown::Video => out.extend(f.poster.filter(|p| crate::model::mime_decodes(mime_of(p))).map(|p| p.attachment_id)),
            Shown::File => {}
        }
    }
    out
}

/// A video's length as a player shows it: "0:42", "12:05", "1:02:09".
pub fn duration(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The user's Downloads folder. On Linux `$XDG_DOWNLOAD_DIR`, else the one `user-dirs.dirs`
/// names (its text in `user_dirs`), else `~/Downloads`; on macOS always `~/Downloads`.
pub fn downloads_dir(mac: bool, xdg: Option<&str>, user_dirs: Option<&str>, home: &Path) -> PathBuf {
    let from_xdg = |v: &str| {
        let v = v.trim().trim_matches('"');
        let p = match v.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(v),
        };
        // A relative path or the home folder itself means the setting is off.
        (p.is_absolute() && p != home).then_some(p)
    };
    if !mac {
        if let Some(p) = xdg.and_then(from_xdg) {
            return p;
        }
        let line = user_dirs.and_then(|t| t.lines().find_map(|l| l.trim().strip_prefix("XDG_DOWNLOAD_DIR=")));
        if let Some(p) = line.and_then(from_xdg) {
            return p;
        }
    }
    home.join("Downloads")
}

/// Where this app saves files agents sent: an AgentOS folder in Downloads.
pub fn save_folder() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let user_dirs = if cfg!(target_os = "macos") { None } else { std::fs::read_to_string(config.join("user-dirs.dirs")).ok() };
    let xdg = std::env::var("XDG_DOWNLOAD_DIR").ok();
    downloads_dir(cfg!(target_os = "macos"), xdg.as_deref(), user_dirs.as_deref(), &home).join("AgentOS")
}

/// The name the agent gave, made safe to save under: no folders, no hidden or empty names.
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last.chars().map(|c| if c.is_control() || c == ':' { '_' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.').trim();
    let clean = if clean.is_empty() { "file" } else { clean };
    // Most file systems take 255 bytes; keep the extension when cutting.
    if clean.len() <= 200 {
        return clean.to_string();
    }
    let (stem, ext) = split_ext(clean);
    let ext = if ext.len() <= 40 { ext } else { "" };
    let mut end = (200 - ext.len()).min(stem.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", &stem[..end])
}

/// "report.pdf" → ("report", ".pdf"); a leading dot is not an extension.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The `n`th name to try for `name`: itself, then "report (1).pdf", "report (2).pdf" …
pub fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    format!("{stem} ({n}){ext}")
}

/// Write `data` into `dir` under `name` (made safe), numbered if that name is taken.
/// Never replaces a file.
pub fn save_unique(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let name = safe_name(name);
    for n in 0..1000 {
        let path = dir.join(numbered(&name, n));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data) {
                    drop(f);
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("too many files called {name}")))
}

/// Whether the system may open this file straight away: pictures, video, sound and
/// documents. Anything else (a script, an app, a shortcut) could run a program, so it is
/// only shown in its folder.
pub fn opens_here(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "tif" | "tiff" | "svg"
            | "mp4" | "m4v" | "mov" | "webm" | "mkv"
            | "mp3" | "m4a" | "wav" | "ogg" | "flac"
            | "pdf" | "txt" | "log" | "md" | "markdown" | "csv" | "tsv" | "json" | "diff" | "patch"
    )
}

/// All of an attachment's bytes, a piece at a time: `read(offset)` is one `attachment.read`
/// call. `progress(done, size)` follows each piece.
pub async fn read_all<F, Fut>(mut read: F, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String>
where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<AttachmentReadResult, String>>,
{
    use base64::Engine;
    let mut data: Vec<u8> = vec![];
    loop {
        let r = read(data.len() as u64).await?;
        if r.size > MAX_ATTACHMENT_BYTES {
            return Err(format!("it is larger than {} MB", MAX_ATTACHMENT_BYTES / (1024 * 1024)));
        }
        let piece = base64::engine::general_purpose::STANDARD.decode(r.data.as_bytes()).map_err(|_| "the host sent bad data".to_string())?;
        if piece.is_empty() && data.len() as u64 == r.size {
            return Ok(data); // an empty file
        }
        if piece.is_empty() || data.len() as u64 + piece.len() as u64 > r.size {
            return Err(format!("the host stopped after {} of {}", human_size(data.len() as u64), human_size(r.size)));
        }
        data.extend(piece);
        progress(data.len() as u64, r.size);
        if data.len() as u64 >= r.size {
            return Ok(data);
        }
    }
}

/// `attachment.read`'s parameters for one piece.
pub fn read_params(session_id: &str, attachment_id: &str, offset: u64) -> serde_json::Value {
    serde_json::to_value(AttachmentReadParams { session_id: session_id.into(), attachment_id: attachment_id.into(), offset }).unwrap_or_default()
}

/// A saved file's folder for people: "~/Downloads/AgentOS".
fn folder_label(path: &Path) -> String {
    let dir = path.parent().unwrap_or(path);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    }
}

impl App {
    /// A file an agent sent was clicked: open the copy saved earlier, else fetch it.
    pub fn open_agent_file(&mut self, session_id: &str, a: &Attachment) {
        match self.downloads.get(&a.attachment_id) {
            Some(Download::Fetching { .. }) => return,
            Some(Download::Saved(p)) if p.is_file() => {
                let p = p.clone();
                self.open_saved(p);
                return;
            }
            _ => {}
        }
        self.downloads.insert(a.attachment_id.clone(), Download::Fetching { name: a.name.clone(), done: 0, size: a.size });
        if let Some(b) = &self.bridge {
            b.send(crate::bridge::ToHost::Download { session_id: session_id.to_string(), attachment: a.clone(), dir: save_folder() });
        }
    }

    /// Open a saved file, or only show it in its folder if it could run a program.
    fn open_saved(&mut self, path: PathBuf) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if opens_here(&name) {
            self.open_files.push(Opening::Open(path));
        } else {
            self.notice = Some(format!("{name} could run a program, so it was saved but not opened. Open it yourself if you trust it."));
            self.open_files.push(Opening::Reveal(path));
        }
    }

    /// Show a saved file in its folder.
    pub fn reveal_agent_file(&mut self, attachment_id: &str) {
        if let Some(Download::Saved(p)) = self.downloads.get(attachment_id) {
            self.open_files.push(Opening::Reveal(p.clone()));
        }
    }

    pub fn download_progress(&mut self, attachment_id: &str, now: u64, of: u64) {
        if let Some(Download::Fetching { done, size, .. }) = self.downloads.get_mut(attachment_id) {
            (*done, *size) = (now, of);
        }
    }

    pub fn downloaded(&mut self, attachment_id: String, result: Result<PathBuf, String>) {
        // Not asked for on this host (it was switched away from): drop it.
        let Some(Download::Fetching { name, .. }) = self.downloads.get(&attachment_id) else { return };
        let name = name.clone();
        match result {
            Ok(path) => {
                self.downloads.insert(attachment_id, Download::Saved(path.clone()));
                self.open_saved(path);
            }
            Err(e) => {
                self.error = Some(format!("Couldn't download {name}: {e}"));
                self.downloads.insert(attachment_id, Download::Failed(e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The transcript's item
// ---------------------------------------------------------------------------

/// Largest picture, and largest video still, in points.
const PICTURE_W: f32 = 440.0;
const PICTURE_H: f32 = 330.0;
const VIDEO_H: f32 = 280.0;

/// `w × h` scaled into `max_w × max_h`, never larger than it is.
fn fit(w: f32, h: f32, max_w: f32, max_h: f32) -> (f32, f32) {
    let scale = (max_w / w.max(1.0)).min(max_h / h.max(1.0)).min(1.0);
    ((w * scale).round().max(24.0), (h * scale).round().max(24.0))
}

/// A decoded picture and its size, if it has one.
fn ready(s: &App, id: &str) -> Option<(rune::Image, [u32; 2])> {
    match s.images.get(id)? {
        SentImage::Ready(img) => img.size().map(|sz| (img.clone(), sz)),
        _ => None,
    }
}

/// Whether a picture won't show: it failed to load or decode.
fn broken(s: &App, id: &str) -> bool {
    match s.images.get(id) {
        Some(SentImage::Failed(_)) => true,
        Some(SentImage::Ready(img)) => img.error().is_some(),
        _ => false,
    }
}

/// What the agent sent, in its reply: the caption, the file, and how saving it went.
pub fn item(s: &App, session_id: &str, f: &AgentFile, t: &Theme, k: &Kit) -> El {
    let a = &f.attachment;
    let id = a.attachment_id.clone();
    let mut c = col().key(format!("agent-file-{id}")).w_full().gap(8.0).items_start();
    if let Some(caption) = f.caption.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
        c = c.child(crate::session::reply(caption, false, t, k));
    }
    let sid = session_id.to_string();
    let a2 = a.clone();
    let open = move |s: &mut App| s.open_agent_file(&sid, &a2);
    let body = match shown_as(f) {
        Shown::Picture if !broken(s, &id) => picture(s, a, t),
        Shown::Video => video(s, f, t),
        _ => chip(a, t),
    };
    c = c.child(
        body.key(format!("agent-file-open-{id}"))
            .role("button")
            .focusable(true)
            .cursor(rune::Cursor::Pointer)
            .on_press(open),
    );
    if let Some(line) = status(s, &id, t) {
        c = c.child(line.key(format!("agent-file-status-{id}")));
    }
    c
}

/// An image, as large as the column allows; a box saying it loads until it has.
fn picture(s: &App, a: &Attachment, t: &Theme) -> El {
    let base = el().radius(12.0).clip_rounded(12.0).border(0.5, Color::WHITE.alpha(0.12)).label(format!("Open {}", a.name)).help(a.name.clone());
    match ready(s, &a.attachment_id) {
        Some((img, [w, h])) => {
            let (w, h) = fit(w as f32, h as f32, PICTURE_W, PICTURE_H);
            base.size(w, h).pixels_fit(img.pixels(), rune::SurfaceFit::Fill)
        }
        None => base.size(240.0, 160.0).fill(Color::WHITE.alpha(0.05)).center().child(
            col()
                .items_center()
                .gap(4.0)
                .child(icon(&ic::FILE_SM, 16.0).color(t.text2))
                .child(text(clip(&a.name, 40)).font(TextStyle::new(12.0, 400.0)).color(t.text2))
                .child(text("Loading…").font(TextStyle::new(11.0, 400.0)).color(t.text3)),
        ),
    }
}

/// A video: its still (or a dark box) with a play button, then its name and length.
fn video(s: &App, f: &AgentFile, t: &Theme) -> El {
    let a = &f.attachment;
    let poster = f.poster.as_ref().and_then(|p| ready(s, &p.attachment_id));
    let dims = match (f.width, f.height, &poster) {
        (Some(w), Some(h), _) if w > 0 && h > 0 => (w as f32, h as f32),
        (_, _, Some((_, [w, h]))) => (*w as f32, *h as f32),
        _ => (16.0, 9.0),
    };
    // The video's shape at the largest size allowed, even when its still is smaller.
    let scale = (PICTURE_W / dims.0).min(VIDEO_H / dims.1);
    let (w, h) = ((dims.0 * scale).round().max(96.0), (dims.1 * scale).round().max(96.0));
    let mut frame = el().size(w, h).radius(12.0).clip_rounded(12.0).border(0.5, Color::WHITE.alpha(0.12)).fill(Color::BLACK.alpha(0.35)).center();
    if let Some((img, _)) = &poster {
        frame = frame.pixels_fit(img.pixels(), rune::SurfaceFit::Fill);
    }
    let play = row()
        .size(48.0, 48.0)
        .radius(24.0)
        .center()
        .fill(Color::BLACK.alpha(0.55))
        .border(0.5, Color::WHITE.alpha(0.25))
        .child(icon(&ic::PLAY, 18.0).color(Color::WHITE).pl(3.0));
    let length = f.duration_ms.map(duration);
    let what = if f.kind == AgentFileKind::Recording { "Recording" } else { "Video" };
    let label = match &length {
        Some(l) => format!("Play {} ({what}, {l})", a.name),
        None => format!("Play {} ({what})", a.name),
    };
    let mut line = row().gap(6.0).center_y().child(text(clip(&a.name, 60)).font(TextStyle::new(t.fs_small, MEDIUM)).color(t.text2));
    if let Some(l) = length {
        line = line.child(text(l).font(TextStyle::new(t.fs_small, 400.0)).color(t.text3));
    }
    col().gap(6.0).label(label).help("Opens in the system's video player").child(frame.child(play)).child(line)
}

/// Any other file: an icon, its name and size.
fn chip(a: &Attachment, t: &Theme) -> El {
    let mut r = row()
        .gap(10.0)
        .center_y()
        .max_w(420.0)
        .px(12.0)
        .py(9.0)
        .radius(t.card_radius)
        .fill(t.card)
        .hover(Color::WHITE.alpha(0.04))
        .label(format!("Open {}, {}", a.name, human_size(a.size)))
        .help(a.name.clone())
        .child(icon(&ic::DOC, 18.0).color(t.text2))
        .child(
            col()
                .min_w(0.0)
                .shrink()
                .gap(1.0)
                .child(text(clip(&a.name, 60)).font(TextStyle::new(t.fs, MEDIUM)).color(t.text))
                .child(text(human_size(a.size)).font(TextStyle::new(t.fs_small, 400.0)).color(t.text2)),
        );
    if t.mac() {
        r = r.border(0.5, t.card_border);
    }
    r
}

/// How fetching it went, under the file: progress, the failure, or where it was saved.
fn status(s: &App, id: &str, t: &Theme) -> Option<El> {
    let line = row().gap(6.0).center_y();
    Some(match s.downloads.get(id)? {
        Download::Fetching { done, size, .. } => {
            let words = if *done == 0 { "Downloading…".to_string() } else { format!("Downloading… {} of {}", human_size(*done), human_size(*size)) };
            line.role("status")
                .label(words.clone())
                .child(icon(&ic::SPINNER, 12.0).color(t.text2).looping(rune::LoopSpec::rotation(0.9)))
                .child(t.small(words))
        }
        Download::Failed(e) => line
            .child(icon(&ic::ERROR, 12.0).color(t.danger))
            .child(text(format!("Couldn't download it: {}. Click it to try again.", clip(e, 200))).font(t.font_small()).color(t.text2).wrap().shrink()),
        Download::Saved(path) => {
            let (id, id2) = (id.to_string(), id.to_string());
            line.child(t.small(format!("Saved in {}", folder_label(path))))
                .child(t.small_button(if t.mac() { "Show in Finder" } else { "Open Folder" }, move |s| s.reveal_agent_file(&id)).key(format!("agent-file-reveal-{id2}")))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_player() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(42_400), "0:42");
        assert_eq!(duration(59_600), "1:00");
        assert_eq!(duration(12 * 60_000 + 5_000), "12:05");
        assert_eq!(duration(3_729_000), "1:02:09");
    }

    #[test]
    fn downloads_folder_follows_the_platform() {
        let home = Path::new("/home/nico");
        assert_eq!(downloads_dir(false, None, None, home), PathBuf::from("/home/nico/Downloads"));
        assert_eq!(downloads_dir(false, Some("/data/dl"), None, home), PathBuf::from("/data/dl"));
        let dirs = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\n";
        assert_eq!(downloads_dir(false, None, Some(dirs), home), PathBuf::from("/home/nico/Téléchargements"));
        assert_eq!(downloads_dir(false, Some("/data/dl"), Some(dirs), home), PathBuf::from("/data/dl"), "the variable wins");
        // "$HOME/" alone means no Downloads folder is set; a relative one is ignored.
        assert_eq!(downloads_dir(false, None, Some("XDG_DOWNLOAD_DIR=\"$HOME/\""), home), PathBuf::from("/home/nico/Downloads"));
        assert_eq!(downloads_dir(false, Some("dl"), None, home), PathBuf::from("/home/nico/Downloads"));
        // macOS has no XDG folders.
        assert_eq!(downloads_dir(true, Some("/data/dl"), Some(dirs), Path::new("/Users/nico")), PathBuf::from("/Users/nico/Downloads"));
    }

    #[test]
    fn saved_names_are_safe_and_unique() {
        assert_eq!(safe_name("report.pdf"), "report.pdf");
        assert_eq!(safe_name("../../.ssh/authorized_keys"), "authorized_keys");
        assert_eq!(safe_name("C:\\temp\\x.txt"), "x.txt");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("a\nb.txt"), "a_b.txt");
        let long = format!("{}.mp4", "é".repeat(200));
        assert!(safe_name(&long).len() <= 200 && safe_name(&long).ends_with(".mp4"));
        assert_eq!(numbered("report.pdf", 0), "report.pdf");
        assert_eq!(numbered("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered("Makefile", 1), "Makefile (1)");
        assert_eq!(numbered("Sign-up flow.mp4", 1), "Sign-up flow (1).mp4");

        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new().prefix("agentfile").tempdir_in(&base).unwrap();
        let into = dir.path().join("AgentOS");
        let first = save_unique(&into, "clip.mp4", b"one").unwrap();
        let second = save_unique(&into, "clip.mp4", b"two").unwrap();
        assert_eq!(first, into.join("clip.mp4"));
        assert_eq!(second, into.join("clip (1).mp4"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one", "never replaced");
        assert_eq!(save_unique(&into, "../escape.txt", b"x").unwrap(), into.join("escape.txt"));
    }

    #[test]
    fn only_documents_and_media_open_by_themselves() {
        for ok in ["a.png", "Sign-up flow.mp4", "report.PDF", "runs.csv", "notes.md"] {
            assert!(opens_here(ok), "{ok}");
        }
        for no in ["install.sh", "Run.command", "x.app", "a.desktop", "setup.exe", "page.html", "Makefile", "x.jar"] {
            assert!(!opens_here(no), "{no}");
        }
    }

    #[test]
    fn files_are_read_a_piece_at_a_time() {
        use base64::Engine;
        let file: Vec<u8> = (0..10u8).collect();
        let piece = |offset: u64, n: usize| {
            let end = (offset as usize + n).min(file.len());
            let data = base64::engine::general_purpose::STANDARD.encode(&file[offset as usize..end]);
            AttachmentReadResult { name: "x.bin".into(), mime: "application/octet-stream".into(), size: file.len() as u64, offset, data }
        };
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let mut asked = vec![];
        let mut seen = vec![];
        let got = rt.block_on(read_all(
            |o| {
                asked.push(o);
                let r = piece(o, 4);
                async move { Ok(r) }
            },
            |d, s| seen.push((d, s)),
        ));
        assert_eq!(got.unwrap(), file);
        assert_eq!(asked, [0, 4, 8]);
        assert_eq!(seen, [(4, 10), (8, 10), (10, 10)]);
        // A host that stops sending, and an empty file.
        let stopped = rt.block_on(read_all(|o| { let r = piece(o, if o == 0 { 4 } else { 0 }); async move { Ok(r) } }, |_, _| {}));
        assert_eq!(stopped.unwrap_err(), "the host stopped after 4 B of 10 B");
        let empty = rt.block_on(read_all(|o| async move { Ok(AttachmentReadResult { name: "e".into(), mime: String::new(), size: 0, offset: o, data: String::new() }) }, |_, _| {}));
        assert_eq!(empty.unwrap(), Vec::<u8>::new());
        assert_eq!(read_params("ses_1", "att_rec", 524_288), serde_json::json!({"session_id": "ses_1", "attachment_id": "att_rec", "offset": 524_288}));
    }
}
