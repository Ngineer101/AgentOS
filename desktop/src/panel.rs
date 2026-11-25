//! The right panel: a trailing pane beside the main one, with a tab per thing it can show.
//! Today that is the terminal and the open session's side questions; a new feature for the
//! panel is a [`PanelTab`], its entry in [`tabs`] and its body in [`pane`].
//!
//! macOS draws it as an inspector (a hairline-separated trailing pane under its own toolbar
//! band); GNOME as the end pane of an AdwOverlaySplitView.

use crate::icons as ic;
use crate::look::Theme;
use crate::model::{App, PanelTab, Screen};
use crate::terminal::{PANEL_MAX_W, PANEL_MIN_W};
use rune::{col, el, icon, row, text, Color, El, TextStyle, Tx};
use rune_kit::Kit;

impl PanelTab {
    pub fn title(self) -> &'static str {
        match self {
            PanelTab::Terminal => "Terminal",
            PanelTab::Side => "Side Questions",
        }
    }

    fn key(self) -> &'static str {
        match self {
            PanelTab::Terminal => "panel-tab-terminal",
            PanelTab::Side => "panel-tab-side",
        }
    }

    fn glyph(self) -> &'static rune::Icon {
        match self {
            PanelTab::Terminal => &ic::TERMINAL,
            PanelTab::Side => &ic::BUBBLE,
        }
    }
}

/// The tabs the panel offers on this screen: the terminal anywhere, side questions in a
/// session that has some.
pub fn tabs(s: &App) -> Vec<PanelTab> {
    let mut v = vec![PanelTab::Terminal];
    if matches!(&s.screen, Screen::Session(id) if s.has_side(id)) {
        v.push(PanelTab::Side);
    }
    v
}

pub fn pane(s: &App, shown: PanelTab, t: &Theme, k: &Kit) -> El {
    let mac = t.mac();
    // The header band, level with the window's toolbar: the tabs, and a button to hide.
    let mut bar = row().key("panel-tabs").gap(if mac { 2.0 } else { 4.0 }).center_y().role("tablist").focus_group();
    for tab in tabs(s) {
        bar = bar.child(tab_button(t, tab, tab == shown));
    }
    let hide = t.icon_button(&ic::CLOSE, 10.0, &format!("Hide {}", shown.title()), |s: &mut App, _: &mut Tx| s.panel_open = false).key("panel-hide");
    let header = if mac {
        row().key("panel-header").w_full().h(t.toolbar_h).pl(10.0).pr(10.0).center_y().fill(t.toolbar).border_sides([0.0, 0.0, 0.5, 0.0], Color::BLACK.alpha(0.6)).window_drag().child(bar).child(el().grow(1.0)).child(hide)
    } else {
        row().key("panel-header").w_full().h(t.toolbar_h).px(6.0).center_y().window_drag().child(bar).child(el().grow(1.0)).child(hide)
    };

    let body = match (shown, &s.screen) {
        (PanelTab::Side, Screen::Session(id)) => crate::side::content(s, id, t, k),
        (PanelTab::Side, _) => col(),
        (PanelTab::Terminal, _) => crate::terminal::body(s, t),
    };
    let mut pane = col()
        .key("panel")
        .w(s.panel_width(t))
        .h_full()
        .min_h(0.0)
        .role("complementary")
        .label(shown.title())
        .child(header)
        .child(body);
    pane = if mac { pane.fill(t.content).border_sides([0.0, 0.0, 0.0, 0.5], Color::BLACK.alpha(0.6)) } else { pane.fill(t.sidebar_top).border_sides([0.0, 0.0, 0.0, 1.0], Color::BLACK.alpha(0.35)) };
    if shown == PanelTab::Terminal {
        pane = pane.child(grip(s.panel_width(t)));
    }
    pane
}

fn tab_button(t: &Theme, tab: PanelTab, on: bool) -> El {
    let mac = t.mac();
    let r = row()
        .key(tab.key())
        .h(if mac { 26.0 } else { 32.0 })
        .gap(6.0)
        .px(if mac { 9.0 } else { 10.0 })
        .center_y()
        .radius(t.btn_radius)
        .role("tab")
        .label(tab.title())
        .checked(on)
        .focusable(true)
        .on_press_tx(move |s: &mut App, tx: &mut Tx| {
            s.show_panel(tab);
            if tab == PanelTab::Terminal {
                tx.focus("term-view", false);
            }
        })
        .child(icon(tab.glyph(), 12.0).color(if on { t.text } else { t.text2 }))
        .child(text(tab.title()).font(TextStyle::new(t.fs, if on { if mac { 600.0 } else { 700.0 } } else { 400.0 })).color(if on { t.text } else { t.text2 }));
    if on { r.fill(Color::WHITE.alpha(0.10)) } else { r.hover(Color::WHITE.alpha(0.05)) }
}

/// The panel's leading edge: drag it to make the terminal wider or narrower (kept between
/// launches), double-press it for the look's own width.
fn grip(w: f32) -> El {
    const GRIP: f32 = 6.0;
    el().key("panel-grip")
        .role("separator")
        .label("Resize Panel")
        .range_value(f64::from(w), f64::from(PANEL_MIN_W), f64::from(PANEL_MAX_W))
        .range_step(16.0)
        .pin(Some(-GRIP / 2.0), Some(0.0), None, Some(0.0))
        .w(GRIP)
        .cursor(rune::Cursor::ResizeColumn)
        .fill(Color::TRANSPARENT)
        .raise()
        .on_drag(move |s: &mut App, d: &mut rune::Drag, _: &mut Tx| match d.phase {
            rune::Phase::Down if d.clicks == 2 => s.panel_w = None,
            rune::Phase::Down => d.data = w,
            // The edge is the leading one: dragging left widens.
            rune::Phase::Move if d.clicks < 2 => s.resize_panel(d.data - d.dx, false),
            rune::Phase::Up if d.cancelled() && d.clicks < 2 => s.resize_panel(d.data, true),
            rune::Phase::Up if d.clicks < 2 => s.resize_panel(d.data - d.dx, true),
            _ => {}
        })
        .on_step(move |s: &mut App, delta: i32, _: &mut Tx| s.resize_panel(w - delta as f32 * 16.0, true))
}
