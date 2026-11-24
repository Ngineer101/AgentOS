import Foundation

/// Notes (protocol 9): plain-text ideas and todos kept with a workspace for
/// later. A phone may do everything with them: a note only becomes a prompt
/// when you start a session from it (`createSession(noteId:)`), and its text
/// then waits in that session's composer.
extension AppModel {
    var hostSupportsNotes: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.noteProtocolVersion
    }

    func note(_ id: String) -> NoteModel? {
        notes?.first { $0.noteId == id }
    }

    /// Open notes in a workspace.
    func openNoteCount(_ workspaceId: String) -> Int {
        notes?.filter { $0.workspaceId == workspaceId && !$0.isDone }.count ?? 0
    }

    /// `note.list`. Throws, for the list screen to show why.
    func loadNotes() async throws {
        guard hostSupportsNotes else {
            notes = nil
            return
        }
        #if DEBUG
        if isDemo { return }
        #endif
        let list: [NoteModel] = try await call(RPC.noteList, [:])
        notes = NoteModel.sorted(list)
    }

    /// `note.list`, quietly (after reconnecting).
    func refreshNotes() async {
        try? await loadNotes()
    }

    /// `note.changed`. Ignored until the list is loaded.
    func applyNoteChanged(_ data: Data) {
        guard let c = try? JSONDecoder().decode(NoteChangedModel.self, from: data) else { return }
        if c.removed == true {
            notes?.removeAll { $0.noteId == c.noteId }
        } else if let n = c.note {
            upsertNote(n)
        }
    }

    func upsertNote(_ n: NoteModel) {
        guard var list = notes else { return }
        if let i = list.firstIndex(where: { $0.noteId == n.noteId }) {
            list[i] = n
        } else {
            list.append(n)
        }
        notes = NoteModel.sorted(list)
    }

    private func checkNoteText(_ text: String) throws -> String {
        let t = NoteModel.clean(text)
        if NoteModel.title(of: t).isEmpty {
            throw ComposerError(message: "A note needs some text.")
        }
        if t.count > RPC.maxNoteText {
            throw ComposerError(message: "A note has at most \(RPC.maxNoteText) characters.")
        }
        return t
    }

    /// `note.create`.
    @discardableResult
    func createNote(workspaceId: String, text: String) async throws -> NoteModel {
        let t = try checkNoteText(text)
        #if DEBUG
        if isDemo {
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            let n = NoteModel(noteId: "note-\(UUID().uuidString.prefix(8).lowercased())", workspaceId: workspaceId,
                              text: t, createdAt: now, updatedAt: now)
            upsertNote(n)
            return n
        }
        #endif
        let n: NoteModel = try await call(RPC.noteCreate, ["workspace_id": workspaceId, "text": t])
        upsertNote(n)
        return n
    }

    /// `note.update` of the text.
    @discardableResult
    func updateNote(_ id: String, text: String) async throws -> NoteModel {
        let t = try checkNoteText(text)
        #if DEBUG
        if isDemo, var n = note(id) {
            n.text = t
            n.updatedAt = Int64(Date().timeIntervalSince1970 * 1000)
            upsertNote(n)
            return n
        }
        #endif
        let n: NoteModel = try await call(RPC.noteUpdate, ["note_id": id, "text": t])
        upsertNote(n)
        return n
    }

    /// Mark a note done, or open it again. Shown at once; put back if the
    /// host refuses.
    func setNoteDone(_ id: String, _ done: Bool) async throws {
        guard let before = note(id) else { return }
        var n = before
        n.doneAt = done ? (n.doneAt ?? Int64(Date().timeIntervalSince1970 * 1000)) : nil
        upsertNote(n)
        #if DEBUG
        if isDemo { return }
        #endif
        do {
            let saved: NoteModel = try await call(RPC.noteUpdate, ["note_id": id, "done": done])
            upsertNote(saved)
        } catch {
            upsertNote(before)
            throw error
        }
    }

    /// `note.delete`. Its sessions stay.
    func deleteNote(_ id: String) async throws {
        #if DEBUG
        if isDemo {
            notes?.removeAll { $0.noteId == id }
            return
        }
        #endif
        try await send(RPC.noteDelete, ["note_id": id])
        notes?.removeAll { $0.noteId == id }
    }

    /// A session was started from a note: the note lists it, and is open
    /// again (the host does the same; this shows it before `note.changed`).
    func noteStartedSession(_ noteId: String, sessionId: String) {
        guard var n = note(noteId) else { return }
        n.sessionIds = (n.sessionIds ?? []) + [sessionId]
        n.doneAt = nil
        upsertNote(n)
    }

    /// Demo: archiving a session started from a note marks the note done,
    /// as the host does.
    func notesDoneForArchived(_ sessionId: String) {
        guard let list = notes else { return }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        for var n in list where !n.isDone && (n.sessionIds ?? []).contains(sessionId) {
            n.doneAt = now
            upsertNote(n)
        }
    }
}
