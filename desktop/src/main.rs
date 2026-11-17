//! agenthost desktop app: the full control surface, over the host's Unix socket.

mod audio;
mod brand;
mod bridge;
mod demo;
mod model;
mod view;
#[cfg(test)]
mod tests;

use model::App;
use rune::{AppSpec, Chrome, KeyEvent, Tx, Waker};

fn setup(s: &mut App, waker: Waker) {
    if std::env::var_os("AGENTHOST_DESKTOP_OFFLINE").is_none() {
        s.bridge = Some(bridge::start(waker));
    }
}

fn poll(s: &mut App, _clock: f64, tx: &mut Tx) -> bool {
    let changed = s.drain();
    if s.catch_up() {
        tx.wake_after(std::time::Duration::from_millis(500));
    }
    for url in std::mem::take(&mut s.open_queue) {
        if tx.open_url(&url).is_err() {
            s.notice = Some(format!("Could not open the browser. Open this link yourself: {url}"));
        }
    }
    // While recording: the meter and timer move, and a forgotten recording stops.
    if let model::Mic::Recording(r) = &s.mic {
        s.mic_level = s.mic_level * 0.6 + r.level() * 0.4;
        if r.seconds() >= audio::MAX_SECONDS {
            s.mic_press();
        }
        tx.wake_after(std::time::Duration::from_millis(100));
        return true;
    }
    // Keep "3m ago" labels fresh.
    tx.wake_after(std::time::Duration::from_secs(30));
    changed
}

fn on_key(s: &mut App, ev: &KeyEvent, _: &mut Tx) -> bool {
    if rune_kit::shell::is_sidebar_toggle(ev) {
        s.sidebar.toggle();
        return true;
    }
    false
}

pub(crate) fn app() -> rune::App<App> {
    let chrome = Chrome { full_size_content: true, traffic_lights: Some((16.0, 25.0)), drag_regions: true };
    rune::App::new(AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: demo::SCENES,
        min_size: Some((760.0, 520.0)),
        families: ("Inter", "ui-monospace"),
        ..AppSpec::new("agenthost", (1180.0, 760.0), model::init, view::view)
    })
    .chrome(chrome)
}

fn main() {
    if let Err(e) = rune::set_app_icon(brand::APP_ICON_PNG) {
        eprintln!("app icon: {e}");
    }
    app().run();
}
