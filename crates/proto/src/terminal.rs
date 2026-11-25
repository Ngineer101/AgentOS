//! Terminals: shells the host runs for you, outside any sandbox (protocol 10).
//!
//! The host owns the shell and its pseudo-terminal, so a terminal outlives
//! the client that opened it. Clients are terminal emulators: they send
//! keystrokes (`terminal.input`) and get the shell's raw output back
//! (`terminal.output`), which they interpret themselves.
//!
//! Output is a byte stream counted from the terminal's start: every
//! `terminal.output` says where its bytes begin (`offset`). The host keeps
//! the last [`TERMINAL_REPLAY_BYTES`] so a client that attaches later sees
//! what was on screen. A client acknowledges what it has processed
//! (`terminal.ack`); the host sends at most [`TERMINAL_WINDOW_BYTES`] past
//! that and then stops reading the shell, so a program printing faster than
//! a client can draw waits, as it would in any terminal.
//!
//! A terminal is a shell as you, with your logins: every `terminal.*` method
//! needs the admin capability, which only the local socket has.

use serde::{Deserialize, Serialize};

/// Most terminals a host keeps open at once.
pub const MAX_TERMINALS: usize = 16;
/// How much of a terminal's latest output the host keeps to replay.
pub const TERMINAL_REPLAY_BYTES: usize = 1024 * 1024;
/// How far past a client's last acknowledgement the host sends.
pub const TERMINAL_WINDOW_BYTES: u64 = 512 * 1024;
/// Most bytes in one `terminal.output`.
pub const TERMINAL_CHUNK_BYTES: usize = 32 * 1024;
/// Most bytes in one `terminal.input` (before base64).
pub const TERMINAL_INPUT_BYTES: usize = 256 * 1024;
/// A terminal's size is kept within these.
pub const TERMINAL_MAX_COLS: u16 = 1000;
pub const TERMINAL_MAX_ROWS: u16 = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Terminal {
    pub terminal_id: String,
    /// The folder the shell started in.
    pub cwd: String,
    /// The shell's program ("/bin/zsh").
    pub shell: String,
    pub cols: u16,
    pub rows: u16,
    pub created_at: i64,
    /// Set once the shell has ended. The terminal stays, with its last
    /// output, until `terminal.close`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited: Option<TerminalExit>,
}

/// How a terminal's shell ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TerminalExit {
    /// The shell's exit status, when it exited by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// The signal that ended it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// `terminal.list` result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TerminalList {
    pub terminals: Vec<Terminal>,
}

/// `terminal.open`: start your login shell in a new terminal. Returns the
/// [`Terminal`]; attach to it to see its output.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TerminalOpenParams {
    /// An existing folder (absolute). Your home folder when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalRef {
    pub terminal_id: String,
}

/// `terminal.attach`: start getting a terminal's output on this connection,
/// beginning with what the host kept of it. A connection attaches to a
/// terminal once; attaching again starts over from the kept output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalAttachParams {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalAttachResult {
    pub terminal: Terminal,
    /// Where the output that follows begins.
    pub offset: u64,
    /// Where the terminal's output stood when you attached. What lies
    /// before it arrives marked [`TerminalOutput::replay`].
    pub end: u64,
}

/// `terminal.input`: bytes for the shell, as a terminal sends them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalInputParams {
    pub terminal_id: String,
    /// Base64.
    pub data: String,
}

/// `terminal.resize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalResizeParams {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// `terminal.ack`: this connection has processed the terminal's output up
/// to `offset`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalAckParams {
    pub terminal_id: String,
    pub offset: u64,
}

/// `terminal.output` notification: the next bytes of an attached terminal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalOutput {
    pub terminal_id: String,
    /// Where `data` begins in the terminal's output. It follows the last
    /// notification's end unless the client fell further behind than the
    /// host keeps; then it jumps ahead.
    pub offset: u64,
    /// Base64.
    pub data: String,
    /// These bytes were printed before this connection attached: draw them,
    /// but don't answer the queries in them (the program that asked got its
    /// answer long ago, and a late one would land in the shell as typing).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replay: bool,
}

/// `terminal.changed` notification: a terminal opened, was resized, its
/// shell ended, or (`removed`) it was closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalChanged {
    pub terminal: Terminal,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}
