//! agenthost desktop app: the full control surface, over the host's Unix socket.

mod audio;
mod automations;
mod brand;
mod bridge;
mod commands;
mod demo;
mod icons;
mod look;
mod model;
mod session;
mod settings;
mod sheet;
mod side;
mod agents;
mod toast;
mod view;
#[cfg(test)]
mod tests;

use model::App;
use rune::{AppSpec, Chrome, KeyEvent, Tx, Waker};

fn setup(s: &mut App, waker: Waker) {
    s.image_cache = Some(rune::ImageCache::new(waker.clone()));
    if std::env::var_os("AGENTHOST_DESKTOP_OFFLINE").is_none() {
        s.bridge = Some(bridge::start(waker));
        s.hostname = hostname();
    }
}

/// This computer's name for the sidebar's host line ("workstation").
fn hostname() -> String {
    let out = std::process::Command::new("hostname").arg("-s").output().or_else(|_| std::process::Command::new("hostname").output());
    out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_default()
}

fn poll(s: &mut App, _clock: f64, tx: &mut Tx) -> bool {
    let changed = s.drain();
    s.fetch_images();
    if s.catch_up() {
        tx.wake_after(std::time::Duration::from_millis(500));
    }
    for url in std::mem::take(&mut s.open_queue) {
        if tx.open_url(&url).is_err() {
            s.notice = Some(format!("Could not open the browser. Open this link yourself: {url}"));
        }
    }
    // Messages become toasts; toasts go when their time is up; the catalog search runs once
    // typing pauses.
    let now = std::time::Instant::now();
    let had = s.toasts.len() + usize::from(s.error.is_some()) + usize::from(s.notice.is_some());
    s.flush_messages(now);
    if let Some(left) = s.tick_toasts(now) {
        tx.wake_after(left);
    }
    let was_searching = s.searching;
    if let Some(left) = s.search_if_due(now) {
        tx.wake_after(left);
    }
    let changed = changed || had != s.toasts.len() || was_searching != s.searching;
    // While recording: the meter and timer move, and a forgotten recording stops.
    // While dictating live, the words so far go to the host as they are spoken.
    if let model::Mic::Recording(r) = &s.mic {
        s.mic_level = s.mic_level * 0.6 + r.level() * 0.4;
        let over = r.seconds() >= audio::MAX_SECONDS;
        s.pump_dictation();
        if over {
            s.mic_press();
        }
        tx.wake_after(std::time::Duration::from_millis(100));
        return true;
    }
    // A running subagent's clock moves every second.
    if let model::Screen::Session(id) = &s.screen {
        if s.running_work(id).0 > 0 {
            tx.wake_after(std::time::Duration::from_secs(1));
            return true;
        }
    }
    // Keep "3m ago" labels fresh.
    tx.wake_after(std::time::Duration::from_secs(30));
    changed
}

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    commands::on_key(s, ev, tx)
}

pub(crate) fn app() -> rune::App<App> {
    let look = look::Look::current();
    // macOS: content under a transparent titlebar, the traffic lights centred in the 52 pt
    // toolbar band. GNOME keeps the window manager's frame.
    let chrome = if look.mac() { Chrome { full_size_content: true, traffic_lights: Some((20.0, 20.0)), drag_regions: true } } else { Chrome::DEFAULT };
    let families = if look.mac() { (".SF NS, SF Pro Text, Inter", "ui-monospace, SF Mono, Menlo") } else { ("Adwaita Sans, Cantarell, Inter", "Adwaita Mono, ui-monospace, monospace") };
    let mut app = rune::App::new(AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: demo::SCENES,
        min_size: Some((760.0, 520.0)),
        families,
        ..AppSpec::new("AgentHost", (1280.0, 800.0), model::init, view::view)
    })
    .chrome(chrome)
    .standard_menus();
    if look.mac() {
        for item in commands::menu_bar() {
            app = app.menu_item(item);
        }
    }
    app
}

fn main() {
    if let Err(e) = rune::set_app_icon(brand::APP_ICON_PNG) {
        eprintln!("app icon: {e}");
    }
    app().run();
}
