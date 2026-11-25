//! Method names, notification names and the capability gate.
//!
//! The gate lives here, in the shared crate, so the host and every client
//! agree on which methods a device may call.

use serde::{Deserialize, Serialize};

// ---- Methods (client -> host) ----
pub const HELLO: &str = "hello";
pub const HOST_INFO: &str = "host.info";
pub const HOST_CHECK: &str = "host.check";

pub const WORKSPACE_LIST: &str = "workspace.list";
pub const WORKSPACE_ADD: &str = "workspace.add";
pub const WORKSPACE_UPDATE: &str = "workspace.update";
pub const WORKSPACE_REMOVE: &str = "workspace.remove";

pub const SESSION_LIST: &str = "session.list";
pub const SESSION_GET: &str = "session.get";
pub const SESSION_CREATE: &str = "session.create";
pub const SESSION_PROMPT: &str = "session.prompt";
pub const SESSION_CANCEL: &str = "session.cancel";
pub const SESSION_END: &str = "session.end";
/// End a session if it is live and hide it from the session list.
pub const SESSION_ARCHIVE: &str = "session.archive";
/// Show an archived session in the session list again.
pub const SESSION_UNARCHIVE: &str = "session.unarchive";
/// Give a session a title of its own (or clear it, back to the host's).
pub const SESSION_RENAME: &str = "session.rename";
/// Pin or unpin a session (protocol 8): `SessionPinParams`, returns the session.
pub const SESSION_PIN: &str = "session.pin";
/// Change one of the agent's session settings (model, effort, …).
pub const SESSION_SET_CONFIG: &str = "session.set_config";
/// Ask a side question (`/btw`) in a fork of the conversation.
pub const SESSION_SIDE_ASK: &str = "session.side_ask";
/// The login new sessions in a workspace start with.
pub const WORKSPACE_SET_DEFAULT_AGENT: &str = "workspace.set_default_agent";

pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
pub const EVENTS_UNSUBSCRIBE: &str = "events.unsubscribe";

pub const APPROVAL_RESPOND: &str = "approval.respond";
pub const APPROVAL_LIST: &str = "approval.list";

pub const CONNECTOR_LIST: &str = "connector.list";
pub const CONNECTOR_ADD: &str = "connector.add";
pub const CONNECTOR_UPDATE: &str = "connector.update";
pub const CONNECTOR_REMOVE: &str = "connector.remove";
pub const CONNECTOR_SIGN_IN: &str = "connector.sign_in";
pub const CONNECTOR_REFRESH: &str = "connector.refresh";
/// The host's own tools and whether each asks first.
pub const BUILTIN_LIST: &str = "builtin.list";
/// Let one of the host's own tools run without asking (or ask again).
pub const BUILTIN_SET_POLICY: &str = "builtin.set_policy";
/// Search the integrations.sh catalog for MCP servers.
pub const CONNECTOR_SEARCH: &str = "connector.search";

/// New workspaces from any client (phones too), but only inside the project
/// folders chosen on the desktop (`project.set_roots`, admin only).
pub const PROJECT_BROWSE: &str = "project.browse";
pub const PROJECT_CREATE: &str = "project.create";
pub const PROJECT_SET_ROOTS: &str = "project.set_roots";

/// Agent profiles: extra logins of an agent, each with its own config folder.
/// `host.info` lists them too (one `agents` entry per login).
pub const AGENT_PROFILE_LIST: &str = "agent.profile_list";
pub const AGENT_PROFILE_ADD: &str = "agent.profile_add";
pub const AGENT_PROFILE_REMOVE: &str = "agent.profile_remove";

pub const DEVICE_LIST: &str = "device.list";
pub const DEVICE_PAIR_START: &str = "device.pair_start";
pub const DEVICE_PAIR_CONFIRM: &str = "device.pair_confirm";
pub const DEVICE_RENAME: &str = "device.rename";
pub const DEVICE_REVOKE: &str = "device.revoke";

/// Send a file to the host in pieces: `upload.begin`, then `upload.append`
/// until all bytes are sent, then `upload.finish`, which returns an
/// [`crate::Attachment`] to pass to `session.prompt` or `voice.transcribe`.
pub const UPLOAD_BEGIN: &str = "upload.begin";
pub const UPLOAD_APPEND: &str = "upload.append";
pub const UPLOAD_FINISH: &str = "upload.finish";
/// An image sent with a prompt, in pieces (protocol 7): give `offset` 0,
/// then the end of each piece, until all `size` bytes are in. The host keeps
/// a session's sent images as long as the session, so clients can show them
/// again in the transcript.
pub const ATTACHMENT_READ: &str = "attachment.read";

/// The local speech-to-text model: whether it is installed, downloading or ready.
pub const VOICE_STATUS: &str = "voice.status";
/// Download and install the speech model (once per host).
pub const VOICE_DOWNLOAD: &str = "voice.download";
/// Turn an uploaded recording (WAV) into text. The recording is deleted after.
pub const VOICE_TRANSCRIBE: &str = "voice.transcribe";
/// Live dictation: `voice.stream_begin` opens a stream, each
/// `voice.stream_append` feeds it audio and answers with the text so far,
/// `voice.stream_finish` ends it with the final text. Only when
/// [`crate::VoiceStatus::streaming`] is true. Streams left alone for a minute
/// are dropped.
pub const VOICE_STREAM_BEGIN: &str = "voice.stream_begin";
pub const VOICE_STREAM_APPEND: &str = "voice.stream_append";
pub const VOICE_STREAM_FINISH: &str = "voice.stream_finish";

/// Screen sharing through screen-relay: what can be shown, and the policy.
pub const SCREEN_STATUS: &str = "screen.status";
/// Start the engine (if needed) and get a one-time ticket for `screen.attach`.
pub const SCREEN_START: &str = "screen.start";
/// Stop the engine and close every viewer.
pub const SCREEN_STOP: &str = "screen.stop";
/// The first message on a new tunnel: turn it into a screen stream. After the
/// response, the tunnel carries screen-relay `nsl1` messages, not JSON-RPC.
pub const SCREEN_ATTACH: &str = "screen.attach";
/// Allow phones to watch (and click and type). Desktop only.
pub const SCREEN_SET_POLICY: &str = "screen.set_policy";

/// Push notifications through APNs. Usually the relay that serves the
/// phone's build of the app sends them to Apple (`push.register` with
/// `gateway`, protocol 6); otherwise the host does, with an APNs key of its
/// own (`push.set_key`, which a phone may call while pairing: it is only the
/// host's credential for Apple). Either way every notification's content is
/// encrypted to the receiving phone.
pub const PUSH_STATUS: &str = "push.status";
pub const PUSH_SET_KEY: &str = "push.set_key";
pub const PUSH_CLEAR_KEY: &str = "push.clear_key";
pub const PUSH_REGISTER: &str = "push.register";
pub const PUSH_UNREGISTER: &str = "push.unregister";
pub const PUSH_SET_PREFS: &str = "push.set_prefs";
pub const PUSH_TEST: &str = "push.test";

/// Automations: saved sessions that run on a schedule, from a webhook or by
/// hand. Phones may manage them, except webhook triggers and pre-approved
/// tools, which the host checks per field (admin only).
pub const AUTOMATION_LIST: &str = "automation.list";
pub const AUTOMATION_GET: &str = "automation.get";
pub const AUTOMATION_CREATE: &str = "automation.create";
pub const AUTOMATION_UPDATE: &str = "automation.update";
pub const AUTOMATION_DELETE: &str = "automation.delete";
pub const AUTOMATION_RUN_NOW: &str = "automation.run_now";
/// Runs of one automation, newest first.
pub const AUTOMATION_RUNS: &str = "automation.runs";
/// Check a cron expression: in words, and its next times.
pub const AUTOMATION_CHECK_SCHEDULE: &str = "automation.check_schedule";

/// Notes (protocol 9): plain-text ideas and todos kept with a workspace for
/// later. Phones may manage them: a note only becomes a prompt when the user
/// starts a session from it.
pub const NOTE_LIST: &str = "note.list";
pub const NOTE_CREATE: &str = "note.create";
pub const NOTE_UPDATE: &str = "note.update";
pub const NOTE_DELETE: &str = "note.delete";

/// Terminals: shells the host runs as you, outside any sandbox (protocol 10;
/// see [`crate::terminal`]). Admin only: a terminal can do anything you can.
pub const TERMINAL_LIST: &str = "terminal.list";
/// Start your login shell in a new terminal: `TerminalOpenParams`, returns the `Terminal`.
pub const TERMINAL_OPEN: &str = "terminal.open";
/// Get a terminal's output on this connection, as `terminal.output` notifications.
pub const TERMINAL_ATTACH: &str = "terminal.attach";
/// Stop getting a terminal's output. The shell keeps running.
pub const TERMINAL_DETACH: &str = "terminal.detach";
/// Keystrokes (or a paste) for the shell.
pub const TERMINAL_INPUT: &str = "terminal.input";
pub const TERMINAL_RESIZE: &str = "terminal.resize";
/// How much output this connection has processed, so the host sends more.
pub const TERMINAL_ACK: &str = "terminal.ack";
/// End the shell (hang up) and forget the terminal.
pub const TERMINAL_CLOSE: &str = "terminal.close";

// ---- Notifications (host -> client) ----
/// A persisted event; params are an [`crate::Event`].
pub const N_EVENT: &str = "event";
/// An ephemeral streaming chunk that will be coalesced into a later event.
pub const N_SESSION_DELTA: &str = "session.delta";
/// A session was created or changed; params are a [`crate::Session`].
pub const N_SESSION_CHANGED: &str = "session.changed";
/// A pairing handshake finished and waits for confirmation on the desktop.
pub const N_PAIRING_PENDING: &str = "pairing.pending";
/// A pairing finished (confirmed, refused or expired).
pub const N_PAIRING_DONE: &str = "pairing.done";
/// Sent to a device right before the host closes its connection after revoking it.
pub const N_DEVICE_REVOKED: &str = "device.revoked";
/// A connector's status or tools changed on its own (sign-in finished or
/// failed). Params: `{"connector_id"}`. Admin connections only.
pub const N_CONNECTOR_CHANGED: &str = "connector.changed";
/// The speech model's status changed (download progress, installed, failed).
/// Params are a [`crate::VoiceStatus`]. Sent to every connection.
pub const N_VOICE_CHANGED: &str = "voice.changed";
/// Screen sharing started, stopped or its policy changed. Params are a
/// [`crate::ScreenStatus`]. Sent to every connection.
pub const N_SCREEN_CHANGED: &str = "screen.changed";
/// An automation changed or a run of it started or finished. Params are an
/// [`crate::AutomationChanged`]. Sent to every connection.
pub const N_AUTOMATION_CHANGED: &str = "automation.changed";
/// A note was created, changed or deleted. Params are a
/// [`crate::NoteChanged`]. Sent to every connection.
pub const N_NOTE_CHANGED: &str = "note.changed";
/// The next bytes of a terminal this connection attached to. Params are a
/// [`crate::TerminalOutput`].
pub const N_TERMINAL_OUTPUT: &str = "terminal.output";
/// A terminal opened, was resized, ended or was closed. Params are a
/// [`crate::TerminalChanged`]. Sent to admin connections.
pub const N_TERMINAL_CHANGED: &str = "terminal.changed";

/// One permission a device holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read host status, workspaces, connectors, devices.
    Read,
    /// Create, prompt, cancel and end sessions; subscribe to events.
    Sessions,
    /// Answer MCP approval requests.
    Approvals,
    /// Change workspaces, connectors, policy and devices. Desktop only.
    Admin,
}

impl Capability {
    /// Everything: the local desktop and command-line client.
    pub fn desktop() -> Vec<Capability> {
        vec![
            Capability::Read,
            Capability::Sessions,
            Capability::Approvals,
            Capability::Admin,
        ]
    }
    /// A paired phone: sessions and approvals, never policy.
    pub fn phone() -> Vec<Capability> {
        vec![
            Capability::Read,
            Capability::Sessions,
            Capability::Approvals,
        ]
    }
}

/// The capability a method requires, or `None` for an unknown method.
pub fn required_capability(method: &str) -> Option<Capability> {
    use Capability::*;
    Some(match method {
        HELLO | HOST_INFO | WORKSPACE_LIST | CONNECTOR_LIST | DEVICE_LIST | SESSION_LIST
        | SESSION_GET | APPROVAL_LIST | VOICE_STATUS | AGENT_PROFILE_LIST | BUILTIN_LIST
        | SCREEN_STATUS | PUSH_STATUS | AUTOMATION_LIST | AUTOMATION_GET | AUTOMATION_RUNS
        | AUTOMATION_CHECK_SCHEDULE | NOTE_LIST => Read,
        SESSION_CREATE | SESSION_PROMPT | SESSION_CANCEL | SESSION_END | SESSION_ARCHIVE
        | SESSION_UNARCHIVE | SESSION_RENAME | SESSION_PIN | SESSION_SET_CONFIG | SESSION_SIDE_ASK | WORKSPACE_SET_DEFAULT_AGENT | EVENTS_SUBSCRIBE
        | EVENTS_UNSUBSCRIBE | UPLOAD_BEGIN | UPLOAD_APPEND | UPLOAD_FINISH | ATTACHMENT_READ | VOICE_DOWNLOAD
        | VOICE_TRANSCRIBE | VOICE_STREAM_BEGIN | VOICE_STREAM_APPEND | VOICE_STREAM_FINISH
        // Bounded by the project folders, which only an admin sets.
        | PROJECT_BROWSE | PROJECT_CREATE
        // Bounded by the screen policy, which only an admin sets.
        | SCREEN_START | SCREEN_STOP | SCREEN_ATTACH
        // A device's own notifications; the key only lets the host talk to Apple.
        | PUSH_SET_KEY | PUSH_REGISTER | PUSH_UNREGISTER | PUSH_SET_PREFS | PUSH_TEST
        // A deferred session.create + session.prompt. Webhook triggers and
        // pre-approved tools widen policy: the host requires admin for those.
        | AUTOMATION_CREATE | AUTOMATION_UPDATE | AUTOMATION_DELETE | AUTOMATION_RUN_NOW
        | NOTE_CREATE | NOTE_UPDATE | NOTE_DELETE => Sessions,
        APPROVAL_RESPOND => Approvals,
        HOST_CHECK | WORKSPACE_ADD | WORKSPACE_UPDATE | WORKSPACE_REMOVE | CONNECTOR_ADD
        | CONNECTOR_UPDATE | CONNECTOR_REMOVE | CONNECTOR_SIGN_IN | CONNECTOR_REFRESH
        | CONNECTOR_SEARCH | BUILTIN_SET_POLICY
        | DEVICE_PAIR_START | DEVICE_PAIR_CONFIRM | DEVICE_RENAME | DEVICE_REVOKE
        // A profile's folder becomes writable inside every session that uses it.
        | AGENT_PROFILE_ADD | AGENT_PROFILE_REMOVE
        | PROJECT_SET_ROOTS
        // Lets a phone see, and maybe control, everything you are logged in to.
        | SCREEN_SET_POLICY
        // A shell as you, outside the sandbox, with your logins.
        | TERMINAL_LIST | TERMINAL_OPEN | TERMINAL_ATTACH | TERMINAL_DETACH | TERMINAL_INPUT
        | TERMINAL_RESIZE | TERMINAL_ACK | TERMINAL_CLOSE
        | PUSH_CLEAR_KEY => Admin,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_cannot_widen_policy() {
        let phone = Capability::phone();
        for m in [
            WORKSPACE_ADD,
            WORKSPACE_UPDATE,
            WORKSPACE_REMOVE,
            CONNECTOR_ADD,
            CONNECTOR_UPDATE,
            CONNECTOR_REMOVE,
            DEVICE_PAIR_START,
            DEVICE_REVOKE,
            AGENT_PROFILE_ADD,
            AGENT_PROFILE_REMOVE,
            PROJECT_SET_ROOTS,
            BUILTIN_SET_POLICY,
            SCREEN_SET_POLICY,
            PUSH_CLEAR_KEY,
            TERMINAL_LIST,
            TERMINAL_OPEN,
            TERMINAL_ATTACH,
            TERMINAL_DETACH,
            TERMINAL_INPUT,
            TERMINAL_RESIZE,
            TERMINAL_ACK,
            TERMINAL_CLOSE,
        ] {
            assert!(!phone.contains(&required_capability(m).unwrap()), "{m}");
        }
        for m in [
            SESSION_CREATE,
            SESSION_PROMPT,
            APPROVAL_RESPOND,
            EVENTS_SUBSCRIBE,
            HOST_INFO,
            UPLOAD_BEGIN,
            UPLOAD_APPEND,
            UPLOAD_FINISH,
            ATTACHMENT_READ,
            VOICE_STATUS,
            VOICE_DOWNLOAD,
            VOICE_TRANSCRIBE,
            VOICE_STREAM_BEGIN,
            VOICE_STREAM_APPEND,
            VOICE_STREAM_FINISH,
            AGENT_PROFILE_LIST,
            PROJECT_BROWSE,
            PROJECT_CREATE,
            SESSION_ARCHIVE,
            SESSION_UNARCHIVE,
            WORKSPACE_SET_DEFAULT_AGENT,
            SESSION_SET_CONFIG,
            SESSION_SIDE_ASK,
            SESSION_RENAME,
            SESSION_PIN,
            SCREEN_STATUS,
            SCREEN_START,
            SCREEN_STOP,
            PUSH_STATUS,
            PUSH_SET_KEY,
            PUSH_REGISTER,
            PUSH_UNREGISTER,
            PUSH_SET_PREFS,
            PUSH_TEST,
            AUTOMATION_LIST,
            AUTOMATION_GET,
            AUTOMATION_CREATE,
            AUTOMATION_UPDATE,
            AUTOMATION_DELETE,
            AUTOMATION_RUN_NOW,
            AUTOMATION_RUNS,
            AUTOMATION_CHECK_SCHEDULE,
            NOTE_LIST,
            NOTE_CREATE,
            NOTE_UPDATE,
            NOTE_DELETE,
        ] {
            assert!(phone.contains(&required_capability(m).unwrap()), "{m}");
        }
    }
}
