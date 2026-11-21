//! Two looks from one codebase: macOS (AppKit patterns) and Linux (GNOME / libadwaita), with
//! the design tokens' values. The look follows the OS; `AGENTHOST_DESKTOP_LOOK=macos|gnome`
//! picks one (to render both headlessly on one machine).

use crate::model::App;
use rune::color::hex;
use rune::{col, el, icon, row, text, Color, El, Icon, TextStyle, Tx};
use rune_kit::Kit;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Mac,
    Gnome,
}

impl Look {
    /// The look for this run: the env override, else the OS.
    pub fn current() -> Look {
        match std::env::var("AGENTHOST_DESKTOP_LOOK").ok().as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("gnome" | "linux" | "adwaita") => Look::Gnome,
            Some("macos" | "mac") => Look::Mac,
            _ if cfg!(target_os = "macos") => Look::Mac,
            _ => Look::Gnome,
        }
    }
    pub fn mac(self) -> bool {
        self == Look::Mac
    }
}

fn c(h: &str) -> Color {
    hex(h).unwrap_or(Color::BLACK)
}
fn white(a: f32) -> Color {
    Color::WHITE.alpha(a)
}
fn black(a: f32) -> Color {
    Color::BLACK.alpha(a)
}

/// Colours and sizes for one look.
#[derive(Clone)]
pub struct Theme {
    pub look: Look,
    pub window: Color,
    pub sidebar_top: Color,
    pub sidebar_bottom: Color,
    pub toolbar: Color,
    pub content: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub sep: Color,
    pub accent: Color,
    /// Accent as text (links, "Add Folder…").
    pub accent_text: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
    pub bolt: Color,
    pub control: Color,
    /// A grouped box (Settings forms, sheet lists), or an Adwaita card.
    pub boxed: Color,
    /// Tool call rows and similar cards in the transcript.
    pub card: Color,
    pub card_border: Color,
    pub code_bg: Color,
    pub row_selected: Color,
    pub row_hover: Color,
    pub sheet: Color,
    pub panel: Color,
    pub field: Color,
    pub field_border: Color,
    pub focus: Color,
    pub warn_tint: Color,
    pub warn_border: Color,
    // Type.
    pub fs: f32,
    pub fs_small: f32,
    pub fs_mono: f32,
    pub fs_mono_small: f32,
    pub fs_msg: f32,
    pub w_bold: f32,
    // Metrics.
    pub btn_h: f32,
    pub btn_radius: f32,
    pub toolbar_h: f32,
    pub sidebar_w: f32,
    pub column: f32,
    pub row_radius: f32,
    pub box_radius: f32,
    pub card_radius: f32,
}

impl Theme {
    pub fn new(look: Look) -> Theme {
        match look {
            Look::Mac => Theme {
                look,
                window: c("#1e1e1e"),
                sidebar_top: c("#3a3842").alpha(0.92),
                sidebar_bottom: c("#2c2a32").alpha(0.92),
                toolbar: c("#2b2b2d"),
                content: c("#1e1e1e"),
                text: white(0.85),
                text2: white(0.55),
                text3: white(0.28),
                sep: white(0.10),
                accent: c("#0a84ff"),
                accent_text: c("#0a84ff"),
                ok: c("#32d74b"),
                warn: c("#ff9f0a"),
                danger: c("#ff453a"),
                bolt: c("#ff9f0a"),
                control: white(0.10),
                boxed: white(0.05),
                card: white(0.04),
                card_border: white(0.10),
                code_bg: black(0.18),
                row_selected: c("#0a84ff"),
                row_hover: white(0.06),
                sheet: c("#2a2a2c"),
                panel: c("#262628"),
                field: black(0.25),
                field_border: white(0.14),
                focus: c("#0a84ff").alpha(0.35),
                warn_tint: c("#ff9f0a").alpha(0.09),
                warn_border: c("#ff9f0a").alpha(0.30),
                fs: 13.0,
                fs_small: 11.0,
                fs_mono: 12.0,
                fs_mono_small: 11.5,
                fs_msg: 13.5,
                w_bold: 600.0,
                btn_h: 24.0,
                btn_radius: 6.0,
                toolbar_h: 52.0,
                sidebar_w: 260.0,
                column: 700.0,
                row_radius: 6.0,
                box_radius: 8.0,
                card_radius: 10.0,
            },
            Look::Gnome => Theme {
                look,
                window: c("#1d1d20"),
                sidebar_top: c("#2e2e32"),
                sidebar_bottom: c("#2e2e32"),
                toolbar: c("#1d1d20"),
                content: c("#1d1d20"),
                text: Color::WHITE,
                text2: white(0.55),
                text3: white(0.40),
                sep: black(0.30),
                accent: c("#3584e4"),
                accent_text: c("#78aeed"),
                ok: c("#57e389"),
                warn: c("#cd9309"),
                danger: c("#ff7b63"),
                bolt: c("#f8e45c"),
                control: white(0.10),
                boxed: white(0.08),
                card: white(0.08),
                card_border: black(0.10),
                code_bg: black(0.15),
                row_selected: white(0.10),
                row_hover: white(0.05),
                sheet: c("#242428"),
                panel: c("#222226"),
                field: white(0.08),
                field_border: c("#3584e4"),
                focus: c("#3584e4").alpha(0.5),
                warn_tint: white(0.08),
                warn_border: c("#cd9309").alpha(0.6),
                fs: 14.5,
                fs_small: 12.5,
                fs_mono: 13.0,
                fs_mono_small: 12.5,
                fs_msg: 14.5,
                w_bold: 700.0,
                btn_h: 34.0,
                btn_radius: 7.0,
                toolbar_h: 47.0,
                sidebar_w: 290.0,
                column: 720.0,
                row_radius: 7.0,
                box_radius: 12.0,
                card_radius: 12.0,
            },
        }
    }

    pub fn mac(&self) -> bool {
        self.look.mac()
    }

    /// rune-kit's tokens recoloured for this look, for the kit pieces the app still uses
    /// (Markdown, text fields).
    pub fn kit(&self) -> Kit {
        let mut k = Kit::neutral(true);
        k.c.text_base = self.text;
        k.c.text_strong = Color::WHITE;
        k.c.text_muted = self.text2;
        k.c.text_faint = self.text3;
        k.c.text_accent = self.accent_text;
        k.c.border_focus = self.accent;
        k.c.bg_base = self.content;
        k.ty.body = TextStyle::new(self.fs_msg, 400.0).lh((self.fs_msg * 1.6).round());
        k.ty.ui = TextStyle::new(self.fs, 400.0).lh((self.fs * 1.45).round());
        k.ty.ui_compact = TextStyle::new(self.fs, 400.0).lh((self.fs * 1.3).round());
        k.ty.mono = TextStyle::new(self.fs_mono, 400.0).mono();
        k
    }

    // ---- type -----------------------------------------------------------------------------

    pub fn font(&self) -> TextStyle {
        TextStyle::new(self.fs, 400.0).lh((self.fs * 1.3).round())
    }
    pub fn font_small(&self) -> TextStyle {
        TextStyle::new(self.fs_small, 400.0).lh((self.fs_small * 1.35).round())
    }
    pub fn font_mono(&self) -> TextStyle {
        TextStyle::new(self.fs_mono, 400.0).mono().lh((self.fs_mono * 1.4).round())
    }

    pub fn label(&self, t: impl Into<String>) -> El {
        text(t.into()).font(self.font()).color(self.text)
    }
    pub fn strong(&self, t: impl Into<String>) -> El {
        text(t.into()).font(TextStyle { weight: self.w_bold, ..self.font() }).color(self.text)
    }
    pub fn small(&self, t: impl Into<String>) -> El {
        text(t.into()).font(self.font_small()).color(self.text2)
    }
    pub fn faint(&self, t: impl Into<String>) -> El {
        text(t.into()).font(self.font_small()).color(self.text3)
    }
    pub fn mono(&self, t: impl Into<String>) -> El {
        text(t.into()).font(self.font_mono()).color(self.text).selectable(true)
    }
    pub fn mono_small(&self, t: impl Into<String>) -> El {
        text(t.into()).font(TextStyle::new(self.fs_mono_small, 400.0).mono().lh((self.fs_mono_small * 1.55).round())).color(self.text2).selectable(true)
    }
    /// A group's heading over a box ("Workspace", "Network").
    pub fn group_title(&self, t: impl Into<String>) -> El {
        if self.mac() {
            text(t.into()).font(TextStyle::new(12.0, 600.0)).color(self.text2)
        } else {
            text(t.into()).font(TextStyle::new(15.0, 700.0)).color(self.text)
        }
    }
    /// A section's heading in Settings ("Agent logins").
    pub fn section_title(&self, t: impl Into<String>) -> El {
        if self.mac() {
            text(t.into()).font(TextStyle::new(13.0, 600.0)).color(self.text)
        } else {
            text(t.into()).font(TextStyle::new(15.0, 700.0)).color(self.text)
        }
    }

    // ---- controls -------------------------------------------------------------------------

    /// A push button: `kind` picks the fill, `hint` shows its shortcut inside it.
    pub fn button(&self, t: &str, kind: Btn, hint: Option<&str>, f: impl Fn(&mut App) + 'static) -> El {
        self.button_el(t, kind, hint).on_press(f)
    }
    pub fn button_tx(&self, t: &str, kind: Btn, hint: Option<&str>, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
        self.button_el(t, kind, hint).on_press_tx(f)
    }
    fn button_el(&self, t: &str, kind: Btn, hint: Option<&str>) -> El {
        let (fill, ink, hint_ink) = match kind {
            Btn::Default => (self.control, self.text, self.text3),
            Btn::Primary => (self.accent, Color::WHITE, white(0.72)),
            Btn::Destructive if self.mac() => (self.control, self.danger, self.text3),
            Btn::Destructive => (c("#c01c28").alpha(0.35), c("#ff7b63"), self.text2),
            Btn::Flat => (Color::TRANSPARENT, self.accent_text, self.text3),
        };
        let weight = if self.mac() { 400.0 } else { 700.0 };
        let size = if self.mac() { 13.0 } else { 14.0 };
        let mut b = row()
            .h(self.btn_h)
            .px(if self.mac() { 12.0 } else { 16.0 })
            .gap(8.0)
            .center_y()
            .radius(self.btn_radius)
            .fill(fill)
            .role("button")
            .label(t.to_string())
            .focusable(true)
            .hover(white(0.07))
            .press(white(0.12))
            .child(text(t.to_string()).font(TextStyle::new(size, weight)).color(ink));
        if self.mac() && kind != Btn::Flat {
            b = b.box_shadow(rune::Shadow { y: 0.5, color: white(if kind == Btn::Primary { 0.3 } else { 0.15 }), inset: true, ..Default::default() });
        }
        if let Some(h) = hint {
            b = b.child(text(h.to_string()).font(TextStyle::new(if self.mac() { 11.0 } else { 12.0 }, 400.0)).color(hint_ink)).shortcut_hint(h.to_string());
        }
        b
    }
    /// A small bezel button in a form row ("Show in Finder").
    pub fn small_button(&self, t: &str, f: impl Fn(&mut App) + 'static) -> El {
        self.small_button_el(t, false).on_press(f)
    }
    pub fn small_button_tx(&self, t: &str, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
        self.small_button_el(t, false).on_press_tx(f)
    }
    /// A small button for something that can't be undone ("Revoke…").
    pub fn small_button_danger(&self, t: &str, f: impl Fn(&mut App) + 'static) -> El {
        self.small_button_el(t, true).on_press(f)
    }
    fn small_button_el(&self, t: &str, danger: bool) -> El {
        if !self.mac() {
            return self.button_el(t, if danger { Btn::Destructive } else { Btn::Default }, None);
        }
        row()
            .h(22.0)
            .px(10.0)
            .center_y()
            .radius(5.0)
            .fill(self.control)
            .box_shadow(rune::Shadow { y: 0.5, color: white(0.15), inset: true, ..Default::default() })
            .role("button")
            .label(t.to_string())
            .focusable(true)
            .hover(white(0.07))
            .press(white(0.12))
            .child(text(t.to_string()).font(TextStyle::new(12.0, 400.0)).color(if danger { self.danger } else { self.text }))
    }
    /// A borderless icon button (toolbar, composer).
    pub fn icon_button(&self, i: &'static Icon, size: f32, label: &str, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
        let (w, h) = if self.mac() { (30.0, 28.0) } else { (34.0, 34.0) };
        row()
            .size(w, h)
            .center()
            .radius(if self.mac() { 6.0 } else { 7.0 })
            .role("button")
            .label(label.to_string())
            .help(label.to_string())
            .focusable(true)
            .hover(white(0.08))
            .press(white(0.14))
            .on_press_tx(f)
            .child(icon(i, size).color(if self.mac() { self.text2 } else { self.text }))
    }

    /// An on/off switch.
    pub fn switch(&self, on: bool, label: &str, f: impl Fn(&mut App) + 'static) -> El {
        let (w, h) = if self.mac() { (32.0, 18.0) } else { (40.0, 22.0) };
        let knob = h - 4.0;
        let track = if on { self.accent } else { white(if self.mac() { 0.12 } else { 0.18 }) };
        row()
            .size(w, h)
            .radius(h / 2.0)
            .fill(track)
            .px(2.0)
            .center_y()
            .role("switch")
            .label(label.to_string())
            .checked(on)
            .focusable(true)
            .on_press(f)
            .child(el().w(if on { w - knob - 4.0 } else { 0.0 }))
            .child(el().size(knob, knob).radius(knob / 2.0).fill(c("#e8e8e8")).shadow(1.0, 2.0, black(0.3)))
    }
    /// A checkbox.
    pub fn checkbox(&self, on: bool, label: &str, f: impl Fn(&mut App) + 'static) -> El {
        let s = if self.mac() { 14.0 } else { 18.0 };
        let mut b = row()
            .size(s, s)
            .center()
            .radius(if self.mac() { 3.5 } else { 5.0 })
            .role("checkbox")
            .label(label.to_string())
            .checked(on)
            .focusable(true)
            .on_press(f);
        b = if on { b.fill(self.accent).child(icon(&crate::icons::CHECK, s - 4.0).color(Color::WHITE)) } else { b.border(1.0, white(0.3)) };
        b
    }
    /// A radio button's circle.
    pub fn radio(&self, on: bool, enabled: bool) -> El {
        let s = if self.mac() { 14.0 } else { 16.0 };
        if on {
            row().size(s, s).radius(s / 2.0).fill(self.accent).center().child(el().size(6.0, 6.0).radius(3.0).fill(Color::WHITE))
        } else {
            el().size(s, s).radius(s / 2.0).border(1.0, white(if enabled { 0.3 } else { 0.15 }))
        }
    }
    /// A grouped box: rows with hairlines between them.
    pub fn group(&self, rows: Vec<El>, inset: f32) -> El {
        let mut b = col().w_full().radius(self.box_radius).fill(self.boxed).clip_rounded(self.box_radius);
        if self.mac() {
            b = b.border(0.5, self.sep);
        }
        let n = rows.len();
        for (i, r) in rows.into_iter().enumerate() {
            b = b.child(r);
            if i + 1 < n {
                b = b.child(self.hairline(inset));
            }
        }
        b
    }
    pub fn hairline(&self, inset: f32) -> El {
        let line = if self.mac() { self.sep } else { black(0.25) };
        row().w_full().pl(inset).child(el().h(if self.mac() { 0.5 } else { 1.0 }).grow(1.0).fill(line))
    }
    /// A coloured status dot.
    pub fn dot(&self, color: Color, size: f32) -> El {
        el().size(size, size).radius(size / 2.0).fill(color)
    }
    /// "● Ready" style status.
    pub fn status(&self, color: Color, t: &str, tinted: bool) -> El {
        row()
            .gap(5.0)
            .center_y()
            .child(self.dot(color, 7.0))
            .child(text(t.to_string()).font(TextStyle::new(if self.mac() { 12.0 } else { 13.0 }, 400.0)).color(if tinted { color } else { self.text2 }))
    }
    /// The "+ −" bar under an AppKit list.
    pub fn plus_minus(&self, add_label: &str, add: impl Fn(&mut App, &mut Tx) + 'static, remove: Option<Box<dyn Fn(&mut App) + 'static>>, note: &str) -> El {
        let cell = |i: &'static Icon, label: &str| {
            row()
                .size(28.0, 26.0)
                .center()
                .border_sides([0.0, 0.5, 0.0, 0.0], self.sep)
                .role("button")
                .label(label.to_string())
                .focusable(true)
                .hover(white(0.06))
                .child(icon(i, 10.0).color(self.text))
        };
        let mut r = row().w_full().h(26.0).center_y().fill(white(0.03)).border_sides([0.5, 0.0, 0.0, 0.0], self.sep).child(cell(&crate::icons::PLUS_SMALL, add_label).on_press_tx(add));
        let minus = cell(&crate::icons::MINUS_SMALL, "Remove");
        r = match remove {
            Some(f) => r.child(minus.on_press(move |s: &mut App| f(s))),
            None => r.child(minus.opacity(0.4).disabled(true)),
        };
        r.child(el().grow(1.0)).child(text(note.to_string()).font(TextStyle::new(11.0, 400.0)).color(self.text3).pr(10.0))
    }
    /// A square letter badge for an agent or connector.
    pub fn badge_letter(&self, letter: char, bg: Color, fg: Color, size: f32) -> El {
        row()
            .size(size, size)
            .center()
            .radius((size * 0.24).round())
            .fill(bg)
            .child(text(letter.to_string()).font(TextStyle::new((size * 0.5).round(), 700.0)).color(fg))
    }
    /// A text field, bezelled per look.
    pub fn field(&self, edit: &rune::TextEdit, placeholder: &str, access: fn(&mut App) -> &mut rune::TextEdit) -> El {
        self.field_with(edit, placeholder, access, None)
    }
    /// A text field whose Return does `submit`.
    pub fn field_submit(&self, edit: &rune::TextEdit, placeholder: &str, access: fn(&mut App) -> &mut rune::TextEdit, submit: impl Fn(&mut App, &mut Tx) + 'static) -> El {
        self.field_with(edit, placeholder, access, Some(Box::new(submit)))
    }
    #[allow(clippy::type_complexity)]
    fn field_with(&self, edit: &rune::TextEdit, placeholder: &str, access: fn(&mut App) -> &mut rune::TextEdit, submit: Option<Box<dyn Fn(&mut App, &mut Tx)>>) -> El {
        let style = TextStyle::new(self.fs, 400.0).lh((self.fs * 1.3).round());
        let input = rune::text_input(edit, style)
            .single_line()
            .label(placeholder.to_string())
            .grow(1.0)
            .min_w(0.0)
            .color(self.text)
            .placeholder(placeholder.to_string(), self.text3)
            .caret(self.text, self.accent.alpha(0.35))
            .on_edit(access);
        let input = match submit {
            Some(f) => input.on_submit(move |s: &mut App, tx: &mut Tx| f(s, tx)),
            None => input,
        };
        let h = if self.mac() { 22.0 } else { 34.0 };
        let mut b = row().h(h).w_full().px(if self.mac() { 7.0 } else { 10.0 }).center_y().radius(if self.mac() { 5.0 } else { 7.0 }).fill(self.field).child(input);
        if self.mac() {
            b = b.border(0.5, self.field_border);
        }
        if edit.focused {
            b = b.box_shadow(rune::Shadow { spread: 3.0, color: self.focus, ..Default::default() });
        }
        b
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Btn {
    Default,
    Primary,
    Destructive,
    Flat,
}

/// An agent's badge: its mark on its colour (Claude orange, Codex white, …), or its letter.
pub fn agent_badge(t: &Theme, agent: agenthost_proto::AgentKind, size: f32, enabled: bool) -> El {
    use agenthost_proto::AgentKind as A;
    let name = agent.display_name();
    let (bg, fg) = match agent {
        A::Claude => (c("#d97757"), Color::WHITE),
        A::Codex => (c("#f2f2f2"), c("#111111")),
        A::Gemini => (c("#3d5afe"), Color::WHITE),
        A::Opencode => (c("#3a3a3c"), Color::WHITE),
        A::Cursor => (c("#111111"), Color::WHITE),
        A::Fake => (c("#5e5ce6"), Color::WHITE),
    };
    let b = match crate::icons::agent_logo(agent) {
        Some(logo) => row().size(size, size).center().radius((size * 0.24).round()).fill(bg).child(icon(logo, (size * 0.62).round()).color(fg)),
        None => t.badge_letter(name.chars().next().unwrap_or('?').to_ascii_uppercase(), bg, fg, size),
    };
    if enabled {
        b
    } else {
        b.opacity(0.4)
    }
}
