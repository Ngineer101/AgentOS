//! A session's files and what it changed (protocol 11), read only.
//!
//! `session.changes` is what the session's branch changed since it left the
//! workspace's own branch, uncommitted work and new files included: what a
//! pull request from it would show. `session.files` lists the session's
//! folder and `session.file_read` reads one file of it.
//!
//! All three need the sessions capability: they show what the session's
//! agent can already read, and print, in the conversation.

use serde::{Deserialize, Serialize};

/// Most bytes of patch in one `session.changes`; the rest is left out, at a
/// file boundary, and `truncated` says so.
pub const CHANGES_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Most paths in one `session.files`.
pub const FILES_MAX: usize = 50_000;
/// Most bytes of a file in one `session.file_read`.
pub const FILE_READ_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionChanges {
    /// The session's branch.
    pub branch: String,
    /// The commit the changes are measured from: where the session's branch
    /// and the workspace's own meet.
    pub base: String,
    /// `git diff` output (unified, three lines of context, renames found),
    /// then each new file git doesn't track yet as an added file.
    pub patch: String,
    /// Some files were left out: the patch passed [`CHANGES_MAX_BYTES`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionFiles {
    /// Paths under the session's folder, `/` separated, sorted: what git
    /// tracks plus new files it doesn't ignore; every file when the folder
    /// isn't in a repository (without `.git`).
    pub files: Vec<String>,
    /// There are more than [`FILES_MAX`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionFileReadParams {
    pub session_id: String,
    /// A path as `session.files` lists it.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionFile {
    pub path: String,
    /// The whole file's size in bytes.
    pub size: u64,
    /// The file's text; `None` for a file that isn't UTF-8 text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `text` is only the file's first [`FILE_READ_MAX_BYTES`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}
