//! Toasts: short messages that go by themselves after ten seconds (the clock stops while the
//! pointer is over them), stack, and keep their button ("Show" for an approval). macOS draws
//! small floating panels under the toolbar's trailing end; GNOME draws AdwToast pills at the
//! bottom centre. System notifications are separate and stay.

use crate::look::MEDIUM;
use crate::icons as ic;
use crate::look::Theme;
use crate::model::{App, Screen, ToastAction, ToastKind};
use rune::{col, icon, row, text, Color, El, TextStyle, Tx};

struct Shown {
    key: String,
    kind: ToastKind,
    text: String,
    action: Option<&'static str>,
    /// A toast's id, or None for a message set this frame (it becomes a toast on the next poll).
    id: Option<u64>,
    error_field: bool,
}

pub fn layer(s: &App, t: &Theme) -> Option<El> {
    let mut list: Vec<Shown> = s
        .toasts
        .iter()
        .map(|x| Shown {
            key: format!("toast-{}", x.id),
            kind: x.kind,
            text: x.text.clone(),
            action: x.action.as_ref().map(|a| match a {
                ToastAction::ShowApproval { .. } | ToastAction::ShowBrowser => "Show",
                ToastAction::OpenSettings(_) if t.mac() => "Settings",
                ToastAction::OpenSettings(_) => "Preferences",
            }),
            id: Some(x.id),
            error_field: false,
        })
        .collect();
    // Set by a handler this frame; the next poll turns them into timed toasts.
    if let Some(e) = &s.error {
        list.push(Shown { key: "toast-error".into(), kind: ToastKind::Error, text: e.clone(), action: None, id: None, error_field: true });
    }
    if let Some(n) = &s.notice {
        list.push(Shown { key: "toast-notice".into(), kind: ToastKind::Info, text: n.clone(), action: None, id: None, error_field: false });
    }
    if list.is_empty() {
        return None;
    }
    let mac = t.mac();
    let mut stack = col()
        .key("toasts")
        .gap(8.0)
        .role("log")
        .label("Notifications")
        .on_hover_change(|s: &mut App, inside: bool, tx: &mut Tx| {
            s.hover_toasts(inside, std::time::Instant::now());
            // Let poll schedule the next expiry.
            tx.wake_after(std::time::Duration::from_millis(30));
        });
    if mac {
        // Newest at the top, under the toolbar.
        for sh in list.iter().rev() {
            stack = stack.child(mac_toast(t, sh));
        }
        let top = t.toolbar_h + 10.0;
        let right = 14.0 + s.trailing_width(t);
        Some(stack.w(340.0).pin(None, Some(top), Some(right), None))
    } else {
        // Newest at the bottom, nearest the eye line of AdwToastOverlay.
        for sh in &list {
            stack = stack.child(gnome_toast(t, sh));
        }
        let over_composer = matches!(&s.screen, Screen::Session(id) if s.session(id).is_some_and(|x| !x.state.is_terminal() && x.archived_at.is_none()));
        let bottom = if over_composer { 132.0 } else { 24.0 };
        // Centred over the content, as AdwToastOverlay sits in the content pane.
        let left = if s.sidebar_open { t.sidebar_w } else { 0.0 };
        let right = s.trailing_width(t);
        Some(row().key("toast-overlay").pin(Some(left), None, Some(right), Some(bottom)).justify_center().child(stack.items_center().max_w(560.0)))
    }
}

fn dismiss(sh: &Shown) -> impl Fn(&mut App) + 'static {
    let (id, error_field) = (sh.id, sh.error_field);
    move |s: &mut App| match id {
        Some(id) => s.dismiss_toast(id),
        None if error_field => s.error = None,
        None => s.notice = None,
    }
}

fn act(sh: &Shown) -> impl Fn(&mut App, &mut Tx) + 'static {
    let id = sh.id;
    move |s: &mut App, tx: &mut Tx| {
        if let Some(id) = id {
            s.toast_action(id, tx);
        }
    }
}

fn mac_toast(t: &Theme, sh: &Shown) -> El {
    let (glyph, color) = match sh.kind {
        ToastKind::Info => (&ic::INFO, t.accent),
        ToastKind::Error => (&ic::ERROR, t.danger),
        ToastKind::Approval => (&ic::WARN, t.warn),
    };
    let mut body = col().grow(1.0).min_w(0.0).gap(8.0).child(text(sh.text.clone()).font(TextStyle::new(12.0, 400.0).lh(16.0)).color(t.text).wrap_anywhere().selectable(true));
    if let Some(a) = sh.action {
        body = body.child(row().gap(6.0).child(t.small_button_tx(a, act(sh)).key(format!("{}-action", sh.key))));
    }
    let close = row()
        .key(format!("{}-close", sh.key))
        .size(16.0, 16.0)
        .center()
        .role("button")
        .label("Dismiss")
        .help("Dismiss")
        .on_press(dismiss(sh))
        .child(icon(&ic::XMARK_CIRCLE, 14.0).color(Color::WHITE.alpha(0.25)));
    row()
        .key(sh.key.clone())
        .w_full()
        .gap(10.0)
        .items_start()
        .pl(12.0)
        .pr(10.0)
        .py(11.0)
        .radius(t.card_radius)
        .fill(rune::color::hex("#323234").unwrap_or(Color::BLACK).alpha(0.97))
        .border(0.5, Color::WHITE.alpha(0.16))
        .shadow(8.0, 28.0, Color::BLACK.alpha(0.45))
        .role(if sh.kind == ToastKind::Error { "alert" } else { "status" })
        .label(sh.text.clone())
        .child(icon(glyph, 16.0).color(color).mt(0.0))
        .child(body)
        .child(close)
}

fn gnome_toast(t: &Theme, sh: &Shown) -> El {
    let mut r = row()
        .key(sh.key.clone())
        .min_h(42.0)
        .max_w(560.0)
        .gap(6.0)
        .center_y()
        .pl(20.0)
        .pr(6.0)
        .py(5.0)
        .radius(t.card_radius)
        .fill(rune::color::hex("#3c3c40").unwrap_or(Color::BLACK))
        .shadow(4.0, 18.0, Color::BLACK.alpha(0.4))
        .role(if sh.kind == ToastKind::Error { "alert" } else { "status" })
        .label(sh.text.clone())
        .child(text(sh.text.clone()).font(TextStyle::new(14.5, 400.0).lh(20.0)).color(if sh.kind == ToastKind::Error { t.danger } else { t.text }).wrap_anywhere().shrink().min_w(0.0).mr(6.0));
    if let Some(a) = sh.action {
        r = r.child(
            row()
                .key(format!("{}-action", sh.key))
                .h(32.0)
                .px(12.0)
                .center_y()
                .radius(t.btn_radius)
                .role("button")
                .label(a)
                .focusable(true)
                .hover(Color::WHITE.alpha(0.08))
                .press(Color::WHITE.alpha(0.14))
                .on_press_tx(act(sh))
                .child(text(a).font(TextStyle::new(14.5, MEDIUM)).color(t.text)),
        );
    }
    r.child(
        row()
            .key(format!("{}-close", sh.key))
            .size(32.0, 32.0)
            .center()
            .radius(t.btn_radius)
            .role("button")
            .label("Dismiss")
            .focusable(true)
            .hover(Color::WHITE.alpha(0.08))
            .on_press(dismiss(sh))
            .child(icon(&ic::CLOSE, 10.0).color(t.text)),
    )
}
