//! The right panel's Browser tab: the host's browser for a workspace, which agents drive and
//! you watch and use here. The host runs Chromium without a window and sends the tab on
//! screen as JPEG frames; the bridge decodes them straight into a [`rune::NativeSurface`]
//! ([`Shown`]), and clicks, wheel and keys go back as `browser.input`.
//!
//! Each workspace has its own browser profile, so you sign in to a site once per workspace
//! and its sessions find you signed in. The panel shows the open session's workspace's
//! browser, and follows that session's agent to the tab it opens.
//!
//! A browser signed in to your accounts can act as you: the host gives it only to the app on
//! its own computer (the admin connection), like the terminal.

use crate::bridge::{BrowserCmd, Tag, ToHost};
use crate::commands::{label as keys, Cmd};
use crate::icons as ic;
use crate::look::Theme;
use crate::model::{App, Conn, PanelTab};
use crate::view::clip;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use rune::{col, el, icon, row, text, Color, El, Key, KeyEvent, TextEdit, TextStyle, Tx};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ---- frames -----------------------------------------------------------------------------

/// The tab on screen and its picture, shared with the bridge's frame decoder: frames of
/// any other tab are dropped there, undecoded.
#[derive(Clone, Default)]
pub struct Shown {
    state: Arc<Mutex<ShownState>>,
    pub surface: rune::NativeSurface,
}

#[derive(Default)]
struct ShownState {
    tab: Option<String>,
    /// The page's size in CSS pixels, as of the latest frame.
    page: Option<(u32, u32)>,
    frames: u64,
}

impl Shown {
    pub fn tab(&self) -> Option<String> {
        self.state.lock().unwrap().tab.clone()
    }

    /// Show `tab` from now on; its first frame replaces the last tab's picture.
    pub fn set_tab(&self, tab: Option<String>) {
        let mut st = self.state.lock().unwrap();
        if st.tab != tab {
            st.tab = tab;
            st.page = None;
            self.surface.clear();
        }
    }

    pub fn page(&self) -> Option<(u32, u32)> {
        self.state.lock().unwrap().page
    }

    /// How many frames have been drawn (tests).
    #[cfg(test)]
    pub fn frames(&self) -> u64 {
        self.state.lock().unwrap().frames
    }

    /// The bridge decoded a frame of `tab`: show it if that tab is still the one on screen.
    pub fn show(&self, tab: &str, frame_id: u64, width: u32, height: u32, bgra: Vec<u8>, page: (u32, u32)) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.tab.as_deref() != Some(tab) {
            return false;
        }
        let frame = rune::native_surface::ByteFrame { width, height, format: rune::native_surface::Format::Bgra, color: Default::default(), planes: [bgra, vec![]] };
        if self.surface.push_bytes(frame, frame_id).is_err() {
            return false;
        }
        st.page = Some(page);
        st.frames += 1;
        true
    }
}

/// A JPEG as BGRA pixels, and its size.
pub fn decode_jpeg(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA).set_max_width(8192).set_max_height(8192);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let px = d.decode().ok()?;
    let (w, h) = d.dimensions()?;
    (px.len() == w * h * 4).then_some((w as u32, h as u32, px))
}

// ---- state ------------------------------------------------------------------------------

#[derive(Default)]
pub struct Browse {
    /// Whether the host can run a browser, once it said.
    pub support: Option<BrowserSupport>,
    /// `browser.list` is on its way.
    listing: bool,
    /// Each workspace's browser, as the host last described it.
    pub browsers: HashMap<String, BrowserInfo>,
    /// The tab shown for each workspace.
    pub chosen: HashMap<String, String>,
    /// The tab this connection gets frames of.
    pub attached: Option<String>,
    /// The address field.
    pub url: TextEdit,
    /// The tab and address last put in the field, so a page that moves on updates it.
    url_of: Option<(String, String)>,
    /// `browser.open` is on its way.
    pub opening: bool,
    /// The view's size in points, once laid out.
    pub view: Option<(f32, f32)>,
    /// A mouse button is down in the view.
    pressed: bool,
    /// Tabs this connection could not attach to: not tried again until it reconnects.
    failed: std::collections::HashSet<String>,
    /// "Sign out everywhere" waits for its second press.
    pub confirm_clear: bool,
    /// Text to put on the clipboard on the next poll.
    pub copy_out: Option<String>,
    pub shown: Shown,
    /// What would have gone to the host, kept while there is no bridge (tests, snapshots).
    pub sent: Vec<BrowserCmd>,
}

fn host_of(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("")
}

fn tab_title(t: &BrowserTab) -> String {
    if !t.title.trim().is_empty() {
        return t.title.trim().to_string();
    }
    match host_of(&t.url) {
        "" | "blank" => "New Tab".into(),
        h => h.to_string(),
    }
}

impl App {
    /// Whether this connection may have the browser: only the host's own computer.
    pub fn browser_allowed(&self) -> bool {
        self.admin && self.info.as_ref().is_none_or(|i| i.protocol_version >= 12)
    }

    /// The workspace whose browser the panel shows: the open session's, else the one picked
    /// for a new session, else Quick tasks.
    pub fn browser_ws(&self) -> String {
        if let Some(x) = self.current() {
            return x.workspace_id.clone();
        }
        self.new_ws.clone().filter(|w| self.workspace(w).is_some()).unwrap_or_else(|| QUICK_WORKSPACE_ID.to_string())
    }

    fn browser_tabs(&self) -> &[BrowserTab] {
        self.web.browsers.get(&self.browser_ws()).map(|b| b.tabs.as_slice()).unwrap_or_default()
    }

    /// The tab on show in the panel's browser.
    pub fn browser_tab(&self) -> Option<&BrowserTab> {
        let ws = self.browser_ws();
        let id = self.web.chosen.get(&ws)?;
        self.browser_tabs().iter().find(|t| &t.tab_id == id)
    }

    /// Show or hide the Browser tab.
    pub fn toggle_browser(&mut self) -> bool {
        if self.panel_shown() == Some(PanelTab::Browser) {
            self.panel_open = false;
            return false;
        }
        self.close_agent();
        self.show_panel(PanelTab::Browser);
        true
    }

    /// Keep the frames coming for the tab on screen, and only for it: run on every poll, so
    /// the panel closing, another tab, session or workspace, or a new connection all land here.
    pub fn browser_tick(&mut self) {
        let shown = self.panel_shown() == Some(PanelTab::Browser) && self.conn == Conn::Connected && self.browser_allowed();
        if shown && self.web.support.is_none() && !self.web.listing {
            self.web.listing = true;
            self.call(Tag::Browsers, m::BROWSER_LIST, json!({}));
        }
        let ws = self.browser_ws();
        self.choose_tab(&ws);
        let want = if shown { self.web.chosen.get(&ws).cloned() } else { None };
        if want != self.web.attached {
            if let Some(old) = self.web.attached.take() {
                self.call(Tag::Quiet, m::BROWSER_DETACH, json!({"tab_id": old}));
            }
            if let Some(new) = &want {
                self.call(Tag::BrowserAttached(new.clone()), m::BROWSER_ATTACH, json!({"tab_id": new}));
            }
            self.web.attached = want.clone();
            self.web.shown.set_tab(want);
            self.web.pressed = false;
        }
        // The address field follows the page, unless you're typing in it.
        if !self.web.url.focused {
            let now = self.browser_tab().map(|t| (t.tab_id.clone(), t.url.clone()));
            if now != self.web.url_of {
                let shown_url = now.as_ref().map(|(_, u)| if u == "about:blank" { String::new() } else { u.clone() }).unwrap_or_default();
                self.web.url = TextEdit::new(&shown_url);
                self.web.url_of = now;
            }
        }
    }

    /// Keep a tab chosen for `ws` while it has any: the one you picked, else the open
    /// session's newest, else the newest.
    fn choose_tab(&mut self, ws: &str) {
        let Some(b) = self.web.browsers.get(ws) else { return };
        let ok = |t: &&BrowserTab| !self.web.failed.contains(&t.tab_id);
        if self.web.chosen.get(ws).is_some_and(|id| b.tabs.iter().filter(ok).any(|t| &t.tab_id == id)) {
            return;
        }
        let session = self.current().map(|x| x.session_id.clone());
        let pick = b.tabs.iter().filter(ok).rev().find(|t| t.session_id.is_some() && t.session_id == session).or(b.tabs.iter().filter(ok).last()).map(|t| t.tab_id.clone());
        match pick {
            Some(id) => self.web.chosen.insert(ws.to_string(), id),
            None => self.web.chosen.remove(ws),
        };
    }

    /// `browser.list` answered.
    pub fn browsers_listed(&mut self, list: Result<BrowserList, String>) {
        self.web.listing = false;
        match list {
            Ok(l) => {
                self.web.support = Some(l.support);
                self.web.browsers = l.browsers.into_iter().map(|b| (b.workspace_id.clone(), b)).collect();
            }
            Err(e) => self.web.support = Some(BrowserSupport { available: false, detail: e }),
        }
    }

    /// `browser.changed`: a workspace's browser started or stopped, or a tab changed.
    pub fn browser_changed(&mut self, b: BrowserInfo) {
        let ws = b.workspace_id.clone();
        // The open session's agent opened a tab: the panel follows it there.
        let session = self.current().filter(|x| x.workspace_id == ws).map(|x| x.session_id.clone());
        let known: Vec<String> = self.web.browsers.get(&ws).map(|o| o.tabs.iter().map(|t| t.tab_id.clone()).collect()).unwrap_or_default();
        if let Some(new) = b.tabs.iter().rev().find(|t| !known.contains(&t.tab_id) && t.session_id.is_some() && t.session_id == session) {
            self.web.chosen.insert(ws.clone(), new.tab_id.clone());
        }
        if b.error.is_none() && !b.running && b.tabs.is_empty() && b.blocked.is_empty() {
            self.web.browsers.remove(&ws);
        } else {
            self.web.browsers.insert(ws, b);
        }
    }

    /// A new connection gets frames only for what it attaches to.
    pub fn browsers_reset(&mut self) {
        self.web.attached = None;
        self.web.support = None;
        self.web.listing = false;
        self.web.opening = false;
        self.web.failed.clear();
        self.web.sent.clear();
        self.web.shown.set_tab(None);
    }

    pub fn browser_attach_failed(&mut self, id: &str) {
        // Not again on this connection (it would fail again on every poll), and ask the
        // host what it has now.
        self.web.failed.insert(id.to_string());
        self.web.chosen.retain(|_, t| t != id);
        for b in self.web.browsers.values_mut() {
            b.tabs.retain(|t| t.tab_id != id);
        }
        if self.web.attached.as_deref() == Some(id) {
            self.web.attached = None;
            self.web.shown.set_tab(None);
        }
        if !self.web.listing {
            self.web.listing = true;
            self.call(Tag::Browsers, m::BROWSER_LIST, json!({}));
        }
    }

    /// The page's size for a new tab of yours: the view's, so the page fits the panel.
    fn view_size(&self) -> (u32, u32) {
        let (w, h) = self.web.view.unwrap_or((BROWSER_DEFAULT_SIZE.0 as f32, BROWSER_DEFAULT_SIZE.1 as f32));
        ((w.round() as u32).clamp(BROWSER_MIN_SIZE, BROWSER_MAX_SIZE), (h.round() as u32).clamp(BROWSER_MIN_SIZE, BROWSER_MAX_SIZE))
    }

    /// Return in the address field: go there in this tab, or in a new one if none is open.
    pub fn browser_go(&mut self) {
        let typed = self.web.url.text.trim().to_string();
        if typed.is_empty() {
            return;
        }
        let url = match browser_url(&typed) {
            Ok(u) => u,
            Err(e) => {
                self.notice = Some(format!("Can't open that: {e}."));
                return;
            }
        };
        match self.browser_tab().map(|t| t.tab_id.clone()) {
            Some(id) => {
                self.act("Open the page", m::BROWSER_NAVIGATE, json!({"tab_id": id, "url": url}), crate::bridge::Refresh::None);
                self.web.url_of = None;
            }
            None => self.browser_new_tab(Some(url)),
        }
    }

    pub fn browser_new_tab(&mut self, url: Option<String>) {
        if self.web.opening {
            return;
        }
        let (w, h) = self.view_size();
        let mut p = json!({"workspace_id": self.browser_ws(), "width": w, "height": h});
        if let Some(u) = url {
            p["url"] = json!(u);
        }
        self.web.opening = true;
        self.call(Tag::BrowserOpened, m::BROWSER_OPEN, p);
    }

    /// `browser.open` answered.
    pub fn browser_opened(&mut self, r: Result<BrowserTab, String>) {
        self.web.opening = false;
        match r {
            Ok(t) => {
                let ws = self.browser_ws();
                let b = self.web.browsers.entry(ws.clone()).or_insert_with(|| BrowserInfo { workspace_id: ws.clone(), running: true, tabs: vec![], blocked: vec![], error: None });
                if !b.tabs.iter().any(|x| x.tab_id == t.tab_id) {
                    b.tabs.push(t.clone());
                }
                self.web.chosen.insert(ws, t.tab_id);
                self.web.url_of = None;
            }
            Err(e) => self.error = Some(format!("Could not open the browser: {e}")),
        }
    }

    pub fn select_browser_tab(&mut self, id: &str) {
        let ws = self.browser_ws();
        self.web.chosen.insert(ws, id.to_string());
    }

    pub fn close_browser_tab(&mut self, id: &str) {
        self.act("Close the tab", m::BROWSER_CLOSE_TAB, json!({"tab_id": id}), crate::bridge::Refresh::None);
    }

    pub fn browser_action(&mut self, a: BrowserAction) {
        if let Some(id) = self.browser_tab().map(|t| t.tab_id.clone()) {
            self.act("Go", m::BROWSER_NAVIGATE, json!({"tab_id": id, "action": a}), crate::bridge::Refresh::None);
        }
    }

    /// "Sign Out Everywhere": the workspace's browser forgets every sign-in.
    pub fn browser_clear(&mut self) {
        self.web.confirm_clear = false;
        let ws = self.browser_ws();
        self.web.chosen.remove(&ws);
        self.act("Sign out of the browser", m::BROWSER_CLEAR, json!({"workspace_id": ws}), crate::bridge::Refresh::None);
    }

    fn browser_send(&mut self, cmd: BrowserCmd) {
        match &self.bridge {
            Some(b) => b.send(ToHost::Browser(cmd)),
            None => self.web.sent.push(cmd),
        }
    }

    fn browser_input(&mut self, input: BrowserInput) {
        if self.conn != Conn::Connected {
            return;
        }
        if let Some(tab) = self.web.attached.clone() {
            self.browser_send(BrowserCmd::Input { tab, input });
        }
    }

    /// The view was laid out `w` by `h` points: a tab you opened follows the panel's size
    /// (an agent's keeps its own and is scaled to fit).
    pub fn browser_fit(&mut self, w: f32, h: f32) {
        self.web.view = Some((w, h));
        let size = self.view_size();
        let Some(t) = self.browser_tab() else { return };
        if t.session_id.is_some() || (t.width.abs_diff(size.0) < 2 && t.height.abs_diff(size.1) < 2) {
            return;
        }
        let tab = t.tab_id.clone();
        self.browser_send(BrowserCmd::Resize { tab, width: size.0, height: size.1 });
    }

    /// A point in the view, in the page's CSS pixels: the picture is fitted and centred.
    pub fn browser_point(&self, x: f32, y: f32) -> Option<(f32, f32, f32)> {
        let (vw, vh) = self.web.view?;
        let (pw, ph) = self.web.shown.page().or_else(|| self.browser_tab().map(|t| (t.width, t.height)))?;
        let (pw, ph) = (pw as f32, ph as f32);
        if pw < 1.0 || ph < 1.0 {
            return None;
        }
        let scale = (vw / pw).min(vh / ph);
        let (ox, oy) = ((vw - pw * scale) / 2.0, (vh - ph * scale) / 2.0);
        let (px, py) = ((x - ox) / scale, (y - oy) / scale);
        (px >= 0.0 && py >= 0.0 && px <= pw && py <= ph).then_some((px, py, scale))
    }

    pub fn browser_drag(&mut self, d: &rune::Drag, tx: &mut Tx) {
        let Some((x, y, _)) = self.browser_point(d.x, d.y).or_else(|| self.web.pressed.then(|| self.browser_point_clamped(d.x, d.y)).flatten()) else { return };
        let phase = match d.phase {
            rune::Phase::Down => {
                tx.focus("browser-view", false);
                self.web.pressed = true;
                BrowserPointer::Down
            }
            rune::Phase::Move => BrowserPointer::Move,
            rune::Phase::Up => {
                self.web.pressed = false;
                BrowserPointer::Up
            }
        };
        self.browser_input(BrowserInput::Mouse { phase, x, y, button: Some(BrowserButton::Left), clicks: Some(d.clicks.max(1)), mods: BrowserMods::default() });
    }

    /// A drag that leaves the picture keeps going at its edge.
    fn browser_point_clamped(&self, x: f32, y: f32) -> Option<(f32, f32, f32)> {
        let (vw, vh) = self.web.view?;
        self.browser_point(x.clamp(0.0, vw), y.clamp(0.0, vh)).or_else(|| {
            let (pw, ph) = self.web.shown.page()?;
            Some((x.clamp(0.0, pw as f32), y.clamp(0.0, ph as f32), 1.0))
        })
    }

    pub fn browser_hover(&mut self, x: f32, y: f32) {
        if self.web.pressed {
            return;
        }
        if let Some((x, y, _)) = self.browser_point(x, y) {
            self.browser_input(BrowserInput::Mouse { phase: BrowserPointer::Move, x, y, button: None, clicks: None, mods: BrowserMods::default() });
        }
    }

    pub fn browser_right_click(&mut self, x: f32, y: f32) {
        if let Some((x, y, _)) = self.browser_point(x, y) {
            for phase in [BrowserPointer::Down, BrowserPointer::Up] {
                self.browser_input(BrowserInput::Mouse { phase, x, y, button: Some(BrowserButton::Right), clicks: Some(1), mods: BrowserMods::default() });
            }
        }
    }

    pub fn browser_wheel(&mut self, e: &rune::WheelEvent) {
        let Some((x, y, scale)) = self.browser_point(e.x, e.y) else { return };
        // The wheel moves content with the fingers; the page scrolls the other way, in its pixels.
        let mods = BrowserMods { shift: e.mods.shift, ctrl: e.mods.ctrl, alt: e.mods.alt, meta: e.mods.logo };
        self.browser_input(BrowserInput::Wheel { x, y, dx: -e.dx / scale, dy: -e.dy / scale, mods });
    }

    pub fn browser_paste(&mut self, text: &str) {
        if !text.is_empty() {
            self.browser_input(BrowserInput::Text { text: text.to_string() });
        }
    }

    /// `browser.selection` answered: copy it.
    pub fn browser_copied(&mut self, text: String) {
        if !text.is_empty() {
            self.web.copy_out = Some(text);
        }
    }

    /// A key while the page has the keyboard. False for one that isn't the page's.
    pub fn browser_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let mac = self.look.mac();
        // The panel's own keys work from inside it.
        for cmd in [Cmd::ToggleBrowser, Cmd::ToggleTerminal] {
            if crate::commands::matches(self.look, cmd, ev, tx) {
                return false;
            }
        }
        let mods = BrowserMods { shift: ev.mods.shift, ctrl: ev.mods.ctrl, alt: ev.mods.alt, meta: ev.mods.logo };
        if let Key::Command(c) = &ev.key {
            let c = c.to_ascii_lowercase();
            match c {
                'c' | 'x' => {
                    if let Some(id) = self.web.attached.clone() {
                        self.call(Tag::BrowserCopied, m::BROWSER_SELECTION, json!({"tab_id": id}));
                    }
                    return true;
                }
                'v' => {
                    tx.read_clipboard(|s: &mut App, content, _| s.browser_paste(content.text()));
                    return true;
                }
                'l' => {
                    tx.focus("browser-url", false);
                    return true;
                }
                'r' => {
                    self.browser_action(BrowserAction::Reload);
                    return true;
                }
                '[' => {
                    self.browser_action(BrowserAction::Back);
                    return true;
                }
                ']' => {
                    self.browser_action(BrowserAction::Forward);
                    return true;
                }
                // Editing keys are the page's; every other shortcut stays the app's.
                'a' | 'z' | 'y' => {
                    let mods = BrowserMods { ctrl: !mac || mods.ctrl, meta: mac, ..mods };
                    self.browser_input(BrowserInput::Key { key: c.to_string(), text: None, mods });
                    return true;
                }
                _ => return false,
            }
        }
        let named = |k: &Key| -> Option<String> {
            Some(
                match k {
                    Key::Enter => "Enter",
                    Key::Backspace => "Backspace",
                    Key::Delete => "Delete",
                    Key::Left => "ArrowLeft",
                    Key::Right => "ArrowRight",
                    Key::Up => "ArrowUp",
                    Key::Down => "ArrowDown",
                    Key::Home => "Home",
                    Key::End => "End",
                    Key::PageUp => "PageUp",
                    Key::PageDown => "PageDown",
                    Key::Insert => "Insert",
                    Key::Escape => "Escape",
                    Key::Tab => "Tab",
                    Key::Function(n) => return Some(format!("F{n}")),
                    _ => return None,
                }
                .to_string(),
            )
        };
        if ev.key == Key::Function(5) {
            self.browser_action(BrowserAction::Reload);
            return true;
        }
        match &ev.key {
            Key::Text(t) if t.chars().count() == 1 => self.browser_input(BrowserInput::Key { key: t.clone(), text: Some(t.clone()), mods }),
            Key::Text(t) => self.browser_input(BrowserInput::Text { text: t.clone() }),
            k => match named(k) {
                Some(key) => self.browser_input(BrowserInput::Key { key, text: None, mods }),
                None => return false,
            },
        }
        true
    }
}

// ---- view -------------------------------------------------------------------------------

/// The Browser tab: the workspace's tabs as a strip, an address bar, and the page.
pub fn body(s: &App, t: &Theme) -> El {
    let mac = t.mac();
    let bg = t.content;
    let note = |msg: &str| col().key("browser-note").w_full().grow(1.0).center().px(24.0).gap(8.0).child(text(msg.to_string()).font(t.font_small()).color(t.text2).wrap().align(rune::Align::Center));
    let frame = |child: El| col().key("browser-body").w_full().grow(1.0).min_h(0.0).fill(bg).child(child);
    if !s.browser_allowed() {
        let msg = if s.admin { "This host is too old for the browser. Update it to use one here." } else { "The browser is signed in to your accounts on the host. It opens in the app on the host's own computer, not on a paired one." };
        return frame(note(msg));
    }
    if s.conn != Conn::Connected {
        return frame(note("Not connected to the host."));
    }
    match &s.web.support {
        None => return frame(note("Looking for a browser…")),
        Some(sup) if !sup.available => return frame(note(&sup.detail)),
        _ => {}
    }
    let ws = s.browser_ws();
    let ws_name = s.workspace(&ws).map(|w| w.name.clone()).unwrap_or_else(|| "this workspace".into());
    let info = s.web.browsers.get(&ws);
    let tab = s.browser_tab();

    // The tabs, and "+" for another.
    let mut strip = row().key("browser-tabs").w_full().gap(if mac { 4.0 } else { 6.0 }).px(8.0).py(6.0).center_y().role("tablist").focus_group();
    for x in info.map(|b| b.tabs.as_slice()).unwrap_or_default() {
        strip = strip.child(chip(s, t, x, tab.is_some_and(|c| c.tab_id == x.tab_id)));
    }
    let new = t.icon_button(&ic::PLUS, if mac { 12.0 } else { 14.0 }, "New Tab", |s: &mut App, tx: &mut Tx| {
        s.browser_new_tab(None);
        tx.focus("browser-url", false);
    });
    strip = strip.child(el().grow(1.0)).child(new.key("browser-new"));
    if mac {
        strip = strip.border_sides([0.0, 0.0, 0.5, 0.0], t.sep);
    }

    // Back, forward, reload (or stop), and the address.
    let (back, fwd, loading) = tab.map(|x| (x.can_go_back, x.can_go_forward, x.loading)).unwrap_or_default();
    let nav = |i: &'static rune::Icon, label: &str, on: bool, a: BrowserAction, key: &str| {
        t.icon_button(i, if mac { 12.0 } else { 14.0 }, label, move |s: &mut App, _: &mut Tx| s.browser_action(a)).key(key.to_string()).disabled(!on).opacity(if on { 1.0 } else { 0.4 })
    };
    let field = t
        .field_submit(&s.web.url, &format!("Search or enter an address ({ws_name})"), |s| &mut s.web.url, |s: &mut App, tx: &mut Tx| {
            s.browser_go();
            tx.focus("browser-view", false);
        })
        .key("browser-url");
    let bar = row()
        .key("browser-bar")
        .w_full()
        .gap(2.0)
        .px(6.0)
        .pb(6.0)
        .center_y()
        .child(nav(&ic::CHEVRON_L, "Back", back, BrowserAction::Back, "browser-back"))
        .child(nav(&ic::CHEVRON_R, "Forward", fwd, BrowserAction::Forward, "browser-forward"))
        .child(if loading { nav(&ic::CLOSE, "Stop", true, BrowserAction::Stop, "browser-stop") } else { nav(&ic::REFRESH, "Reload", tab.is_some(), BrowserAction::Reload, "browser-reload") })
        .child(row().grow(1.0).min_w(0.0).pl(4.0).child(field));

    let page = match tab {
        None => {
            let msg = if s.web.opening {
                "Opening…".to_string()
            } else {
                format!("The browser for {ws_name}. Type an address to open a page. Sign in to sites here once: agents in this workspace's sessions use the same browser, signed in as you.")
            };
            note(&msg)
        }
        Some(x) => {
            let waiting = s.web.shown.page().is_none();
            let label = format!("Web page: {}", tab_title(x));
            let mut v = col()
                .key("browser-view")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .center()
                .fill(Color::BLACK.alpha(0.25))
                .native_surface(&s.web.shown.surface, rune::SurfaceFit::Fit)
                .role("group")
                .label(label)
                .focusable(true)
                .on_key(|s: &mut App, ev: &KeyEvent, tx: &mut Tx| s.browser_key(ev, tx))
                .on_drag(|s: &mut App, d: &mut rune::Drag, tx: &mut Tx| s.browser_drag(d, tx))
                .on_pointer_move_local(|s: &mut App, p: rune::PointerEvent, _: &mut Tx| s.browser_hover(p.x, p.y))
                .on_secondary_press_local(|s: &mut App, p: rune::PointerEvent, _: &mut Tx| s.browser_right_click(p.x, p.y))
                .on_wheel(|s: &mut App, e: rune::WheelEvent, _: &mut Tx| {
                    s.browser_wheel(&e);
                    true
                })
                .on_layout(|s: &mut App, r: [f32; 4]| s.browser_fit(r[2], r[3]));
            if waiting {
                v = v.child(text(if x.loading { "Loading…" } else { "Waiting for the page…" }).font(t.font_small()).color(t.text2));
            }
            v
        }
    };

    let mut body = col().key("browser-body").w_full().grow(1.0).min_h(0.0).fill(bg).child(strip).child(bar).child(page);
    // What the workspace's network settings refused, and why the browser isn't running.
    let foot = |key: &str, child: El| row().key(key.to_string()).w_full().px(12.0).py(6.0).gap(8.0).center_y().border_sides([0.5, 0.0, 0.0, 0.0], t.sep).child(child);
    if let Some(e) = info.and_then(|b| b.error.as_ref()) {
        body = body.child(foot("browser-error", text(e.clone()).font(t.font_small()).color(t.danger).wrap().grow(1.0).min_w(0.0)));
    }
    if let Some(b) = info.filter(|b| !b.blocked.is_empty()) {
        let hosts = b.blocked.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
        let msg = format!("{ws_name}'s network settings refused {hosts}. Add hosts in Settings → Workspaces to allow them.");
        body = body.child(foot("browser-blocked", text(msg).font(t.font_small()).color(t.text2).wrap().grow(1.0).min_w(0.0)));
    }
    if let Some(x) = tab.filter(|x| x.session_id.is_some()) {
        let who = x.session_id.as_deref().and_then(|id| s.session(id)).map(|ss| ss.title.clone().unwrap_or_default()).filter(|t| !t.is_empty()).unwrap_or_else(|| "an agent".into());
        body = body.child(foot("browser-owner", text(format!("Opened by {}", clip(&who, 60))).font(t.font_small()).color(t.text2).truncate().grow(1.0).min_w(0.0)));
    }
    let clear = if s.web.confirm_clear {
        row()
            .gap(6.0)
            .center_y()
            .child(text(format!("Sign out of every site in {ws_name}'s browser?")).font(t.font_small()).color(t.text).wrap().grow(1.0).min_w(0.0))
            .child(t.small_button_danger("Sign Out", |s| s.browser_clear()).key("browser-clear-confirm"))
            .child(t.small_button("Cancel", |s| s.web.confirm_clear = false))
    } else {
        row()
            .gap(6.0)
            .center_y()
            .child(text(format!("Sign-ins are kept for {ws_name}.")).font(t.font_small()).color(t.text3).truncate().grow(1.0).min_w(0.0))
            .child(t.small_button("Sign Out Everywhere…", |s| s.web.confirm_clear = true).key("browser-clear"))
    };
    body.child(foot("browser-foot", clear.w_full()))
}

/// One tab in the strip: an agent's shows a bolt.
fn chip(s: &App, t: &Theme, tab: &BrowserTab, on: bool) -> El {
    let mac = t.mac();
    let id = tab.tab_id.clone();
    let id2 = id.clone();
    let title = tab_title(tab);
    let glyph = if tab.session_id.is_some() { &ic::BOLT } else { &ic::GLOBE };
    let who = tab.session_id.as_deref().and_then(|x| s.session(x)).and_then(|x| x.title.clone()).map(|t| format!(" (opened by {t})")).unwrap_or_default();
    let r = row()
        .key(format!("browser-tab-{id}"))
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
        .help(format!("{title} — {}{who}", tab.url))
        .checked(on)
        .focusable(true)
        .on_press(move |s: &mut App| s.select_browser_tab(&id))
        .child(icon(glyph, 12.0).color(if on { t.text } else { t.text2 }))
        .child(text(clip(&title, 40)).font(TextStyle::new(if mac { 12.0 } else { 13.5 }, if on && !mac { 700.0 } else { 400.0 })).color(if on { t.text } else { t.text2 }).truncate().shrink().min_w(0.0))
        .child(
            row()
                .key(format!("browser-close-{id2}"))
                .size(18.0, 18.0)
                .center()
                .radius(t.btn_radius)
                .role("button")
                .label(format!("Close {}", clip(&title, 40)))
                .hover(Color::WHITE.alpha(0.1))
                .on_press(move |s: &mut App| s.close_browser_tab(&id2))
                .child(icon(&ic::CLOSE, 8.0).color(t.text2)),
        );
    if on { r.fill(Color::WHITE.alpha(0.10)) } else { r.hover(Color::WHITE.alpha(0.05)) }
}

/// The toolbar's button for the browser.
pub fn toggle_button(s: &App, t: &Theme) -> El {
    let shown = s.panel_shown() == Some(PanelTab::Browser);
    let label = format!("{} Browser ({})", if shown { "Hide" } else { "Show" }, keys(s.look, Cmd::ToggleBrowser));
    let b = t.icon_button(&ic::GLOBE, if t.mac() { 17.0 } else { 16.0 }, &label, |s, tx| crate::commands::run(s, Cmd::ToggleBrowser, tx)).key("toggle-browser").role("button").checked(shown);
    if shown { b.fill(Color::WHITE.alpha(if t.mac() { 0.10 } else { 0.12 })) } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jpeg_becomes_bgra() {
        // 8x8 solid red, baseline JPEG.
        let jpeg = include_bytes!("../assets/test-red-8x8.jpg");
        let (w, h, px) = decode_jpeg(jpeg).expect("decodes");
        assert_eq!((w, h, px.len()), (8, 8, 8 * 8 * 4));
        let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
        assert!(r > 200 && g < 60 && b < 60 && a == 255, "{b} {g} {r} {a}");
        assert!(decode_jpeg(b"not a jpeg").is_none());
    }

    #[test]
    fn frames_of_another_tab_are_not_shown() {
        let shown = Shown::default();
        shown.set_tab(Some("a".into()));
        assert!(!shown.show("b", 1, 2, 2, vec![0; 16], (2, 2)));
        assert!(shown.show("a", 1, 2, 2, vec![0; 16], (2, 2)));
        assert_eq!((shown.frames(), shown.page()), (1, Some((2, 2))));
        shown.set_tab(Some("b".into()));
        assert_eq!(shown.page(), None, "a new tab waits for its own picture");
    }
}
