//! Notes: plain-text ideas and todos kept with a workspace, for later
//! (protocol 9). The first line is the note's title. A note can start a
//! session (`session.create` with `note_id`); archiving a session started
//! from a note marks the note done.

use serde::{Deserialize, Serialize};

/// Longest note, in characters.
pub const MAX_NOTE_TEXT: usize = 20_000;
/// Most notes one workspace may hold.
pub const MAX_NOTES_PER_WORKSPACE: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub note_id: String,
    pub workspace_id: String,
    /// Plain text; the first non-blank line is its title.
    pub text: String,
    /// When it was marked done (by the user, or by archiving a session
    /// started from it). `None` while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    /// Sessions started from it, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Note {
    /// The first non-blank line, trimmed.
    pub fn title(&self) -> &str {
        title_of(&self.text)
    }

    /// The text after the title line, trimmed (empty when there is none).
    pub fn body(&self) -> &str {
        body_of(&self.text)
    }
}

/// The first non-blank line of a note's text, trimmed.
pub fn title_of(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// What follows a note's title line, trimmed.
pub fn body_of(text: &str) -> &str {
    let t = text.trim_start();
    match t.find('\n') {
        Some(i) => t[i + 1..].trim(),
        None => "",
    }
}

/// `note.list`: one workspace's notes, or every workspace's.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NoteListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteCreateParams {
    pub workspace_id: String,
    pub text: String,
}

/// `note.update`: change the text, mark it done or open again, or both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteUpdateParams {
    pub note_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteRef {
    pub note_id: String,
}

/// `note.changed`: a note was created or changed (`note`), or deleted
/// (`removed`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteChanged {
    pub note_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_body() {
        assert_eq!(title_of("\n  Fix the login flow  \nIt drops the token\n\nsometimes"), "Fix the login flow");
        assert_eq!(body_of("\n  Fix the login flow  \nIt drops the token\n\nsometimes"), "It drops the token\n\nsometimes");
        assert_eq!(title_of("one line"), "one line");
        assert_eq!(body_of("one line"), "");
        assert_eq!(title_of("   \n "), "");
    }
}
