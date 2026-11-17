//! agenthost desktop app: the full control surface, over the host's Unix socket.

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
    for url in std::mem::take(&mut s.open_queue) {
        if tx.open_url(&url).is_err() {
            s.notice = Some(format!("Could not open the browser. Open this link yourself: {url}"));
        }
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

fn main() {
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
    .run();
}
