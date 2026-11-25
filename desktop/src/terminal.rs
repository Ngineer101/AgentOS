//! The right panel's Terminal tab: shells the host runs for you, each drawn by an emulator
//! here ([`crate::term::Emu`]). The host owns the shell, so a terminal carries on while the
//! app is closed and shows what it printed when the app comes back.
//!
//! A terminal is a shell as you, outside every sandbox: the host gives it only to the app
//! on its own computer (the admin connection), not to a paired device.

use crate::bridge::{Tag, TermCmd, ToHost};
use crate::commands::{label as keys, Cmd};
use crate::icons as ic;
use crate::look::Theme;
use crate::model::{App, Conn, PanelTab, Screen};
use crate::term::{color, Emu, Palette};
use crate::view::clip;
use agenthost_proto::methods as m;
use agenthost_proto::{Terminal, TerminalChanged, TerminalOutput};
use alacritty_terminal::vte::ansi::Rgb;
use rune::{col, el, icon, row, text, Color, El, KeyEvent, TextStyle, Tx};
use serde_json::json;

/// Tell the host what has been drawn every this many bytes (its window is far larger).
const ACK_EVERY: u64 = 64 * 1024;
/// A shell that ends this soon after starting failed to start: its tab stays, with what
/// it printed, instead of closing as one you left with `exit` does.
const DIED_YOUNG_MS: i64 = 3000;
/// Space between the pane's edge and the cells.
const PAD: f32 = 8.0;
/// The size a terminal starts with before the pane has been laid out.
const DEFAULT_SIZE: (u16, u16) = (80, 24);
pub const PANEL_MIN_W: f32 = 320.0;
pub const PANEL_MAX_W: f32 = 1200.0;

/// One of the host's terminals, as this app shows it.
pub struct TermTab {
    pub info: Terminal,
    pub emu: Emu,
    /// This connection asked for its output.
    attached: bool,
    /// How much of its output has been drawn, and how much of that the host knows of.
    pos: u64,
    acked: u64,
}

impl TermTab {
    fn new(info: Terminal, size: (u16, u16)) -> TermTab {
        TermTab { info, emu: Emu::new(size.0, size.1), attached: false, pos: 0, acked: 0 }
    }

    /// The tab's name: what the shell calls itself, else the folder it started in.
    pub fn title(&self) -> String {
        if let Some(t) = &self.emu.title {
            return t.clone();
        }
        let cwd = self.info.cwd.trim_end_matches('/');
        let name = cwd.rsplit('/').next().unwrap_or_default();
        if name.is_empty() { "/".into() } else { name.to_string() }
    }
}

/// Every terminal the host has, and which one shows.
#[derive(Default)]
pub struct Terms {
    pub tabs: Vec<TermTab>,
    pub selected: Option<String>,
    /// `terminal.open` is on its way.
    pub opening: bool,
    /// The pane's size in cells, once it has been laid out.
    pub fit: Option<(u16, u16)>,
    /// Put the keyboard in the terminal once there is one to put it in: you asked for a
    /// terminal, and the shell was still starting.
    pub focus_wanted: bool,
    /// Wheel travel not yet a whole line.
    wheel: f32,
    /// What would have gone to the host, kept while there is no bridge (tests, snapshots).
    pub sent: Vec<TermCmd>,
}

impl Terms {
    pub fn current(&self) -> Option<&TermTab> {
        let id = self.selected.as_deref()?;
        self.tabs.iter().find(|t| t.info.terminal_id == id)
    }

    fn current_mut(&mut self) -> Option<&mut TermTab> {
        let id = self.selected.as_deref()?;
        self.tabs.iter_mut().find(|t| t.info.terminal_id == id)
    }

    fn size(&self) -> (u16, u16) {
        self.fit.unwrap_or(DEFAULT_SIZE)
    }
}

/// The pane's background as sRGB bytes, for the palette (and for programs that ask).
fn pane_bg(t: &Theme) -> Rgb {
    let h = t.content.hex();
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("1e"), 16).unwrap_or(0x1e);
    Rgb { r: p(1), g: p(3), b: p(5) }
}

fn palette(t: &Theme) -> Palette {
    Palette::dark(pane_bg(t))
}

/// The terminal's font, and its cell: one character's advance by the line height.
fn metrics(t: &Theme) -> (TextStyle, f32, f32) {
    let style = t.font_mono();
    let w = rune::text::advance("MMMMMMMMMM", style) / 10.0;
    (style, if w > 1.0 { w } else { style.size * 0.6 }, style.line_height)
}

impl App {
    /// Which tab of the right panel is on screen, if the panel is. Side questions belong
    /// to a session: on a screen without any, the panel waits rather than turning into a
    /// terminal nobody asked for.
    pub fn panel_shown(&self) -> Option<PanelTab> {
        if !self.panel_open || self.agent_visible() {
            return None;
        }
        match self.panel_tab {
            PanelTab::Terminal => Some(PanelTab::Terminal),
            PanelTab::Side => matches!(&self.screen, Screen::Session(id) if self.has_side(id)).then_some(PanelTab::Side),
        }
    }

    pub fn show_panel(&mut self, tab: PanelTab) {
        self.panel_open = true;
        self.panel_tab = tab;
        if tab == PanelTab::Terminal {
            self.terminal_shown();
        }
    }

    /// The panel's width: the side questions' own, or the terminal's (which drags).
    pub fn panel_width(&self, t: &Theme) -> f32 {
        match self.panel_shown() {
            Some(PanelTab::Terminal) => self.panel_w.unwrap_or(if t.mac() { 520.0 } else { 560.0 }).clamp(PANEL_MIN_W, PANEL_MAX_W),
            _ => crate::side::width(t),
        }
    }

    /// Drag the panel's edge to `w` (clamped); `save` on release keeps it between launches.
    pub fn resize_panel(&mut self, w: f32, save: bool) {
        self.panel_w = Some(w.clamp(PANEL_MIN_W, PANEL_MAX_W));
        if save && self.save_prefs {
            let mut p = crate::prefs::load();
            p.panel_w = self.panel_w;
            crate::prefs::save(&p);
        }
    }

    /// Whether this connection may have a terminal: only the host's own computer.
    pub fn terminal_allowed(&self) -> bool {
        self.admin && self.info.as_ref().is_none_or(|i| i.protocol_version >= 10)
    }

    /// Show or hide the Terminal tab. Returns whether it shows now (so the caller can
    /// put the keyboard in it).
    pub fn toggle_terminal(&mut self) -> bool {
        if self.panel_shown() == Some(PanelTab::Terminal) {
            self.panel_open = false;
            return false;
        }
        self.close_agent();
        self.show_panel(PanelTab::Terminal);
        true
    }

    /// The Terminal tab came on screen: start a shell if there is none, and make sure the
    /// one that shows is sending its output.
    fn terminal_shown(&mut self) {
        if self.conn != Conn::Connected || !self.terminal_allowed() {
            return;
        }
        if self.terms.tabs.is_empty() {
            self.open_terminal();
        } else {
            if self.terms.current().is_none() {
                self.terms.selected = self.terms.tabs.first().map(|t| t.info.terminal_id.clone());
            }
            self.attach_selected();
        }
    }

    /// Where a new terminal starts: the open session's own folder (its worktree, else its
    /// workspace's folder), or your home folder.
    fn terminal_cwd(&self) -> Option<String> {
        let x = self.current()?;
        match &x.worktree {
            Some(w) => Some(w.path.clone()),
            None => self.workspace(&x.workspace_id).map(|w| w.path.clone()),
        }
    }

    pub fn open_terminal(&mut self) {
        if self.terms.opening {
            return;
        }
        let (cols, rows) = self.terms.size();
        let mut p = json!({"cols": cols, "rows": rows});
        if let Some(cwd) = self.terminal_cwd() {
            p["cwd"] = json!(cwd);
        }
        self.terms.opening = true;
        self.call(Tag::TerminalOpened, m::TERMINAL_OPEN, p);
    }

    /// `terminal.open` answered.
    pub fn terminal_opened(&mut self, result: Result<Terminal, String>) {
        self.terms.opening = false;
        match result {
            Ok(t) => {
                let id = t.terminal_id.clone();
                self.upsert_terminal(t);
                self.select_terminal(&id);
            }
            Err(e) => self.error = Some(format!("Could not open a terminal: {e}")),
        }
    }

    fn upsert_terminal(&mut self, t: Terminal) {
        match self.terms.tabs.iter_mut().find(|x| x.info.terminal_id == t.terminal_id) {
            Some(x) => x.info = t,
            None => {
                let size = self.terms.size();
                self.terms.tabs.push(TermTab::new(t, size));
            }
        }
    }

    /// `terminal.list` answered (after connecting): the host's terminals, which may have
    /// been running since before this app started.
    pub fn terminals_listed(&mut self, list: Vec<Terminal>) {
        self.terms.tabs.retain(|x| list.iter().any(|t| t.terminal_id == x.info.terminal_id));
        for t in list {
            self.upsert_terminal(t);
        }
        if self.terms.current().is_none() {
            self.terms.selected = self.terms.tabs.first().map(|t| t.info.terminal_id.clone());
        }
        if self.panel_shown() == Some(PanelTab::Terminal) {
            self.terminal_shown();
        }
    }

    /// A new connection has no attachments: every terminal's screen is drawn again from
    /// what the host kept.
    pub fn terminals_reset(&mut self) {
        self.terms.tabs.clear();
        self.terms.opening = false;
        self.terms.sent.clear();
    }

    pub fn select_terminal(&mut self, id: &str) {
        if self.terms.tabs.iter().any(|t| t.info.terminal_id == id) {
            self.terms.selected = Some(id.to_string());
            self.attach_selected();
        }
    }

    /// Ask for the selected terminal's output (once per connection) and give it the
    /// pane's size.
    fn attach_selected(&mut self) {
        let size = self.terms.size();
        let Some(tab) = self.terms.current_mut() else { return };
        let id = tab.info.terminal_id.clone();
        let (attach, resize) = (!tab.attached, tab.emu.size() != size || (tab.info.cols, tab.info.rows) != size);
        tab.emu.resize(size.0, size.1);
        if attach {
            tab.attached = true;
            self.call(Tag::TerminalAttached(id.clone()), m::TERMINAL_ATTACH, json!({"terminal_id": id}));
        }
        if resize && self.terms.fit.is_some() {
            self.term_send(TermCmd::Resize { id, cols: size.0, rows: size.1 });
        }
    }

    /// `terminal.attach` failed: the terminal is gone, or the host is older.
    pub fn terminal_attach_failed(&mut self, id: &str) {
        if let Some(tab) = self.terms.tabs.iter_mut().find(|t| t.info.terminal_id == id) {
            tab.attached = false;
        }
    }

    fn term_send(&mut self, cmd: TermCmd) {
        match &self.bridge {
            Some(b) => b.send(ToHost::Term(cmd)),
            None => self.terms.sent.push(cmd),
        }
    }

    /// `terminal.output`: the shell printed something.
    pub fn terminal_output(&mut self, o: TerminalOutput) {
        use base64::Engine as _;
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(o.data.as_bytes()) else { return };
        let palette = palette(&Theme::new(self.look));
        let Some(tab) = self.terms.tabs.iter_mut().find(|t| t.info.terminal_id == o.terminal_id) else { return };
        // The first piece, or one after a gap the host skipped: carry on from where it says.
        tab.pos = o.offset;
        let answer = tab.emu.feed(&data, o.replay, &palette);
        tab.pos += data.len() as u64;
        let ack = (tab.pos - tab.acked.min(tab.pos) >= ACK_EVERY).then_some(tab.pos);
        if let Some(offset) = ack {
            tab.acked = offset;
        }
        let id = o.terminal_id;
        if !answer.is_empty() {
            self.term_send(TermCmd::Input { id: id.clone(), data: answer });
        }
        if let Some(offset) = ack {
            self.term_send(TermCmd::Ack { id, offset });
        }
    }

    /// `terminal.changed`: a terminal opened (here or from another window), its shell
    /// ended, or it was closed.
    pub fn terminal_changed(&mut self, ch: TerminalChanged) {
        let id = ch.terminal.terminal_id.clone();
        if ch.removed {
            let at = self.terms.tabs.iter().position(|t| t.info.terminal_id == id);
            self.terms.tabs.retain(|t| t.info.terminal_id != id);
            if self.terms.selected.as_deref() == Some(id.as_str()) {
                // The tab that took its place, else the one before.
                let next = at.and_then(|i| self.terms.tabs.get(i).or_else(|| self.terms.tabs.last())).map(|t| t.info.terminal_id.clone());
                self.terms.selected = next;
                if self.panel_shown() == Some(PanelTab::Terminal) {
                    self.attach_selected();
                }
            }
            // The last terminal went: so does the panel, as a terminal window closes
            // when you leave its shell.
            if self.terms.tabs.is_empty() && self.panel_tab == PanelTab::Terminal && !self.terms.opening {
                self.panel_open = false;
            }
            return;
        }
        let ended = ch.terminal.exited.is_some();
        let young = crate::view::now_ms() - ch.terminal.created_at < DIED_YOUNG_MS;
        self.upsert_terminal(ch.terminal);
        if ended && !young {
            // You left the shell: its tab closes.
            self.call(Tag::Quiet, m::TERMINAL_CLOSE, json!({"terminal_id": id}));
        }
    }

    /// The terminal you asked for is on screen now: whether to give it the keyboard.
    pub fn take_terminal_focus(&mut self) -> bool {
        let ready = self.terms.focus_wanted && self.terms.current().is_some() && self.panel_shown() == Some(PanelTab::Terminal);
        if ready {
            self.terms.focus_wanted = false;
        }
        ready
    }

    pub fn close_terminal(&mut self, id: &str) {
        self.act("Close the terminal", m::TERMINAL_CLOSE, json!({"terminal_id": id}), crate::bridge::Refresh::None);
    }

    /// The pane was laid out `w` by `h` points: fit the terminal to it.
    pub fn terminal_fit(&mut self, w: f32, h: f32, cell: (f32, f32)) {
        let cols = (((w - 2.0 * PAD) / cell.0).floor() as i32).clamp(2, i32::from(agenthost_proto::TERMINAL_MAX_COLS)) as u16;
        let rows = (((h - 2.0 * PAD) / cell.1).floor() as i32).clamp(1, i32::from(agenthost_proto::TERMINAL_MAX_ROWS)) as u16;
        if self.terms.fit == Some((cols, rows)) {
            return;
        }
        self.terms.fit = Some((cols, rows));
        let Some(tab) = self.terms.current_mut() else { return };
        tab.emu.resize(cols, rows);
        let id = tab.info.terminal_id.clone();
        self.term_send(TermCmd::Resize { id, cols, rows });
    }

    /// Bytes for the selected terminal's shell. Typing brings the view back to the
    /// prompt and drops the selection, as terminals do.
    pub fn terminal_input(&mut self, data: Vec<u8>) {
        if data.is_empty() || self.conn != Conn::Connected {
            return;
        }
        let Some(tab) = self.terms.current_mut().filter(|t| t.info.exited.is_none()) else { return };
        tab.emu.scroll_to_bottom();
        tab.emu.clear_selection();
        let id = tab.info.terminal_id.clone();
        self.term_send(TermCmd::Input { id, data });
    }

    pub fn terminal_paste(&mut self, text: &str) {
        if let Some(data) = self.terms.current().map(|t| t.emu.paste(text)) {
            self.terminal_input(data);
        }
    }

    /// A key while the terminal has the keyboard. False for a key that isn't its own.
    pub fn terminal_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let mac = self.look.mac();
        // The key that shows and hides the terminal works from inside it too.
        if crate::commands::matches(self.look, Cmd::ToggleTerminal, ev, tx) {
            return false;
        }
        // Copy and paste: ⌘C / ⌘V, or Ctrl+Shift+C / Ctrl+Shift+V (Ctrl+C is the shell's).
        let chord = if mac { ev.mods.logo && !ev.mods.ctrl && !ev.mods.alt } else { ev.mods.ctrl && ev.mods.shift && !ev.mods.alt };
        if chord && ev.is_command('c') {
            if let Some(sel) = self.terms.current().and_then(|t| t.emu.selection_text()) {
                tx.copy(sel);
            }
            return true;
        }
        if chord && ev.is_command('v') {
            tx.read_clipboard(|s: &mut App, content, _| s.terminal_paste(content.text()));
            return true;
        }
        let Some(tab) = self.terms.current_mut() else { return false };
        // Shift+Page Up / Down read back through what scrolled away.
        if ev.mods.shift && !tab.emu.alt_screen() && matches!(ev.key, rune::Key::PageUp | rune::Key::PageDown) {
            let page = i32::from(tab.emu.size().1.saturating_sub(1).max(1));
            tab.emu.scroll(if ev.key == rune::Key::PageUp { page } else { -page });
            return true;
        }
        match tab.emu.key(ev, tx.key_base(), mac) {
            Some(data) => {
                self.terminal_input(data);
                true
            }
            None => false,
        }
    }

    /// The wheel over the terminal, `dy` points: through the scrollback, or as arrow keys
    /// for a full-screen program.
    pub fn terminal_wheel(&mut self, dy: f32, cell_h: f32) {
        self.terms.wheel += dy;
        let lines = (self.terms.wheel / cell_h).trunc();
        if lines == 0.0 {
            return;
        }
        self.terms.wheel -= lines * cell_h;
        let Some(tab) = self.terms.current_mut() else { return };
        if tab.emu.alt_screen() {
            let (id, data) = (tab.info.terminal_id.clone(), tab.emu.wheel_keys(lines as i32));
            self.term_send(TermCmd::Input { id, data });
        } else {
            tab.emu.scroll(lines as i32);
        }
    }

    /// A press or drag over the cells selects text.
    pub fn terminal_drag(&mut self, d: &rune::Drag, cell: (f32, f32)) {
        let Some(tab) = self.terms.current_mut() else { return };
        let (cols, rows) = tab.emu.size();
        let x = ((d.x - PAD) / cell.0).max(0.0);
        let col = (x.floor() as u16).min(cols - 1);
        let row = ((((d.y - PAD) / cell.1).max(0.0)).floor() as u16).min(rows - 1);
        let right = x.fract() >= 0.5;
        match d.phase {
            rune::Phase::Down => tab.emu.select_start(col, row, right, d.clicks),
            rune::Phase::Move => tab.emu.select_to(col, row, right),
            rune::Phase::Up => {
                if tab.emu.selection_text().is_none() {
                    tab.emu.clear_selection();
                }
            }
        }
    }
}

impl App {
    /// A terminal as if the host had announced it and sent its output: for the sample
    /// scenes and tests, which have no host.
    pub fn terminal_sample(&mut self, id: &str, cwd: &str, age_ms: i64, exited: Option<agenthost_proto::TerminalExit>, output: &str) {
        use base64::Engine as _;
        let terminal = Terminal { terminal_id: id.into(), cwd: cwd.into(), shell: "/bin/zsh".into(), cols: 80, rows: 24, created_at: crate::view::now_ms() - age_ms, exited: None };
        self.terminal_changed(TerminalChanged { terminal: terminal.clone(), removed: false });
        let data = base64::engine::general_purpose::STANDARD.encode(output);
        self.terminal_output(TerminalOutput { terminal_id: id.into(), offset: 0, data, replay: false });
        if exited.is_some() {
            self.terminal_changed(TerminalChanged { terminal: Terminal { exited, ..terminal }, removed: false });
        }
    }
}

/// The Terminal tab: its terminals as a strip, and the selected one's screen.
pub fn body(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let bg = color(pane_bg(t));
    let note = |msg: &str| col().key("term-note").w_full().grow(1.0).center().px(24.0).child(text(msg.to_string()).font(t.font_small()).color(t.text2).wrap().align(rune::Align::Center));
    if !s.terminal_allowed() {
        let msg = if s.admin { "This host is too old for terminals. Update it to use one here." } else { "A terminal is a shell on the host, outside every sandbox. It opens in the app on the host's own computer, not on a paired one." };
        return col().key("term-body").w_full().grow(1.0).min_h(0.0).fill(bg).child(note(msg));
    }
    if s.conn != Conn::Connected {
        return col().key("term-body").w_full().grow(1.0).min_h(0.0).fill(bg).child(note("Not connected to the host."));
    }

    // The terminals, and "+" for another in the open session's folder.
    let mut strip = row().key("term-tabs").w_full().gap(if mac { 4.0 } else { 6.0 }).px(8.0).py(6.0).center_y().role("tablist").focus_group();
    for tab in &s.terms.tabs {
        strip = strip.child(chip(s, t, tab));
    }
    let new = t
        .icon_button(&ic::PLUS, if mac { 12.0 } else { 14.0 }, "New Terminal", |s: &mut App, tx: &mut Tx| {
            s.open_terminal();
            s.terms.focus_wanted = true;
            tx.focus("term-view", false);
        })
        .key("term-new");
    strip = strip.child(el().grow(1.0)).child(new);
    if mac {
        strip = strip.border_sides([0.0, 0.0, 0.5, 0.0], t.sep);
    }

    let Some(tab) = s.terms.current() else {
        let msg = if s.terms.opening { "Starting your shell…" } else { "No terminal is open." };
        return col().key("term-body").w_full().grow(1.0).min_h(0.0).fill(bg).child(strip).child(note(msg));
    };

    let (style, cw, ch) = metrics(t);
    let (cols, rows) = tab.emu.size();
    let mut grid = rune::grid(cols, cw, ch, tab.emu.grid_rows(&palette(t), style)).key("term-grid").w(f32::from(cols) * cw).h(f32::from(rows) * ch);
    if let Some((c, r)) = tab.emu.cursor() {
        grid = grid.grid_caret(c, r);
    }
    let view = col()
        .key("term-view")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .padding(PAD)
        .role("group")
        .label(format!("Terminal: {}", tab.title()))
        .cursor(rune::Cursor::Text)
        .focusable(true)
        .on_key(|s: &mut App, ev: &KeyEvent, tx: &mut Tx| s.terminal_key(ev, tx))
        .on_wheel(move |s: &mut App, e: rune::WheelEvent, _: &mut Tx| {
            s.terminal_wheel(e.dy, ch);
            true
        })
        .on_drag(move |s: &mut App, d: &mut rune::Drag, _: &mut Tx| s.terminal_drag(d, (cw, ch)))
        .on_layout(move |s: &mut App, r: [f32; 4]| s.terminal_fit(r[2], r[3], (cw, ch)))
        .child(grid);

    let mut body = col().key("term-body").w_full().grow(1.0).min_h(0.0).fill(bg).child(strip).child(view);
    // A shell that never got going says why in its last output; say that it ended.
    if let Some(exit) = tab.info.exited {
        let how = match (exit.code, exit.signal) {
            (Some(c), _) => format!("The shell ended (exit code {c})."),
            (None, Some(sig)) => format!("The shell was ended by signal {sig}."),
            (None, None) => "The shell ended.".to_string(),
        };
        body = body.child(row().key("term-ended").w_full().px(12.0).py(8.0).border_sides([0.5, 0.0, 0.0, 0.0], t.sep).child(text(how).font(t.font_small()).color(t.text2)));
    } else if tab.emu.scrolled() > 0 {
        let n = tab.emu.scrolled();
        body = body.child(
            row()
                .key("term-scrolled")
                .w_full()
                .px(12.0)
                .py(5.0)
                .center_y()
                .border_sides([0.5, 0.0, 0.0, 0.0], t.sep)
                .hover(Color::WHITE.alpha(0.04))
                .role("button")
                .label("Back to the prompt")
                .on_press(|s: &mut App| {
                    if let Some(tab) = s.terms.current_mut() {
                        tab.emu.scroll_to_bottom();
                    }
                })
                .child(text(format!("{n} line{} up · back to the prompt", if n == 1 { "" } else { "s" })).font(t.font_small()).color(t.text2)),
        );
    }
    body
}

/// One terminal in the strip: its name, and a button to close it.
fn chip(s: &App, t: &Theme, tab: &TermTab) -> El {
    let mac = t.mac();
    let id = tab.info.terminal_id.clone();
    let id2 = id.clone();
    let on = s.terms.selected.as_deref() == Some(id.as_str());
    let title = tab.title();
    let mut r = row()
        .key(format!("term-tab-{id}"))
        .h(if mac { 24.0 } else { 32.0 })
        .min_w(0.0)
        .shrink()
        .max_w(if mac { 170.0 } else { 180.0 })
        .gap(6.0)
        .pl(if mac { 8.0 } else { 10.0 })
        .pr(4.0)
        .center_y()
        .radius(t.btn_radius)
        .role("tab")
        .label(title.clone())
        .help(format!("{} — {}", title, tab.info.cwd))
        .checked(on)
        .focusable(true)
        .on_press_tx(move |s: &mut App, tx: &mut Tx| {
            s.select_terminal(&id);
            tx.focus("term-view", false);
        })
        .child(icon(&ic::TERMINAL, 12.0).color(if on { t.text } else { t.text2 }))
        .child(text(clip(&title, 40)).font(TextStyle::new(if mac { 12.0 } else { 13.5 }, if on && !mac { 700.0 } else { 400.0 })).color(if on { t.text } else { t.text2 }).truncate().shrink().min_w(0.0))
        .child(
            row()
                .key(format!("term-close-{id2}"))
                .size(18.0, 18.0)
                .center()
                .radius(t.btn_radius)
                .role("button")
                .label(format!("Close {}", clip(&title, 40)))
                .help("Close Terminal (ends its shell)")
                .hover(Color::WHITE.alpha(0.1))
                .on_press(move |s: &mut App| s.close_terminal(&id2))
                .child(icon(&ic::CLOSE, 8.0).color(t.text2)),
        );
    r = if on { r.fill(Color::WHITE.alpha(0.10)) } else { r.hover(Color::WHITE.alpha(0.05)) };
    r
}

/// The toolbar's button for the terminal.
pub fn toggle_button(s: &App, t: &Theme) -> El {
    let shown = s.panel_shown() == Some(PanelTab::Terminal);
    let label = format!("{} Terminal ({})", if shown { "Hide" } else { "Show" }, keys(s.look, Cmd::ToggleTerminal));
    let b = t.icon_button(&ic::TERMINAL, if t.mac() { 18.0 } else { 17.0 }, &label, |s, tx| crate::commands::run(s, Cmd::ToggleTerminal, tx)).key("toggle-terminal").role("button").checked(shown);
    if shown { b.fill(Color::WHITE.alpha(if t.mac() { 0.10 } else { 0.12 })) } else { b }
}
