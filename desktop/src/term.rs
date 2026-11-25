//! One terminal's screen: `alacritty_terminal` interprets the shell's output (the host
//! owns the shell itself), and this turns its cells into [`rune::GridRow`]s, keys into the
//! bytes a terminal sends, and a drag into a selection.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as Ansi, CursorShape, NamedColor, Processor, Rgb};
use rune::{Color, GridRow, Key, KeyEvent, Span, TextStyle};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// What the terminal reports while interpreting output; read after each feed.
#[derive(Clone, Default)]
struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

struct Size {
    cols: u16,
    rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// The terminal's colours: the 16 ANSI ones and the defaults, as sRGB bytes.
#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

const fn rgb(v: u32) -> Rgb {
    Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
}

impl Palette {
    /// A dark palette (the app is dark in both looks), over the pane's own background.
    pub fn dark(bg: Rgb) -> Palette {
        Palette {
            fg: rgb(0xd8d8d8),
            bg,
            cursor: rgb(0xd8d8d8),
            selection: rgb(0x34507a),
            ansi: [
                rgb(0x3b3b3f),
                rgb(0xf0635a),
                rgb(0x5fc86a),
                rgb(0xe5b454),
                rgb(0x5aa2f7),
                rgb(0xc580e8),
                rgb(0x4fc4cf),
                rgb(0xc8c8c8),
                rgb(0x6e6e73),
                rgb(0xff8a80),
                rgb(0x83e08c),
                rgb(0xf5cf7a),
                rgb(0x82b9ff),
                rgb(0xdba6f5),
                rgb(0x7fdde6),
                rgb(0xffffff),
            ],
        }
    }

    /// One of xterm's 256 colours.
    fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) }
            }
            _ => {
                let v = 8 + 10 * (i - 232);
                Rgb { r: v, g: v, b: v }
            }
        }
    }

    fn named(&self, n: NamedColor) -> Rgb {
        let dim = |c: Rgb| mix(c, self.bg, 0.4);
        match n {
            NamedColor::Foreground | NamedColor::BrightForeground => self.fg,
            NamedColor::Background => self.bg,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => dim(self.fg),
            NamedColor::DimBlack => dim(self.ansi[0]),
            NamedColor::DimRed => dim(self.ansi[1]),
            NamedColor::DimGreen => dim(self.ansi[2]),
            NamedColor::DimYellow => dim(self.ansi[3]),
            NamedColor::DimBlue => dim(self.ansi[4]),
            NamedColor::DimMagenta => dim(self.ansi[5]),
            NamedColor::DimCyan => dim(self.ansi[6]),
            NamedColor::DimWhite => dim(self.ansi[7]),
            n => self.ansi[(n as usize).min(15)],
        }
    }

    /// A cell's colour: what the program set with OSC 4/10/11 wins over the palette.
    fn resolve(&self, c: Ansi, set: &Colors) -> Rgb {
        match c {
            Ansi::Spec(rgb) => rgb,
            Ansi::Named(n) => set[n].unwrap_or_else(|| self.named(n)),
            Ansi::Indexed(i) => set[i as usize].unwrap_or_else(|| self.indexed(i)),
        }
    }
}

/// `a` moved `t` of the way to `b`.
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b) }
}

/// sRGB bytes as one of Rune's (linear) colours.
pub fn color(c: Rgb) -> Color {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Color { r: lin(c.r), g: lin(c.g), b: lin(c.b), a: 1.0 }
}

/// A cell as it will draw.
#[derive(Clone, PartialEq, Hash)]
struct Drawn {
    text: String,
    /// Columns it takes: 1, 2 for a wide character, 0 for the second half of one.
    width: u8,
    fg: (u8, u8, u8),
    /// `None` is the pane's own background.
    bg: Option<(u8, u8, u8)>,
    bold: bool,
    italic: bool,
    underline: bool,
}

fn tuple(c: Rgb) -> (u8, u8, u8) {
    (c.r, c.g, c.b)
}

/// Whether every monospace font is sure to draw `c` one column wide: anything else is
/// placed on its own cell, so a fallback font's width can't push the rest of the row.
fn plain(c: char) -> bool {
    (' '..='~').contains(&c) || ('\u{a1}'..'\u{250}').contains(&c)
}

/// A terminal emulator for one of the host's terminals.
pub struct Emu {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    cols: u16,
    rows: u16,
    /// The title the shell set (OSC 0/2), if any.
    pub title: Option<String>,
    /// Counts everything that can change what the view shows.
    version: u64,
    cache: RefCell<Cache>,
}

/// The rows last handed to the grid.
#[derive(Default)]
struct Cache {
    /// The version, palette and font `all` was made for: while they hold, nothing is remade.
    made_for: Option<(u64, u64)>,
    all: Vec<Rc<GridRow>>,
    /// Each row's shaped line and what it was made from, so only changed rows are remade.
    rows: Vec<(u64, Rc<GridRow>)>,
}

impl Emu {
    pub fn new(cols: u16, rows: u16) -> Emu {
        let events = Listener::default();
        let (cols, rows) = (cols.max(2), rows.max(1));
        let term = Term::new(Config::default(), &Size { cols, rows }, events.clone());
        Emu { term, parser: Processor::new(), events, cols, rows, title: None, version: 0, cache: RefCell::default() }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if (cols, rows) != (self.cols, self.rows) {
            (self.cols, self.rows) = (cols, rows);
            self.term.resize(Size { cols, rows });
            self.version += 1;
        }
    }

    /// Interpret output from the shell. Returns what the terminal answers (to "where is
    /// the cursor?" and the like), to send back as input; nothing for a `replay`, whose
    /// questions were answered when they were first asked.
    pub fn feed(&mut self, bytes: &[u8], replay: bool, palette: &Palette) -> Vec<u8> {
        self.parser.advance(&mut self.term, bytes);
        self.version += 1;
        let events: Vec<Event> = std::mem::take(&mut *self.events.0.borrow_mut());
        let mut answer = vec![];
        for ev in events {
            match ev {
                Event::Title(t) => self.title = Some(t).filter(|t| !t.trim().is_empty()),
                Event::ResetTitle => self.title = None,
                Event::PtyWrite(s) if !replay => answer.extend_from_slice(s.as_bytes()),
                Event::ColorRequest(i, f) if !replay => {
                    let c = self.term.colors()[i].unwrap_or_else(|| match i {
                        0..=255 => palette.indexed(i as u8),
                        256 => palette.fg,
                        257 => palette.bg,
                        _ => palette.cursor,
                    });
                    answer.extend_from_slice(f(c).as_bytes());
                }
                Event::TextAreaSizeRequest(f) if !replay => {
                    let size = WindowSize { num_lines: self.rows, num_cols: self.cols, cell_width: 8, cell_height: 16 };
                    answer.extend_from_slice(f(size).as_bytes());
                }
                // The clipboard is not a program's to read or set from here; no bell.
                _ => {}
            }
        }
        answer
    }

    fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// A full-screen program (an editor, a pager) has the screen.
    pub fn alt_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    /// How many lines up the view is scrolled into the scrollback.
    pub fn scrolled(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Scroll the view through the scrollback: positive is up, into the past.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.version += 1;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.version += 1;
    }

    fn point(&self, col: u16, row: u16) -> Point {
        let line = row.min(self.rows - 1) as i32 - self.scrolled() as i32;
        Point::new(Line(line), Column(col.min(self.cols - 1) as usize))
    }

    /// Start selecting at a cell of the view: by character, or by word or line for a
    /// double or triple click. `right` when the pointer is in the cell's right half.
    pub fn select_start(&mut self, col: u16, row: u16, right: bool, clicks: u32) {
        let ty = match clicks {
            0 | 1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        let side = if right { Side::Right } else { Side::Left };
        self.term.selection = Some(Selection::new(ty, self.point(col, row), side));
        self.version += 1;
    }

    pub fn select_to(&mut self, col: u16, row: u16, right: bool) {
        let p = self.point(col, row);
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(p, if right { Side::Right } else { Side::Left });
        }
        self.version += 1;
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
        self.version += 1;
    }

    /// The selected text, if anything is selected.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// What pasting `text` sends: wrapped so the program knows it is a paste when it
    /// asked to be told, else with line ends as the Return key sends them.
    pub fn paste(&self, text: &str) -> Vec<u8> {
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // The end marker inside a paste would end it early and run the rest.
            let clean = text.replace('\x1b', "");
            format!("\x1b[200~{clean}\x1b[201~").into_bytes()
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
        }
    }

    /// The bytes a key sends, or `None` for a key that isn't the terminal's.
    pub fn key(&self, ev: &KeyEvent, base: Option<char>, mac: bool) -> Option<Vec<u8>> {
        encode_key(ev, base, self.mode(), mac)
    }

    /// What the wheel sends to a full-screen program: arrow keys, as terminals do for
    /// pagers and editors that don't read the mouse.
    pub fn wheel_keys(&self, lines: i32) -> Vec<u8> {
        let app = self.mode().contains(TermMode::APP_CURSOR);
        let key: &[u8] = match (lines > 0, app) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        key.repeat(lines.unsigned_abs() as usize)
    }

    /// The cursor's cell in the view, unless the program hid it or it is scrolled away.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let content = self.term.renderable_content();
        if content.cursor.shape == CursorShape::Hidden {
            return None;
        }
        let row = content.cursor.point.line.0 + content.display_offset as i32;
        (0..self.rows as i32).contains(&row).then_some((content.cursor.point.column.0 as u16, row as u16))
    }

    /// The view's cells, row by row, as they will draw.
    fn drawn(&self, palette: &Palette) -> Vec<Vec<Drawn>> {
        let content = self.term.renderable_content();
        let cursor = self.cursor().filter(|_| content.mode.contains(TermMode::SHOW_CURSOR));
        let block = matches!(content.cursor.shape, CursorShape::Block);
        let mut rows: Vec<Vec<Drawn>> = (0..self.rows).map(|_| Vec::with_capacity(self.cols as usize)).collect();
        for cell in content.display_iter {
            let row = cell.point.line.0 + content.display_offset as i32;
            if row < 0 || row >= self.rows as i32 {
                continue;
            }
            let col = cell.point.column.0 as u16;
            let flags = cell.flags;
            let (mut fg, mut bg) = (palette.resolve(cell.fg, content.colors), palette.resolve(cell.bg, content.colors));
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::DIM) {
                fg = mix(fg, bg, 0.4);
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if content.selection.is_some_and(|s| s.contains(cell.point)) {
                bg = palette.selection;
            }
            if cursor == Some((col, row as u16)) {
                // A block takes the cell; a bar or underline is drawn as a tint.
                (fg, bg) = if block { (palette.bg, palette.cursor) } else { (fg, mix(bg, palette.cursor, 0.45)) };
            }
            let spacer = flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            let mut text = String::new();
            if !spacer && cell.c != ' ' && cell.c != '\0' {
                text.push(cell.c);
                for z in cell.zerowidth().unwrap_or_default() {
                    text.push(*z);
                }
            }
            rows[row as usize].push(Drawn {
                text,
                width: if flags.contains(Flags::WIDE_CHAR_SPACER) { 0 } else if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 },
                fg: tuple(fg),
                bg: (bg != palette.bg).then_some(tuple(bg)),
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
            });
        }
        rows
    }

    /// The view as rows for [`rune::grid`]. Nothing is remade while nothing changed, and a
    /// row that draws as it did last time is the same `Rc`, which keeps its shaped text.
    pub fn grid_rows(&self, palette: &Palette, style: TextStyle) -> Vec<Rc<GridRow>> {
        let mut cache = self.cache.borrow_mut();
        let look = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            style.hash(&mut h);
            for c in [palette.fg, palette.bg, palette.cursor, palette.selection].iter().chain(&palette.ansi) {
                (c.r, c.g, c.b).hash(&mut h);
            }
            h.finish()
        };
        if cache.made_for == Some((self.version, look)) {
            return cache.all.clone();
        }
        let drawn = self.drawn(palette);
        cache.rows.truncate(drawn.len());
        let mut out = Vec::with_capacity(drawn.len());
        for (i, cells) in drawn.iter().enumerate() {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cells.hash(&mut h);
            look.hash(&mut h);
            let key = h.finish();
            match cache.rows.get(i) {
                Some((k, row)) if *k == key => out.push(row.clone()),
                _ => {
                    let row = Rc::new(build_row(cells, style));
                    if i < cache.rows.len() {
                        cache.rows[i] = (key, row.clone());
                    } else {
                        cache.rows.push((key, row.clone()));
                    }
                    out.push(row);
                }
            }
        }
        cache.made_for = Some((self.version, look));
        cache.all = out.clone();
        out
    }

    /// The view's text, a line per row, without trailing blanks (for tests and reading).
    pub fn screen_text(&self) -> String {
        let palette = Palette::dark(rgb(0));
        let lines: Vec<String> = self.drawn(&palette).iter().map(|r| r.iter().map(|c| if c.text.is_empty() && c.width > 0 { " " } else { c.text.as_str() }).collect::<String>().trim_end().to_string()).collect();
        lines.join("\n").trim_end().to_string()
    }
}

/// One row of cells as a [`GridRow`]: the plain characters as one styled line, the others
/// each on their own cell, and the backgrounds as fills.
fn build_row(cells: &[Drawn], style: TextStyle) -> GridRow {
    // Blank cells past the last thing to draw need no text.
    let end = cells.iter().rposition(|c| !c.text.is_empty()).map_or(0, |i| i + 1);
    let mut line = String::new();
    let mut spans: Vec<Span> = vec![];
    let mut pinned: Vec<(u16, u16, &Drawn)> = vec![];
    // The run of text being built: where it starts and how it looks.
    let mut run: Option<(usize, &Drawn)> = None;
    let same = |a: &Drawn, b: &Drawn| a.fg == b.fg && a.bold == b.bold && a.italic == b.italic && a.underline == b.underline;
    let close = |spans: &mut Vec<Span>, run: Option<(usize, &Drawn)>, end: usize| {
        if let Some((start, c)) = run.filter(|(start, _)| *start < end) {
            let fg = color(Rgb { r: c.fg.0, g: c.fg.1, b: c.fg.2 });
            let mut s = Span::new(start, end).color(fg);
            if c.bold {
                s = s.weight(700.0);
            }
            if c.italic {
                s = s.italic();
            }
            if c.underline {
                s = s.underline(fg);
            }
            spans.push(s);
        }
    };
    for (col, c) in cells.iter().enumerate().take(end) {
        let inline = c.width == 1 && c.text.chars().next().is_some_and(plain);
        if !inline {
            if c.width > 0 && !c.text.is_empty() {
                pinned.push((col as u16, c.width as u16, c));
            }
            // A space holds the column in the line; it needs no style of its own.
            line.push(' ');
            continue;
        }
        if !run.is_some_and(|(_, r)| same(r, c)) {
            close(&mut spans, run, line.len());
            run = Some((line.len(), c));
        }
        line.push_str(&c.text);
    }
    close(&mut spans, run, line.len());
    let mut row = GridRow::new(line, spans, style);
    for (col, width, c) in pinned {
        let mut st = style;
        if c.bold {
            st.weight = 700.0;
        }
        st.italic = c.italic;
        row = row.cell(col, width, c.text.clone(), st, color(Rgb { r: c.fg.0, g: c.fg.1, b: c.fg.2 }));
    }
    // Backgrounds: one fill per run of the same colour.
    let mut col = 0;
    while col < cells.len() {
        let Some(bg) = cells[col].bg else {
            col += 1;
            continue;
        };
        let start = col;
        while col < cells.len() && cells[col].bg == Some(bg) {
            col += 1;
        }
        row = row.fill(start as u16, col as u16, color(Rgb { r: bg.0, g: bg.1, b: bg.2 }));
    }
    row
}

/// xterm's modifier parameter: 1 + Shift (1) + Alt (2) + Ctrl (4); `None` with none held.
fn modifier(ev: &KeyEvent) -> Option<u8> {
    let m = ev.mods.shift as u8 + 2 * ev.mods.alt as u8 + 4 * ev.mods.ctrl as u8;
    (m > 0).then_some(m + 1)
}

/// The control character Ctrl makes of `c` (Ctrl+C is 3, Ctrl+[ is Escape).
fn control(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '-' | '/' | '7' => 31,
        '?' | '8' => 127,
        _ => return None,
    })
}

/// The bytes a key sends to the program in a terminal (xterm's encoding), or `None` for
/// a key that isn't the terminal's: a ⌘ chord on macOS, a Ctrl+Shift chord elsewhere.
pub fn encode_key(ev: &KeyEvent, base: Option<char>, mode: TermMode, mac: bool) -> Option<Vec<u8>> {
    let mods = ev.mods;
    // ⌘ on macOS is the app's (and off macOS `cmd` is only Ctrl's other name).
    if mac && (mods.cmd || mods.logo) {
        return None;
    }
    let alt = mods.alt;
    let esc = |mut v: Vec<u8>| {
        if alt {
            v.insert(0, 0x1b);
        }
        v
    };
    // CSI or SS3 keys: arrows, Home and End take a letter; the others a number and "~".
    let letter = |l: char| match modifier(ev) {
        Some(m) => format!("\x1b[1;{m}{l}").into_bytes(),
        None if mode.contains(TermMode::APP_CURSOR) => format!("\x1bO{l}").into_bytes(),
        None => format!("\x1b[{l}").into_bytes(),
    };
    let tilde = |n: u8| match modifier(ev) {
        Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
        None => format!("\x1b[{n}~").into_bytes(),
    };
    Some(match &ev.key {
        Key::Text(t) if mods.ctrl => {
            // Ctrl with a character (macOS reports it as text): the control character.
            let c = base.or_else(|| t.chars().next())?;
            esc(vec![control(c)?])
        }
        // Option on a Mac types characters of its own (å, ∂): they go as typed.
        Key::Text(t) if mac => t.clone().into_bytes(),
        Key::Text(t) => esc(t.clone().into_bytes()),
        Key::Command(c) => {
            // Off macOS this is Ctrl+character. Ctrl+Shift chords are the app's (copy,
            // paste, its own shortcuts): a terminal can't tell them from Ctrl alone.
            if mods.shift {
                return None;
            }
            esc(vec![control(*c)?])
        }
        Key::Enter => esc(vec![b'\r']),
        Key::Backspace => esc(vec![if mods.ctrl { 0x08 } else { 0x7f }]),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![b'\t'],
        Key::Escape => vec![0x1b],
        Key::Up => letter('A'),
        Key::Down => letter('B'),
        Key::Right => letter('C'),
        Key::Left => letter('D'),
        Key::Home => letter('H'),
        Key::End => letter('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::Function(n @ 1..=4) => {
            let l = [b'P', b'Q', b'R', b'S'][*n as usize - 1] as char;
            match modifier(ev) {
                Some(m) => format!("\x1b[1;{m}{l}").into_bytes(),
                None => format!("\x1bO{l}").into_bytes(),
            }
        }
        Key::Function(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][*n as usize - 5]),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::Mods;

    fn pal() -> Palette {
        Palette::dark(rgb(0x1e1e1e))
    }

    fn key(key: Key, mods: Mods) -> KeyEvent {
        KeyEvent { key, mods }
    }

    fn sent(k: Key, mods: Mods, mode: TermMode, mac: bool) -> Option<String> {
        encode_key(&key(k, mods), None, mode, mac).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn output_lands_on_the_screen() {
        let mut e = Emu::new(40, 5);
        let answer = e.feed(b"\x1b[1;31mred\x1b[0m plain \xe6\x97\xa5\xe6\x9c\xac\r\nsecond \x1b[42mline\x1b[0m\r\n$ ", false, &pal());
        assert!(answer.is_empty());
        assert_eq!(e.screen_text(), "red plain 日本\nsecond line\n$");
        assert_eq!(e.cursor(), Some((2, 2)));
        let rows = e.drawn(&pal());
        assert!(rows[0][0].bold && rows[0][0].fg == tuple(pal().ansi[1]), "bold red");
        assert_eq!(rows[0][10].width, 2, "a wide character takes two columns");
        assert_eq!(rows[0][11].width, 0);
        assert_eq!(rows[1][7].bg, Some(tuple(pal().ansi[2])), "a green background");
        assert_eq!(rows[1][0].bg, None);
        // The cursor is a block in the cursor colour.
        assert_eq!(rows[2][2].bg, Some(tuple(pal().cursor)));
    }

    #[test]
    fn unchanged_rows_keep_their_shaped_line() {
        let mut e = Emu::new(20, 4);
        let style = TextStyle::new(13.0, 400.0).mono();
        e.feed(b"one\r\ntwo\r\n", false, &pal());
        let a = e.grid_rows(&pal(), style);
        e.feed(b"three", false, &pal());
        let b = e.grid_rows(&pal(), style);
        assert_eq!(b.len(), 4);
        assert!(Rc::ptr_eq(&a[0], &b[0]) && Rc::ptr_eq(&a[1], &b[1]), "rows that didn't change are the same rows");
        assert!(!Rc::ptr_eq(&a[2], &b[2]), "the row typed on is made again");
    }

    #[test]
    fn queries_are_answered_once_and_never_in_a_replay() {
        let mut e = Emu::new(20, 4);
        // "Where is the cursor?" (DSR 6) after moving to row 2, column 5.
        let ask = b"\x1b[2;5H\x1b[6n";
        assert_eq!(e.feed(ask, true, &pal()), b"", "a replayed question gets no answer");
        assert_eq!(e.feed(ask, false, &pal()), b"\x1b[2;5R");
        // "What is your background colour?" (OSC 11): programs pick light or dark by it.
        let bg = String::from_utf8(e.feed(b"\x1b]11;?\x07", false, &pal())).unwrap();
        assert!(bg.starts_with("\x1b]11;rgb:1e1e/1e1e/1e1e"), "{bg:?}");
    }

    #[test]
    fn the_title_is_the_shells() {
        let mut e = Emu::new(20, 4);
        e.feed(b"\x1b]0;me@host: ~/code\x07", false, &pal());
        assert_eq!(e.title.as_deref(), Some("me@host: ~/code"));
    }

    #[test]
    fn keys_send_what_a_terminal_sends() {
        let none = Mods::default();
        let n = TermMode::NONE;
        assert_eq!(sent(Key::Text("a".into()), none, n, false).as_deref(), Some("a"));
        assert_eq!(sent(Key::Enter, none, n, false).as_deref(), Some("\r"));
        assert_eq!(sent(Key::Tab, none, n, false).as_deref(), Some("\t"));
        assert_eq!(sent(Key::Tab, Mods { shift: true, ..none }, n, false).as_deref(), Some("\x1b[Z"));
        assert_eq!(sent(Key::Backspace, none, n, false).as_deref(), Some("\x7f"));
        assert_eq!(sent(Key::Escape, none, n, false).as_deref(), Some("\x1b"));
        assert_eq!(sent(Key::Up, none, n, false).as_deref(), Some("\x1b[A"));
        assert_eq!(sent(Key::Up, none, TermMode::APP_CURSOR, false).as_deref(), Some("\x1bOA"), "an editor's arrows");
        let ctrl = Mods { ctrl: true, cmd: true, ..none };
        assert_eq!(sent(Key::Right, Mods { ctrl: true, ..none }, n, false).as_deref(), Some("\x1b[1;5C"), "Ctrl+Right moves by word");
        assert_eq!(sent(Key::Delete, none, n, false).as_deref(), Some("\x1b[3~"));
        assert_eq!(sent(Key::PageUp, none, n, false).as_deref(), Some("\x1b[5~"));
        assert_eq!(sent(Key::Function(1), none, n, false).as_deref(), Some("\x1bOP"));
        assert_eq!(sent(Key::Function(5), none, n, false).as_deref(), Some("\x1b[15~"));
        // Ctrl+C off macOS arrives as the primary chord.
        assert_eq!(sent(Key::Command('c'), ctrl, n, false).as_deref(), Some("\x03"));
        assert_eq!(sent(Key::Command('d'), ctrl, n, false).as_deref(), Some("\x04"));
        assert_eq!(sent(Key::Command('c'), Mods { shift: true, ..ctrl }, n, false), None, "Ctrl+Shift+C is Copy, the app's");
        // On a Mac, Ctrl+C is Control and ⌘C is the app's.
        assert_eq!(sent(Key::Text("c".into()), Mods { ctrl: true, ..none }, n, true).as_deref(), Some("\x03"));
        assert_eq!(sent(Key::Command('c'), Mods { cmd: true, logo: true, ..none }, n, true), None);
        // Alt is Meta off macOS; Option types its own characters on a Mac.
        assert_eq!(sent(Key::Text("b".into()), Mods { alt: true, ..none }, n, false).as_deref(), Some("\x1bb"));
        assert_eq!(sent(Key::Text("∫".into()), Mods { alt: true, ..none }, n, true).as_deref(), Some("∫"));
    }

    #[test]
    fn a_paste_is_marked_when_the_program_asks() {
        let mut e = Emu::new(20, 4);
        assert_eq!(e.paste("ls\necho hi\n"), b"ls\recho hi\r");
        e.feed(b"\x1b[?2004h", false, &pal());
        assert_eq!(e.paste("ls\n\x1b[201~rm"), b"\x1b[200~ls\n[201~rm\x1b[201~", "an end marker inside can't end the paste early");
    }

    #[test]
    fn a_drag_selects_and_the_view_scrolls_back() {
        let mut e = Emu::new(10, 3);
        e.feed(b"alpha\r\nbravo\r\ncharlie\r\ndelta", false, &pal());
        assert_eq!(e.screen_text(), "bravo\ncharlie\ndelta");
        e.select_start(0, 0, false, 1);
        e.select_to(2, 1, true);
        assert_eq!(e.selection_text().as_deref(), Some("bravo\ncha"));
        assert!(e.drawn(&pal())[1][1].bg == Some(tuple(pal().selection)));
        e.clear_selection();
        assert_eq!(e.selection_text(), None);
        // A double click takes the word.
        e.select_start(3, 1, false, 2);
        assert_eq!(e.selection_text().as_deref(), Some("charlie"));
        e.scroll(1);
        assert_eq!((e.scrolled(), e.screen_text().as_str()), (1, "alpha\nbravo\ncharlie"));
        assert_eq!(e.cursor(), None, "the cursor is below the view");
        e.scroll_to_bottom();
        assert_eq!(e.scrolled(), 0);
    }

    #[test]
    fn resizing_rewraps() {
        let mut e = Emu::new(20, 3);
        e.feed(b"0123456789ab", false, &pal());
        e.resize(8, 3);
        assert_eq!(e.size(), (8, 3));
        // The cursor's row stays where it was; the line's start wraps up into the scrollback.
        e.scroll(5);
        assert_eq!(e.screen_text(), "01234567\n89ab");
        e.scroll_to_bottom();
        e.resize(20, 3);
        e.scroll(5);
        assert!(e.screen_text().contains("0123456789ab"), "{:?}", e.screen_text());
    }
}
