//! The right panel's Changes and Files tabs: what the open session changed, and its folder
//! to read (protocol 11). Changes is rune-kit's diff view over the host's patch; Files is
//! its file tree, and a file opens in place of the tree, coloured by language.
//!
//! Both belong to the open session. They load when shown, again when the agent's turn
//! ends, and on Refresh.

use crate::bridge::Tag;
use crate::icons as ic;
use crate::look::Theme;
use crate::model::{App, Conn, PanelTab, Screen};
use agenthost_proto::methods as m;
use agenthost_proto::{Session, SessionChanges, SessionFile, SessionFiles};
use rune::{col, el, rich, row, text, Align, Color, El, GrowthSpec, ScrollSpec, TextEdit, TextStyle, Tx};
use rune_kit::diff::{self, DiffMode, DiffState, DiffView, FileStatus};
use rune_kit::syntax::{self, Highlight, SyntaxTheme};
use rune_kit::{FileNode, FileTree, Kit, TextInput, Tree, TreeItem, TreeModel};
use serde_json::json;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

/// Where one of the panel's loads stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Load {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// What a file's row in the tree says about it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Change {
    pub added: u32,
    pub removed: u32,
    pub new: bool,
}

/// A file open in the Files tab.
pub struct OpenFile {
    pub path: String,
    pub load: Load,
    /// The file's text as drawn: tabs are spaces to the next four-column stop.
    pub text: Rc<str>,
    /// Each line's bytes in `text`, without its line end.
    pub lines: Rc<[(u32, u32)]>,
    pub highlights: Rc<[Highlight]>,
    pub lang: Option<&'static str>,
    pub size: u64,
    /// `text` is only the file's start.
    pub truncated: bool,
    /// Not text: nothing to draw.
    pub binary: bool,
    /// The longest line, in characters.
    pub columns: usize,
}

pub struct FilesState {
    /// The session all of this belongs to.
    pub session: Option<String>,
    pub diff: DiffState,
    pub changes: Load,
    pub branch: String,
    /// The patch was too long: some files are missing from it.
    pub changes_truncated: bool,
    /// Lines added and removed per path, from the diff, for the tree's rows.
    pub counts: HashMap<String, Change>,
    pub tree: TreeModel<FileNode<Change>>,
    pub paths: Vec<String>,
    pub listing: Load,
    pub listing_truncated: bool,
    pub filter: TextEdit,
    pub open: Option<OpenFile>,
    /// The agent finished a turn since these loaded.
    pub stale: bool,
}

impl Default for FilesState {
    fn default() -> Self {
        FilesState {
            session: None,
            diff: DiffState::default(),
            changes: Load::Idle,
            branch: String::new(),
            changes_truncated: false,
            counts: HashMap::new(),
            tree: TreeModel::default(),
            paths: Vec::new(),
            listing: Load::Idle,
            listing_truncated: false,
            filter: TextEdit::new(""),
            open: None,
            stale: false,
        }
    }
}

/// `text` with each tab as spaces to the next four-column stop.
pub fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut col = 0;
    for c in text.chars() {
        match c {
            '\t' => {
                let n = 4 - col % 4;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' => {
                out.push(c);
                col = 0;
            }
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// Each line's byte range in `text`, without `\n` or `\r\n`, and the longest in characters.
fn line_ranges(text: &str) -> (Vec<(u32, u32)>, usize) {
    let mut lines = Vec::new();
    let mut columns = 0;
    let mut at = 0;
    for l in text.split_inclusive('\n') {
        let body = l.strip_suffix('\n').unwrap_or(l);
        let body = body.strip_suffix('\r').unwrap_or(body);
        lines.push((at as u32, (at + body.len()) as u32));
        columns = columns.max(body.chars().count());
        at += l.len();
    }
    (lines, columns)
}

impl OpenFile {
    fn loading(path: &str) -> OpenFile {
        OpenFile { path: path.to_string(), load: Load::Loading, text: "".into(), lines: Rc::new([]), highlights: Rc::new([]), lang: syntax::lang_for_path(path), size: 0, truncated: false, binary: false, columns: 0 }
    }

    fn set(&mut self, f: SessionFile) {
        self.load = Load::Ready;
        self.size = f.size;
        self.truncated = f.truncated;
        self.binary = f.text.is_none();
        let text = expand_tabs(f.text.as_deref().unwrap_or(""));
        let (lines, columns) = line_ranges(&text);
        self.highlights = syntax::highlight(self.lang, &text);
        self.text = text.into();
        self.lines = lines.into();
        self.columns = columns;
    }

    /// The colours of line `i`, as spans of that line.
    fn spans(&self, i: usize, theme: &SyntaxTheme) -> Vec<rune::Span> {
        let (a, b) = self.lines[i];
        let (a, b) = (a as usize, b as usize);
        let first = self.highlights.partition_point(|(r, _)| r.end <= a);
        let mut out = Vec::new();
        for (r, p) in &self.highlights[first..] {
            if r.start >= b {
                break;
            }
            let (s, e) = (r.start.max(a) - a, r.end.min(b) - a);
            if s < e {
                out.extend(theme.spans(&[(s..e, *p)], 0));
            }
        }
        out
    }
}

/// "48 KB", for a file that can't be drawn.
fn size_label(n: u64) -> String {
    match n {
        0..1024 => format!("{n} bytes"),
        1024..1_048_576 => format!("{} KB", n.div_ceil(1024)),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

impl App {
    /// The session the Changes and Files tabs are about: the one on screen.
    fn files_session(&self) -> Option<&Session> {
        match &self.screen {
            Screen::Session(id) => self.sessions.iter().find(|x| &x.session_id == id).or_else(|| self.archived.iter().find(|x| &x.session_id == id)),
            _ => None,
        }
    }

    fn files_host_ok(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.protocol_version >= 11)
    }

    /// The Files tab shows: a session is open, on a host that can list its folder.
    pub fn files_available(&self) -> bool {
        self.files_host_ok() && self.files_session().is_some()
    }

    /// The Changes tab shows: the open session works on a branch of its own.
    pub fn changes_available(&self) -> bool {
        self.files_host_ok() && self.files_session().is_some_and(|x| x.worktree.is_some())
    }

    /// How many files the open session changed, once that is known.
    pub fn changed_files(&self) -> Option<usize> {
        let id = self.files_session()?.session_id.as_str();
        (self.browse.session.as_deref() == Some(id) && self.browse.changes == Load::Ready).then(|| self.browse.diff.files().len())
    }

    /// Bring the tabs' state in step with the session on screen, and load what the shown
    /// tab needs. Called when a tab is shown and when the screen changes.
    pub fn files_sync(&mut self) {
        let Some(tab) = self.panel_shown().filter(|t| matches!(t, PanelTab::Changes | PanelTab::Files)) else { return };
        let Some(id) = self.files_session().map(|x| x.session_id.clone()) else { return };
        if self.browse.session.as_deref() != Some(&id) {
            self.browse = FilesState { session: Some(id), ..Default::default() };
        }
        if self.conn != Conn::Connected {
            return;
        }
        let stale = std::mem::take(&mut self.browse.stale);
        if self.changes_available() && (stale || self.browse.changes == Load::Idle) {
            self.load_changes();
        }
        if tab == PanelTab::Files {
            if stale || self.browse.listing == Load::Idle {
                self.load_files();
            }
            if stale {
                self.reload_open_file();
            }
        }
    }

    /// Refresh: ask again for everything the tabs show.
    pub fn files_refresh(&mut self) {
        self.browse.stale = true;
        self.files_sync();
    }

    /// The agent's turn in `session` ended: what the tabs show may be old.
    pub fn files_turn_ended(&mut self, session: &str) {
        if self.browse.session.as_deref() == Some(session) {
            self.browse.stale = true;
            self.files_sync();
        }
    }

    fn load_changes(&mut self) {
        let Some(id) = self.browse.session.clone() else { return };
        if self.browse.changes != Load::Ready {
            self.browse.changes = Load::Loading;
        }
        self.call(Tag::Changes(id.clone()), m::SESSION_CHANGES, json!({"session_id": id}));
    }

    fn load_files(&mut self) {
        let Some(id) = self.browse.session.clone() else { return };
        if self.browse.listing != Load::Ready {
            self.browse.listing = Load::Loading;
        }
        self.call(Tag::Files(id.clone()), m::SESSION_FILES, json!({"session_id": id}));
    }

    fn reload_open_file(&mut self) {
        let (Some(id), Some(path)) = (self.browse.session.clone(), self.browse.open.as_ref().map(|f| f.path.clone())) else { return };
        self.call(Tag::FileRead { session: id.clone(), path: path.clone() }, m::SESSION_FILE_READ, json!({"session_id": id, "path": path}));
    }

    pub fn changes_loaded(&mut self, session: &str, r: Result<SessionChanges, String>) {
        if self.browse.session.as_deref() != Some(session) {
            return;
        }
        match r {
            Ok(c) => {
                let files = diff::parse(&c.patch);
                self.browse.counts = files
                    .iter()
                    .map(|f| (f.path().to_string(), Change { added: f.additions() as u32, removed: f.deletions() as u32, new: f.status == FileStatus::Added }))
                    .collect();
                self.browse.diff.set_files(files);
                self.browse.branch = c.branch;
                self.browse.changes_truncated = c.truncated;
                self.browse.changes = Load::Ready;
                if self.browse.listing == Load::Ready {
                    self.rebuild_tree();
                }
            }
            Err(e) => self.browse.changes = Load::Failed(e),
        }
    }

    pub fn files_listed(&mut self, session: &str, r: Result<SessionFiles, String>) {
        if self.browse.session.as_deref() != Some(session) {
            return;
        }
        match r {
            Ok(f) => {
                self.browse.paths = f.files;
                self.browse.listing_truncated = f.truncated;
                self.browse.listing = Load::Ready;
                self.rebuild_tree();
            }
            Err(e) => self.browse.listing = Load::Failed(e),
        }
    }

    /// The tree from the listed paths, keeping which folders were open and the row chosen.
    fn rebuild_tree(&mut self) {
        let f = &mut self.browse;
        let mut open = Vec::new();
        let mut stack: Vec<String> = f.tree.children(None).map(str::to_string).collect();
        while let Some(id) = stack.pop() {
            if f.tree.has_children(&id) && f.tree.is_expanded(&id) {
                stack.extend(f.tree.children(Some(&id)).map(str::to_string));
                open.push(id);
            }
        }
        let cursor = f.tree.cursor().map(str::to_string);
        let counts = &f.counts;
        f.tree = FileTree::new().key("files-tree").build(f.paths.iter().map(|p| (p.as_str(), counts.get(p).copied().unwrap_or_default())));
        for id in open {
            if f.tree.contains(&id) {
                f.tree.set_expanded(&id, true);
            }
        }
        if let Some(id) = cursor.filter(|id| f.tree.contains(id)) {
            f.tree.select(&id);
        }
        self.apply_file_filter();
    }

    pub fn apply_file_filter(&mut self) {
        let q = self.browse.filter.text.trim().to_lowercase();
        if q.is_empty() {
            self.browse.tree.clear_filter();
        } else {
            self.browse.tree.filter(|n| n.path.to_lowercase().contains(&q));
        }
    }

    /// Open `path` of the session's folder in the Files tab.
    pub fn open_file(&mut self, path: &str) {
        let Some(id) = self.browse.session.clone() else { return };
        if self.browse.tree.get(path).is_some_and(|n| n.dir) {
            return;
        }
        self.browse.open = Some(OpenFile::loading(path));
        self.show_panel(PanelTab::Files);
        self.call(Tag::FileRead { session: id.clone(), path: path.to_string() }, m::SESSION_FILE_READ, json!({"session_id": id, "path": path}));
    }

    pub fn close_file(&mut self) {
        self.browse.open = None;
    }

    pub fn file_read(&mut self, session: &str, path: &str, r: Result<SessionFile, String>) {
        if self.browse.session.as_deref() != Some(session) {
            return;
        }
        let Some(f) = self.browse.open.as_mut().filter(|f| f.path == path) else { return };
        match r {
            Ok(file) => f.set(file),
            // A reload that failed (the file was deleted) leaves what was read.
            Err(e) if f.load != Load::Ready => f.load = Load::Failed(e),
            Err(_) => {}
        }
    }

    /// From an open file to its part of the diff.
    pub fn show_file_changes(&mut self, path: &str, tx: &mut Tx) {
        self.show_panel(PanelTab::Changes);
        if let Some(i) = self.browse.diff.file_index(path) {
            self.browse.diff.set_collapsed(i, false);
            self.browse.diff.scroll_to_file(i, tx);
        }
    }

    /// Show or hide one of the two tabs. Returns whether it shows now.
    pub fn toggle_files_tab(&mut self, tab: PanelTab) -> bool {
        if self.panel_shown() == Some(tab) {
            self.panel_open = false;
            return false;
        }
        self.show_panel(tab);
        self.panel_shown() == Some(tab)
    }
}

/// The toolbar's button for the open session's changes.
pub fn toggle_button(s: &App, t: &Theme) -> El {
    use crate::commands::{label as keys, run, Cmd};
    let shown = s.panel_shown() == Some(PanelTab::Changes);
    let label = format!("{} Changes ({})", if shown { "Hide" } else { "Show" }, keys(s.look, Cmd::ToggleChanges));
    let b = t.icon_button(&ic::DIFF, if t.mac() { 17.0 } else { 16.0 }, &label, |s, tx| run(s, Cmd::ToggleChanges, tx)).key("toggle-changes").role("button").checked(shown);
    if shown { b.fill(Color::WHITE.alpha(if t.mac() { 0.10 } else { 0.12 })) } else { b }
}

// ---- views ------------------------------------------------------------------------------

fn note(t: &Theme, key: &str, msg: impl Into<String>) -> El {
    col().key(key.to_string()).w_full().grow(1.0).center().px(24.0).child(text(msg.into()).font(t.font_small()).color(t.text2).wrap().align(Align::Center))
}

/// The strip under the panel's header.
fn strip(t: &Theme) -> El {
    let r = row().w_full().h(if t.mac() { 40.0 } else { 46.0 }).px(if t.mac() { 12.0 } else { 10.0 }).gap(8.0).center_y();
    if t.mac() { r.border_sides([0.0, 0.0, 0.5, 0.0], t.sep) } else { r }
}

fn refresh_button(t: &Theme) -> El {
    t.icon_button(&ic::REFRESH, if t.mac() { 13.0 } else { 15.0 }, "Refresh", |s: &mut App, _: &mut Tx| s.files_refresh()).key("files-refresh")
}

fn segment(t: &Theme, label: &'static str, key: &'static str, on: bool, mode: DiffMode) -> El {
    let r = row()
        .key(key)
        .h(if t.mac() { 22.0 } else { 28.0 })
        .px(9.0)
        .center_y()
        .radius(t.btn_radius - 1.0)
        .role("radio")
        .label(label)
        .checked(on)
        .focusable(true)
        .on_press(move |s: &mut App| s.browse.diff.set_mode(mode))
        .child(text(label).font(t.font_small()).color(if on { t.text } else { t.text2 }));
    if on { r.fill(Color::WHITE.alpha(0.12)) } else { r.hover(Color::WHITE.alpha(0.05)) }
}

/// The Changes tab.
pub fn changes(s: &App, t: &Theme, k: &Kit) -> El {
    let body = col().key("changes-body").w_full().grow(1.0).min_h(0.0);
    if s.conn != Conn::Connected {
        return body.child(note(t, "changes-note", "Not connected to the host."));
    }
    let f = &s.browse;
    let files = f.diff.files();
    let summary = match &f.changes {
        Load::Ready => {
            let (a, d) = files.iter().fold((0, 0), |(a, d), x| (a + x.additions(), d + x.deletions()));
            let n = match files.len() {
                1 => "1 file".to_string(),
                n => format!("{n} files"),
            };
            row()
                .key("changes-summary")
                .gap(8.0)
                .center_y()
                .min_w(0.0)
                .shrink()
                .child(text(n).font(t.font_small()).color(t.text))
                .child(text(format!("+{a}")).font(t.font_mono()).color(t.ok))
                .child(text(format!("\u{2212}{d}")).font(t.font_mono()).color(t.danger))
                .child(text(format!("on {}", f.branch)).font(t.font_small()).color(t.text2).truncate().shrink().min_w(0.0))
        }
        _ => row().key("changes-summary").child(text("Changes").font(t.font_small()).color(t.text2)),
    };
    let split = f.diff.mode() == DiffMode::Split;
    let modes = row()
        .key("changes-mode")
        .gap(2.0)
        .padding(2.0)
        .radius(t.btn_radius)
        .fill(Color::WHITE.alpha(0.06))
        .role("radiogroup")
        .label("Layout")
        .child(segment(t, "Unified", "changes-unified", !split, DiffMode::Unified))
        .child(segment(t, "Split", "changes-split", split, DiffMode::Split));
    let bar = strip(t).key("changes-bar").child(summary).child(el().grow(1.0)).child(modes).child(refresh_button(t));

    let content = match &f.changes {
        Load::Idle | Load::Loading => note(t, "changes-note", "Loading changes…"),
        Load::Failed(e) => note(t, "changes-note", format!("Could not load the changes: {e}")),
        Load::Ready if files.is_empty() => note(t, "changes-note", format!("No changes yet on {}.", f.branch)),
        Load::Ready => {
            let t2 = t.clone();
            DiffView::new(|s: &App| &s.browse.diff, |s: &mut App| &mut s.browse.diff)
                .key("changes-diff")
                .header_actions(move |s: &App, i| {
                    let Some(file) = s.browse.diff.files().get(i) else { return el() };
                    if file.status == FileStatus::Deleted {
                        return el();
                    }
                    let path = file.path().to_string();
                    // The view names this element itself, so the button keeps its key one level in.
                    row().child(t2.icon_button(&ic::DOC, 12.0, "Open File", move |s: &mut App, _: &mut Tx| s.open_file(&path)).key(format!("changes-open-{i}")))
                })
                .build(s, k)
        }
    };
    let mut body = body.child(bar);
    if f.changes_truncated {
        body = body.child(row().key("changes-truncated").w_full().px(12.0).py(6.0).child(text("This is a long diff: some files are left out.").font(t.font_small()).color(t.warn)));
    }
    body.child(content)
}

fn tree_label(n: &FileNode<Change>) -> &str {
    &n.name
}

/// The Files tab: the tree, or the file opened from it.
pub fn files(s: &App, t: &Theme, k: &Kit) -> El {
    let body = col().key("files-body").w_full().grow(1.0).min_h(0.0);
    if s.conn != Conn::Connected {
        return body.child(note(t, "files-note", "Not connected to the host."));
    }
    let f = &s.browse;
    if let Some(open) = &f.open {
        return body.child(file_bar(s, t, open)).child(file_view(t, k, open));
    }
    let filter = TextInput::new(&f.filter)
        .placeholder("Filter files")
        .leading_icon(k.glyph.search)
        .clear_button()
        .on_edit(|s: &mut App| &mut s.browse.filter)
        .on_change(|s: &mut App, _| s.apply_file_filter())
        .build(k)
        .key("files-filter")
        .grow(1.0)
        .min_w(0.0);
    let bar = strip(t).key("files-bar").px(8.0).child(filter).child(refresh_button(t));
    let content = match &f.listing {
        Load::Idle | Load::Loading => note(t, "files-note", "Loading files…"),
        Load::Failed(e) => note(t, "files-note", format!("Could not list the files: {e}")),
        Load::Ready if f.paths.is_empty() => note(t, "files-note", "This folder is empty."),
        Load::Ready => {
            let query = f.filter.text.trim().to_lowercase();
            let faint = t.text2;
            let tree = Tree::new(|s: &App| &s.browse.tree, |s: &mut App| &mut s.browse.tree, tree_label)
                .name("Files")
                .row(move |n, r| {
                    let item = TreeItem::file(n, r).highlight(query.clone());
                    match n.data {
                        Some(c) if c.added + c.removed > 0 => item.changes(c.added, c.removed),
                        Some(c) if c.new => item.badge("New"),
                        _ if n.dir => item.label_color(faint),
                        _ => item,
                    }
                })
                .on_open(|s: &mut App, id, _| {
                    let id = id.to_string();
                    s.open_file(&id);
                })
                .build(s, k)
                .grow(1.0)
                .min_h(0.0);
            col().key("files-tree-wrap").w_full().grow(1.0).min_h(0.0).px(6.0).pt(6.0).child(tree)
        }
    };
    let mut body = body.child(bar).child(content);
    if f.listing_truncated {
        body = body.child(row().key("files-truncated").w_full().px(12.0).py(6.0).child(text("Only the first 50,000 files are listed.").font(t.font_small()).color(t.warn)));
    }
    body
}

fn file_bar(s: &App, t: &Theme, open: &OpenFile) -> El {
    let (dir, name) = match open.path.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n.to_string()),
        None => (String::new(), open.path.clone()),
    };
    let meta = match (&open.load, open.binary) {
        (Load::Ready, false) => {
            let n = open.lines.len();
            let lines = if n == 1 { "1 line".to_string() } else { format!("{n} lines") };
            match open.lang {
                Some(l) => format!("{lines} · {l}"),
                None => lines,
            }
        }
        _ => String::new(),
    };
    let back = t.icon_button(&ic::CHEVRON_L, if t.mac() { 11.0 } else { 13.0 }, "Back to Files", |s: &mut App, tx: &mut Tx| {
        s.close_file();
        tx.focus("files-tree", false);
    });
    let mut bar = strip(t)
        .key("file-bar")
        .px(6.0)
        .gap(6.0)
        .child(back.key("file-back"))
        .child(
            row()
                .key("file-path")
                .min_w(0.0)
                .shrink()
                .child(text(dir).font(t.font()).color(t.text2).truncate().shrink().min_w(0.0))
                .child(text(name).font(TextStyle { weight: t.w_bold, ..t.font() }).color(t.text)),
        )
        .child(text(meta).key("file-meta").font(t.font_small()).color(t.text2))
        .child(el().grow(1.0));
    if s.browse.diff.file_index(&open.path).is_some() {
        let path = open.path.clone();
        bar = bar.child(
            row()
                .key("file-show-changes")
                .h(if t.mac() { 24.0 } else { 30.0 })
                .px(9.0)
                .center_y()
                .radius(t.btn_radius)
                .role("button")
                .label("Show Changes")
                .focusable(true)
                .fill(Color::WHITE.alpha(0.08))
                .hover(Color::WHITE.alpha(0.12))
                .on_press_tx(move |s: &mut App, tx: &mut Tx| s.show_file_changes(&path, tx))
                .child(text("Show Changes").font(t.font_small()).color(t.text)),
        );
    }
    bar
}

/// One file, read only: line numbers, then the code in the language's colours. Only the
/// lines in view are built.
fn file_view(t: &Theme, k: &Kit, open: &OpenFile) -> El {
    match &open.load {
        Load::Idle | Load::Loading => return note(t, "file-note", "Loading…"),
        Load::Failed(e) => return note(t, "file-note", format!("Could not open this file: {e}")),
        Load::Ready if open.binary => return note(t, "file-note", format!("Binary file · {}", size_label(open.size))),
        Load::Ready if open.lines.is_empty() => return note(t, "file-note", "This file is empty."),
        Load::Ready => {}
    }
    let font = t.font_mono();
    let lh = font.line_height;
    let adv = rune::text::advance("MMMMMMMMMM", font) / 10.0;
    let adv = if adv > 1.0 { adv } else { font.size * 0.6 };
    let digits = open.lines.len().to_string().len().max(2);
    let gutter = ((digits + 2) as f32 * adv).ceil();
    let width = gutter + ((open.columns + 2) as f32 * adv).ceil();
    let theme = SyntaxTheme::from_kit(k);
    let (ink, faint) = (t.text, t.text3);
    let path = open.path.clone();
    let build = move |s: &App, i: usize| -> El {
        let Some(f) = s.browse.open.as_ref().filter(|f| f.path == path && i < f.lines.len()) else { return el() };
        let (a, b) = f.lines[i];
        let line = &f.text[a as usize..b as usize];
        row()
            .h(lh)
            .child(text((i + 1).to_string()).font(font).color(faint).w(gutter).pr(adv * 1.5).align(Align::End).selectable(false))
            .child(rich(line.to_string(), f.spans(i, &theme)).font(font).color(ink).selectable(true))
    };
    let list = col()
        .key("file-rows")
        .w_full()
        .min_w(width)
        .py(6.0)
        .growth(GrowthSpec::default())
        .selection_group(true)
        .role("list")
        .label(open.path.clone())
        .virtual_rows(open.lines.len(), lh, |_: &App, i| Cow::Owned(format!("L{i}")), build);
    let scroll = col().key(format!("file-scroll-{}", open.path)).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).scroll_xy().child(list);
    let mut view = col().key("file-view").dir(rune::Direction::Ltr).w_full().grow(1.0).min_h(0.0).child(scroll);
    if open.truncated {
        view = view.child(row().key("file-truncated").w_full().px(12.0).py(6.0).child(text(format!("Showing the first 1 MB of {}.", size_label(open.size))).font(t.font_small()).color(t.warn)));
    }
    view
}
