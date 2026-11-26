//! Every action with a key: one table for the keys (both looks), the labels the UI shows, and
//! the macOS menu bar.

use crate::bridge::{Refresh, Tag};
use crate::look::Look;
use crate::model::{App, Menu, PanelTab, Screen, Tab};
use agenthost_proto::methods as m;
use rune::{Chord, KeyEvent, MenuItem, Tx};
use serde_json::json;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    NewSession,
    QuickTask,
    Send,
    Attach,
    Dictate,
    Stop,
    Approve,
    Deny,
    NextApproval,
    PrevSession,
    NextSession,
    /// ⌘1…⌘9 (Alt+1…9): the n-th live session in the sidebar.
    Jump(u8),
    Search,
    ToggleSidebar,
    Settings,
    /// Hide the session from the list (the host ends it first if it's live).
    Archive,
    /// Show or hide the side-question pane.
    ToggleSide,
    /// Show or hide the terminal (the right panel's Terminal tab).
    ToggleTerminal,
    /// Show or hide the workspace's browser (the right panel's Browser tab).
    ToggleBrowser,
    /// Show or hide what the open session changed (the right panel's Changes tab).
    ToggleChanges,
    /// Show or hide the open session's files (the right panel's Files tab).
    ToggleFiles,
    ShowFolder,
    /// Rename Session…: the header's title becomes a field.
    RenameSession,
    /// Copy the session's worktree branch name.
    CopyBranch,
    Shortcuts,
    MainMenu,
    PairPhone,
    /// Pair this app with a remote host (Settings → General).
    PairRemoteHost,
    CheckHost,
    Quit,
}

const KEYED: [Cmd; 27] = [
    Cmd::Archive,
    Cmd::NewSession,
    Cmd::QuickTask,
    Cmd::Attach,
    Cmd::Dictate,
    Cmd::Stop,
    Cmd::Approve,
    Cmd::Deny,
    Cmd::NextApproval,
    Cmd::PrevSession,
    Cmd::NextSession,
    Cmd::Search,
    Cmd::ToggleSidebar,
    Cmd::Settings,
    Cmd::ToggleSide,
    Cmd::ToggleTerminal,
    Cmd::ToggleBrowser,
    Cmd::ToggleChanges,
    Cmd::ToggleFiles,
    Cmd::ShowFolder,
    Cmd::Shortcuts,
    Cmd::MainMenu,
    Cmd::Quit,
    Cmd::Send,
    Cmd::PairPhone,
    Cmd::PairRemoteHost,
    Cmd::CheckHost,
];

/// The chord for `cmd` in `look`, as `Chord::parse` reads it.
pub fn chord(look: Look, cmd: Cmd) -> Option<String> {
    let mac = look.mac();
    Some(match cmd {
        Cmd::NewSession => if mac { "super+n" } else { "ctrl+n" }.into(),
        Cmd::QuickTask => if mac { "super+shift+n" } else { "ctrl+shift+n" }.into(),
        Cmd::Attach => if mac { "super+shift+a" } else { "ctrl+shift+a" }.into(),
        Cmd::Dictate => if mac { "super+shift+d" } else { "ctrl+shift+d" }.into(),
        Cmd::Stop => if mac { "super+." } else { "ctrl+." }.into(),
        Cmd::Approve => if mac { "super+alt+a" } else { "alt+a" }.into(),
        Cmd::Deny => if mac { "super+alt+d" } else { "alt+d" }.into(),
        Cmd::NextApproval => if mac { "super+j" } else { "ctrl+j" }.into(),
        Cmd::PrevSession => if mac { "super+alt+up" } else { "alt+up" }.into(),
        Cmd::NextSession => if mac { "super+alt+down" } else { "alt+down" }.into(),
        Cmd::Jump(n) => format!("{}+{n}", if mac { "super" } else { "alt" }),
        Cmd::Search => if mac { "super+f" } else { "ctrl+f" }.into(),
        Cmd::ToggleSidebar => if mac { "ctrl+super+s" } else { "f9" }.into(),
        Cmd::Settings => if mac { "super+," } else { "ctrl+," }.into(),
        // Pages, Keynote and Numbers show their inspector with ⌥⌘I; GNOME pairs it with F9.
        Cmd::ToggleSide => if mac { "super+alt+i" } else { "shift+f9" }.into(),
        // Where editors keep it: Control and the key under Escape, on both.
        Cmd::ToggleTerminal => "ctrl+`".into(),
        Cmd::ToggleBrowser => if mac { "super+shift+b" } else { "ctrl+shift+b" }.into(),
        // Where editors keep source control and the explorer.
        Cmd::ToggleChanges => if mac { "super+shift+g" } else { "ctrl+shift+g" }.into(),
        Cmd::ToggleFiles => if mac { "super+shift+e" } else { "ctrl+shift+e" }.into(),
        // ⌘⌫ would be "delete to the line's start" in the composer; ⌃⌘A is Mail's Archive.
        Cmd::Archive => if mac { "ctrl+super+a" } else { "ctrl+alt+a" }.into(),
        Cmd::ShowFolder if mac => "super+alt+r".into(),
        Cmd::Shortcuts if !mac => "ctrl+shift+/".into(),
        Cmd::MainMenu if !mac => "f10".into(),
        Cmd::Quit if !mac => "ctrl+q".into(),
        _ => return None,
    })
}

/// How the UI writes `cmd`'s keys: "⌥⌘A" on macOS, "Alt+A" on GNOME.
pub fn label(look: Look, cmd: Cmd) -> String {
    let mac = look.mac();
    let s = match cmd {
        Cmd::NewSession => if mac { "⌘N" } else { "Ctrl+N" },
        Cmd::QuickTask => if mac { "⇧⌘N" } else { "Ctrl+Shift+N" },
        Cmd::Send => if mac { "↩" } else { "Enter" },
        Cmd::Attach => if mac { "⇧⌘A" } else { "Ctrl+Shift+A" },
        Cmd::Dictate => if mac { "⇧⌘D" } else { "Ctrl+Shift+D" },
        Cmd::Stop => if mac { "⌘." } else { "Ctrl+." },
        Cmd::Approve => if mac { "⌥⌘A" } else { "Alt+A" },
        Cmd::Deny => if mac { "⌥⌘D" } else { "Alt+D" },
        Cmd::NextApproval => if mac { "⌘J" } else { "Ctrl+J" },
        Cmd::PrevSession => if mac { "⌥⌘↑" } else { "Alt+↑" },
        Cmd::NextSession => if mac { "⌥⌘↓" } else { "Alt+↓" },
        Cmd::Jump(n) => return if mac { format!("⌘{n}") } else { format!("Alt+{n}") },
        Cmd::Search => if mac { "⌘F" } else { "Ctrl+F" },
        Cmd::ToggleSidebar => if mac { "⌃⌘S" } else { "F9" },
        Cmd::Settings => if mac { "⌘," } else { "Ctrl+," },
        Cmd::ToggleSide => if mac { "⌥⌘I" } else { "Shift+F9" },
        Cmd::ToggleTerminal => if mac { "⌃`" } else { "Ctrl+`" },
        Cmd::ToggleBrowser => if mac { "⇧⌘B" } else { "Ctrl+Shift+B" },
        Cmd::ToggleChanges => if mac { "⇧⌘G" } else { "Ctrl+Shift+G" },
        Cmd::ToggleFiles => if mac { "⇧⌘E" } else { "Ctrl+Shift+E" },
        Cmd::Archive => if mac { "⌃⌘A" } else { "Ctrl+Alt+A" },
        Cmd::ShowFolder => if mac { "⌥⌘R" } else { "" },
        Cmd::Shortcuts => if mac { "" } else { "Ctrl+?" },
        Cmd::MainMenu => if mac { "" } else { "F10" },
        Cmd::Quit => if mac { "⌘Q" } else { "Ctrl+Q" },
        Cmd::PairPhone | Cmd::PairRemoteHost | Cmd::CheckHost | Cmd::RenameSession | Cmd::CopyBranch => "",
    };
    s.to_string()
}

pub(crate) fn matches(look: Look, cmd: Cmd, ev: &KeyEvent, tx: &Tx) -> bool {
    let Some(spec) = chord(look, cmd) else { return false };
    let Ok(ch) = Chord::parse(&spec) else { return false };
    if ch.matches_tx(ev, tx) {
        return true;
    }
    // "Ctrl+?" is Ctrl+Shift+/ on a US layout, or the "?" key itself elsewhere.
    cmd == Cmd::Shortcuts && Chord::parse("ctrl+?").is_ok_and(|c| c.matches_tx(ev, tx))
}

/// The window's keys: every shortcut, both looks.
pub fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let look = s.look;
    // ⌘↩ / Ctrl+Enter starts the session from the sheet.
    if s.new_open {
        let start = if look.mac() { "super+enter" } else { "ctrl+enter" };
        if Chord::parse(start).is_ok_and(|c| c.matches_tx(ev, tx)) {
            s.start_from_sheet();
            return true;
        }
    }
    // ⌘↩ / Ctrl+Enter saves the automation being edited.
    if s.auto_open && s.confirm_delete_auto.is_none() {
        let save = if look.mac() { "super+enter" } else { "ctrl+enter" };
        if Chord::parse(save).is_ok_and(|c| c.matches_tx(ev, tx)) {
            s.save_automation();
            return true;
        }
    }
    // ⌘↩ / Ctrl+Enter saves the note being written.
    if s.screen == crate::model::Screen::Notes && !s.new_open && s.settings.is_none() && s.confirm_delete_note.is_none() {
        let save = if look.mac() { "super+enter" } else { "ctrl+enter" };
        if Chord::parse(save).is_ok_and(|c| c.matches_tx(ev, tx)) {
            s.save_note();
            return true;
        }
    }
    if s.menu.is_some() && ev.key == rune::Key::Escape {
        s.menu = None;
        return true;
    }
    // Escape leaves Settings when nothing over them takes the key (a sheet has its own).
    if ev.key == rune::Key::Escape && ev.mods == rune::Mods::default() && s.settings.is_some() && s.pairing.is_none() && !s.new_open && !s.shortcuts_open && s.confirm_archive.is_none() && !s.auto_open && s.viewing.is_none() {
        crate::settings::close(s);
        return true;
    }
    // Escape closes the agent panel when nothing over it takes the key.
    if ev.key == rune::Key::Escape && ev.mods == rune::Mods::default() && s.agent_visible() && !s.new_open && s.settings.is_none() && !s.shortcuts_open && s.confirm_archive.is_none() && !s.auto_open && s.viewing.is_none() {
        s.close_agent();
        return true;
    }
    for cmd in KEYED {
        if cmd != Cmd::Send && matches(look, cmd, ev, tx) {
            run(s, cmd, tx);
            return true;
        }
    }
    for n in 1..=9 {
        if matches(look, Cmd::Jump(n), ev, tx) {
            run(s, Cmd::Jump(n), tx);
            return true;
        }
    }
    false
}

/// Do `cmd`.
pub fn run(s: &mut App, cmd: Cmd, tx: &mut Tx) {
    let session = s.current().cloned();
    match cmd {
        Cmd::NewSession => s.open_new_session(false),
        Cmd::QuickTask => s.open_new_session(true),
        Cmd::Send => s.send_prompt(),
        Cmd::Attach => {
            if session.as_ref().is_some_and(|x| !x.state.is_terminal()) {
                pick_files(tx);
            }
        }
        Cmd::Dictate => {
            if session.as_ref().is_some_and(|x| !x.state.is_terminal()) {
                s.mic_press();
            }
        }
        Cmd::Stop => {
            if let Some(x) = session.filter(|x| matches!(x.state, agenthost_proto::SessionState::Running | agenthost_proto::SessionState::WaitingApproval)) {
                s.act("Stop the turn", m::SESSION_CANCEL, json!({"session_id": x.session_id}), Refresh::None);
            }
        }
        Cmd::Approve | Cmd::Deny => {
            if let Some(a) = s.current_approval().map(|a| a.approval_id.clone()) {
                s.respond(&a, cmd == Cmd::Approve);
            } else if !s.approvals.is_empty() {
                s.notice = Some(format!("No request is waiting in this session. {} goes to the next one.", label(s.look, Cmd::NextApproval)));
            }
        }
        Cmd::NextApproval => {
            let cur = session.map(|x| x.session_id);
            let mut ids: Vec<String> = vec![];
            for a in &s.approvals {
                if !ids.contains(&a.session_id) {
                    ids.push(a.session_id.clone());
                }
            }
            let next = match cur.and_then(|c| ids.iter().position(|i| *i == c)) {
                Some(i) => ids.get((i + 1) % ids.len()).cloned(),
                None => ids.first().cloned(),
            };
            match next {
                Some(id) => {
                    s.close_overlays();
                    s.open_session(&id);
                    tx.scroll_into_view("approval-card");
                }
                None => s.notice = Some("Nothing is waiting for your approval.".into()),
            }
        }
        Cmd::PrevSession | Cmd::NextSession => {
            let order = s.sidebar_order();
            if order.is_empty() {
                return;
            }
            let i = session.and_then(|x| order.iter().position(|o| *o == x.session_id));
            let n = order.len();
            let j = match (i, cmd == Cmd::NextSession) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(i), true) => (i + 1).min(n - 1),
                (Some(i), false) => i.saturating_sub(1),
            };
            let id = order[j].clone();
            s.open_session(&id);
        }
        Cmd::Jump(n) => {
            if let Some(id) = s.numbered().get(n as usize - 1).cloned() {
                s.close_overlays();
                s.open_session(&id);
            }
        }
        Cmd::Search => {
            s.close_overlays();
            s.open_search();
        }
        Cmd::ToggleSidebar => s.sidebar_open = !s.sidebar_open,
        // Their own key leaves Settings too, as it does the search field.
        Cmd::Settings if s.settings.is_some() => crate::settings::close(s),
        Cmd::Settings => s.open_settings(Tab::General),
        Cmd::ToggleSide => {
            if session.is_some_and(|x| s.has_side(&x.session_id)) {
                // The agent panel stands in for the right panel: the toggle brings the side back.
                if s.agent_visible() {
                    s.close_agent();
                    s.show_panel(PanelTab::Side);
                } else if s.side_visible() {
                    s.panel_open = false;
                } else {
                    s.show_panel(PanelTab::Side);
                }
            }
        }
        Cmd::ToggleChanges | Cmd::ToggleFiles => {
            let (tab, ok) = if cmd == Cmd::ToggleChanges { (PanelTab::Changes, s.changes_available()) } else { (PanelTab::Files, s.files_available()) };
            if ok {
                if s.toggle_files_tab(tab) {
                    s.close_overlays();
                }
            } else if !matches!(s.screen, Screen::Session(_)) {
                s.notice = Some("Open a session to see its changes and files.".into());
            } else if s.info.as_ref().is_some_and(|i| i.protocol_version < 11) {
                s.notice = Some("This host is too old to show a session's changes and files.".into());
            } else if tab == PanelTab::Changes {
                s.notice = Some("This session has no branch of its own to compare.".into());
            }
        }
        Cmd::ToggleTerminal => {
            if !s.terminal_allowed() {
                s.notice = Some(if s.admin { "This host is too old for terminals." } else { "A terminal opens in the app on the host's own computer." }.into());
            } else if s.toggle_terminal() {
                s.close_overlays();
                // Now if a terminal is there; else when its shell has started.
                s.terms.focus_wanted = true;
                tx.focus("term-view", false);
            } else {
                s.terms.focus_wanted = false;
            }
        }
        Cmd::ToggleBrowser => {
            if !s.browser_allowed() {
                s.notice = Some(if s.admin { "This host is too old for the browser." } else { "The browser opens in the app on the host's own computer." }.into());
            } else if s.toggle_browser() {
                s.close_overlays();
                tx.focus("browser-view", false);
            }
        }
        Cmd::Archive => {
            if let Some(x) = session.filter(|x| x.archived_at.is_none()) {
                s.archive(&x.session_id);
            }
        }
        Cmd::ShowFolder => {
            if let Some(p) = session.and_then(|x| s.workspace(&x.workspace_id).map(|w| w.path.clone())) {
                s.reveal(&p);
            }
        }
        Cmd::RenameSession => s.start_session_rename(),
        Cmd::CopyBranch => {
            if let Some(b) = session.and_then(|x| x.worktree).map(|w| w.branch) {
                copy_branch(s, &b, tx);
            }
        }
        Cmd::Shortcuts => {
            s.close_overlays();
            s.shortcuts_open = true;
        }
        Cmd::MainMenu => s.menu = if s.menu == Some(Menu::Main) { None } else { Some(Menu::Main) },
        Cmd::PairPhone => {
            s.open_settings(Tab::Devices);
            if s.pairing.is_none() {
                s.call(Tag::PairStart, m::DEVICE_PAIR_START, json!({}));
            }
        }
        Cmd::PairRemoteHost => {
            s.open_settings(Tab::General);
            s.start_remote_pair();
        }
        Cmd::CheckHost => {
            s.open_settings(Tab::General);
            s.call(Tag::HostInfo, m::HOST_CHECK, json!({}));
        }
        Cmd::Quit => tx.quit(),
    }
    if s.screen == Screen::Home && matches!(cmd, Cmd::Send) {
        s.menu = None;
    }
}

/// Put a branch name on the clipboard and say so.
pub fn copy_branch(s: &mut App, branch: &str, tx: &mut Tx) {
    tx.copy(branch.to_string());
    s.notice = Some("Copied branch name".into());
}

/// The file picker for attachments.
pub fn pick_files(tx: &mut Tx) {
    let options = rune::FileDialogOptions { title: "Attach a file".into(), ..Default::default() };
    tx.open_file(options, |s: &mut App, result, _| match result {
        Ok(Some(p)) => s.attach(&[p]),
        Ok(None) => {}
        Err(e) => s.error = Some(format!("Could not open the file picker: {e}")),
    });
}

type Action = fn(&mut App, &mut Tx);

/// The macOS menu bar's items: title, menu (None = the app menu), key equivalent, action.
/// `None` titles are separators.
pub fn menu_table() -> Vec<(Option<&'static str>, Option<&'static str>, Option<&'static str>, Action)> {
    fn noop(_: &mut App, _: &mut Tx) {}
    vec![
        (Some("Settings…"), None, Some("cmd+comma"), |s, tx| run(s, Cmd::Settings, tx)),
        (Some("Pair iPhone…"), None, None, |s, tx| run(s, Cmd::PairPhone, tx)),
        (Some("Pair with a Remote Host…"), None, None, |s, tx| run(s, Cmd::PairRemoteHost, tx)),
        (Some("Check Host"), None, None, |s, tx| run(s, Cmd::CheckHost, tx)),
        (Some("New Session…"), Some("File"), Some("cmd+n"), |s, tx| run(s, Cmd::NewSession, tx)),
        (Some("New Quick Task…"), Some("File"), Some("cmd+shift+n"), |s, tx| run(s, Cmd::QuickTask, tx)),
        (Some("Send"), Some("Session"), Some("enter"), |s, tx| run(s, Cmd::Send, tx)),
        (Some("Attach Files…"), Some("Session"), Some("cmd+shift+a"), |s, tx| run(s, Cmd::Attach, tx)),
        (Some("Start Dictation"), Some("Session"), Some("cmd+shift+d"), |s, tx| run(s, Cmd::Dictate, tx)),
        (Some("Stop Turn"), Some("Session"), Some("cmd+period"), |s, tx| run(s, Cmd::Stop, tx)),
        (None, Some("Session"), None, noop),
        (Some("Approve Request"), Some("Session"), Some("alt+cmd+a"), |s, tx| run(s, Cmd::Approve, tx)),
        (Some("Deny Request"), Some("Session"), Some("alt+cmd+d"), |s, tx| run(s, Cmd::Deny, tx)),
        (Some("Go to Next Waiting Approval"), Some("Session"), Some("cmd+j"), |s, tx| run(s, Cmd::NextApproval, tx)),
        (None, Some("Session"), None, noop),
        (Some("Previous Session"), Some("Session"), Some("alt+cmd+up"), |s, tx| run(s, Cmd::PrevSession, tx)),
        (Some("Next Session"), Some("Session"), Some("alt+cmd+down"), |s, tx| run(s, Cmd::NextSession, tx)),
        (Some("Go to Session 1"), Some("Session"), Some("cmd+1"), |s, tx| run(s, Cmd::Jump(1), tx)),
        (Some("Go to Session 2"), Some("Session"), Some("cmd+2"), |s, tx| run(s, Cmd::Jump(2), tx)),
        (Some("Go to Session 3"), Some("Session"), Some("cmd+3"), |s, tx| run(s, Cmd::Jump(3), tx)),
        (Some("Go to Session 4"), Some("Session"), Some("cmd+4"), |s, tx| run(s, Cmd::Jump(4), tx)),
        (Some("Go to Session 5"), Some("Session"), Some("cmd+5"), |s, tx| run(s, Cmd::Jump(5), tx)),
        (Some("Go to Session 6"), Some("Session"), Some("cmd+6"), |s, tx| run(s, Cmd::Jump(6), tx)),
        (Some("Go to Session 7"), Some("Session"), Some("cmd+7"), |s, tx| run(s, Cmd::Jump(7), tx)),
        (Some("Go to Session 8"), Some("Session"), Some("cmd+8"), |s, tx| run(s, Cmd::Jump(8), tx)),
        (Some("Go to Session 9"), Some("Session"), Some("cmd+9"), |s, tx| run(s, Cmd::Jump(9), tx)),
        (None, Some("Session"), None, noop),
        (Some("Rename Session…"), Some("Session"), None, |s, tx| run(s, Cmd::RenameSession, tx)),
        (Some("Copy Branch Name"), Some("Session"), None, |s, tx| run(s, Cmd::CopyBranch, tx)),
        (Some("Show Session Folder in Finder"), Some("Session"), Some("alt+cmd+r"), |s, tx| run(s, Cmd::ShowFolder, tx)),
        (Some("Archive Session…"), Some("Session"), Some("ctrl+cmd+a"), |s, tx| run(s, Cmd::Archive, tx)),
        (Some("Automations"), Some("View"), None, |s, _| s.open_automations()),
        (Some("Archived Sessions…"), Some("View"), None, |s, _| s.open_settings(Tab::Archived)),
        (Some("Toggle Sidebar"), Some("View"), Some("ctrl+cmd+s"), |s, tx| run(s, Cmd::ToggleSidebar, tx)),
        (Some("Toggle Side Questions"), Some("View"), Some("alt+cmd+i"), |s, tx| run(s, Cmd::ToggleSide, tx)),
        (Some("Toggle Terminal"), Some("View"), Some("ctrl+`"), |s, tx| run(s, Cmd::ToggleTerminal, tx)),
        (Some("Toggle Browser"), Some("View"), Some("shift+cmd+b"), |s, tx| run(s, Cmd::ToggleBrowser, tx)),
        (Some("Toggle Changes"), Some("View"), Some("shift+cmd+g"), |s, tx| run(s, Cmd::ToggleChanges, tx)),
        (Some("Toggle Files"), Some("View"), Some("shift+cmd+e"), |s, tx| run(s, Cmd::ToggleFiles, tx)),
        (Some("Find Session"), Some("View"), Some("cmd+f"), |s, tx| run(s, Cmd::Search, tx)),
    ]
}

/// The macOS menu bar: the app menu, File, Session and View, with every shortcut. Key
/// equivalents go to the window first (`keys_first`), so text fields keep theirs and `on_key`
/// runs the same action; picking an item runs it directly.
pub fn menu_bar() -> Vec<MenuItem<App>> {
    menu_table()
        .into_iter()
        .map(|(title, menu, keys, action)| {
            let mut i = match title {
                Some(t) => MenuItem::new(t, action).id(t).keys_first(),
                None => MenuItem::separator(),
            };
            if let Some(menu) = menu {
                i = i.menu(menu);
            }
            if let Some(k) = keys.and_then(|k| k.parse().ok()) {
                i = i.key_equivalent(k);
            }
            i
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_parses_in_both_looks() {
        for look in [Look::Mac, Look::Gnome] {
            let mut cmds: Vec<Cmd> = KEYED.to_vec();
            cmds.extend((1..=9).map(Cmd::Jump));
            for cmd in cmds {
                if let Some(c) = chord(look, cmd) {
                    assert!(Chord::parse(&c).is_ok(), "{look:?} {cmd:?}: {c}");
                    assert!(!label(look, cmd).is_empty(), "{look:?} {cmd:?} has keys but no label");
                }
            }
        }
        for (title, _, keys, _) in menu_table() {
            if let Some(k) = keys {
                assert!(k.parse::<rune::Shortcut>().is_ok(), "menu item {title:?}: {k}");
            }
        }
        assert_eq!(menu_bar().len(), menu_table().len());
    }

    #[test]
    fn approve_and_deny_keys_follow_the_look() {
        assert_eq!(label(Look::Mac, Cmd::Approve), "⌥⌘A");
        assert_eq!(label(Look::Gnome, Cmd::Approve), "Alt+A");
        assert_eq!(label(Look::Mac, Cmd::Jump(3)), "⌘3");
        assert_eq!(label(Look::Gnome, Cmd::Jump(3)), "Alt+3");
        assert_eq!(chord(Look::Gnome, Cmd::ToggleSidebar).as_deref(), Some("f9"));
        assert_eq!(label(Look::Mac, Cmd::Archive), "⌃⌘A");
        assert_eq!(label(Look::Gnome, Cmd::Archive), "Ctrl+Alt+A");
        assert_eq!(label(Look::Mac, Cmd::ToggleSide), "⌥⌘I");
        assert_eq!(chord(Look::Gnome, Cmd::ToggleSide).as_deref(), Some("shift+f9"));
    }

    #[test]
    fn no_end_session_anywhere() {
        // Archive replaced End Session: no menu item says it.
        assert!(menu_table().iter().all(|(t, ..)| !t.is_some_and(|t| t.contains("End Session"))));
    }
}
