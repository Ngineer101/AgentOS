import Foundation

// Notes (`note.*`, protocol 9): plain-text ideas and todos kept with a
// workspace for later. The first line is the title. A note can start a
// session (`session.create` with `note_id`); archiving that session marks the
// note done. Mirrors `crates/proto/src/note.rs`.

extension RPC {
    /// `{workspace_id?}`: every workspace's notes (or one's), open ones first.
    static let noteList = "note.list"
    /// `{workspace_id, text}`.
    static let noteCreate = "note.create"
    /// `{note_id, text?, done?}`: only the fields given change.
    static let noteUpdate = "note.update"
    static let noteDelete = "note.delete"
    /// `{note_id, note?, removed?}`: a note was created, changed or deleted.
    static let nNoteChanged = "note.changed"
    /// Hosts on this protocol version or newer have notes.
    static let noteProtocolVersion: UInt32 = 9
    /// `MAX_NOTE_TEXT`: the longest note, in characters.
    static let maxNoteText = 20_000
}

struct NoteModel: Codable, Identifiable, Hashable {
    var noteId: String
    var workspaceId: String
    /// Plain text; the first non-blank line is the title.
    var text: String
    /// When it was marked done (nil while open).
    var doneAt: Int64? = nil
    /// Sessions started from it, oldest first.
    var sessionIds: [String]? = nil
    var createdAt: Int64
    var updatedAt: Int64

    var id: String { noteId }
    var isDone: Bool { doneAt != nil }
    var title: String { Self.title(of: text) }
    /// The lines after the title, trimmed (empty when there are none).
    var body: String { Self.body(of: text) }
    /// The first line of the body, for the list.
    var preview: String? {
        body.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.first { !$0.isEmpty }
    }

    static func title(of text: String) -> String {
        text.split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty } ?? ""
    }

    static func body(of text: String) -> String {
        let t = text.drop(while: { $0.isWhitespace })
        guard let nl = t.firstIndex(of: "\n") else { return "" }
        return t[t.index(after: nl)...].trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// The text as the host stores it: trailing space and leading blank lines dropped.
    static func clean(_ text: String) -> String {
        var t = Substring(text)
        while let last = t.last, last.isWhitespace { t.removeLast() }
        while let first = t.first, first == "\n" || first == "\r" { t.removeFirst() }
        return String(t)
    }

    /// The host's order: open notes newest first, then done ones, most
    /// recently done first.
    static func sorted(_ list: [NoteModel]) -> [NoteModel] {
        list.sorted { a, b in
            if a.isDone != b.isDone { return !a.isDone }
            let ka = a.doneAt ?? a.createdAt, kb = b.doneAt ?? b.createdAt
            if ka != kb { return ka > kb }
            return a.noteId < b.noteId
        }
    }

    enum CodingKeys: String, CodingKey {
        case noteId = "note_id"
        case workspaceId = "workspace_id"
        case text
        case doneAt = "done_at"
        case sessionIds = "session_ids"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct NoteChangedModel: Codable {
    var noteId: String
    var note: NoteModel?
    var removed: Bool?

    enum CodingKeys: String, CodingKey {
        case noteId = "note_id"
        case note, removed
    }
}
