import SwiftUI

// Notes (hosts on protocol 9): ideas and todos kept with a workspace until
// they're ready to start. The list (every workspace's open notes, done ones
// on request), a note's editor, and the New Note sheet.
// Designs: "iOS — Notes", "iOS — Note", "iOS — New note (sheet)".

/// The workspaces in the list's order: Quick tasks last, the rest by name.
private func noteWorkspaces(_ model: AppModel) -> [WorkspaceInfo] {
    model.workspaces.sorted {
        if $0.isQuickTasks != $1.isQuickTasks { return !$0.isQuickTasks }
        return $0.name.localizedStandardCompare($1.name) == .orderedAscending
    }
}

private func workspaceTitle(_ model: AppModel, _ id: String) -> String {
    guard let w = model.workspace(id) else { return "Removed workspace" }
    return w.isQuickTasks ? "Quick tasks" : w.name
}

private func relative(_ ms: Int64) -> String {
    Date(timeIntervalSince1970: TimeInterval(ms) / 1000).formatted(.relative(presentation: .named))
}

// MARK: - The list

struct NotesView: View {
    @EnvironmentObject private var model: AppModel
    @AppStorage("notesShowDone") private var showDone = false
    @State private var loadError: String?
    @State private var actionError: String?
    /// The New Note sheet, in this workspace.
    @State private var newIn: NewNoteTarget?
    /// The New Session sheet for this note.
    @State private var starting: NoteModel?
    /// Asking before deleting.
    @State private var deleting: NoteModel?

    private var open: [NoteModel] { (model.notes ?? []).filter { !$0.isDone } }
    private var done: [NoteModel] { (model.notes ?? []).filter(\.isDone) }

    var body: some View {
        List {
            if let actionError {
                Section {
                    Text(actionError).foregroundStyle(Palette.red)
                }
            }
            ForEach(noteWorkspaces(model)) { w in
                let notes = open.filter { $0.workspaceId == w.workspaceId }
                if !notes.isEmpty {
                    Section(caps: w.isQuickTasks ? "Quick tasks" : w.name) {
                        ForEach(notes) { n in row(n, showWorkspace: false) }
                    }
                }
            }
            if showDone, !done.isEmpty {
                Section(caps: "Done") {
                    ForEach(done) { n in row(n, showWorkspace: true) }
                }
            }
            if model.notes?.isEmpty == false {
                Section {
                } footer: {
                    Text("Tap the circle to mark a note done. Swipe right to start a session from it; archiving that session marks the note done.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Notes")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Toggle(isOn: $showDone) {
                        Label("Show Done", systemImage: "checkmark.circle")
                    }
                } label: {
                    ToolbarIcon(systemName: "ellipsis")
                }
                .accessibilityLabel("More")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    newIn = NewNoteTarget(workspaceId: nil)
                } label: {
                    ToolbarIcon(systemName: "square.and.pencil")
                }
                .accessibilityLabel("New Note")
                .disabled(!model.link.isOnline || model.workspaces.isEmpty)
            }
        }
        .overlay {
            if model.notes == nil {
                if let loadError {
                    ContentUnavailableView(
                        "Couldn't Load Notes",
                        systemImage: "exclamationmark.triangle",
                        description: Text(loadError)
                    )
                } else if !model.hostSupportsNotes {
                    ContentUnavailableView(
                        "Update AgentOS",
                        systemImage: "note.text",
                        description: Text("Update agenthostd on \(model.hostName) to keep notes with your workspaces.")
                    )
                } else if model.link.isOnline {
                    ProgressView()
                } else {
                    ContentUnavailableView(
                        "Not Connected",
                        systemImage: "wifi.slash",
                        description: Text("Notes load when the host is reachable.")
                    )
                }
            } else if open.isEmpty && (!showDone || done.isEmpty) {
                ContentUnavailableView {
                    Label(done.isEmpty ? "No Notes" : "No Open Notes", systemImage: "note.text")
                } description: {
                    Text("Keep ideas and todos with a project until they're ready to start.")
                } actions: {
                    Button("New Note") { newIn = NewNoteTarget(workspaceId: nil) }
                        .disabled(!model.link.isOnline || model.workspaces.isEmpty)
                }
            }
        }
        .task {
            await load()
            #if DEBUG
            if model.isDemo, DemoData.requestedScreen == .noteNew { newIn = NewNoteTarget(workspaceId: "ws-relay") }
            #endif
        }
        .refreshable { await load() }
        .sheet(item: $newIn) { target in
            NewNoteView(workspaceId: target.workspaceId)
                .environmentObject(model)
        }
        .sheet(item: $starting) { n in
            NewSessionView(note: n) { created in
                starting = nil
                model.openSessionRequest = created.sessionId
            }
            .environmentObject(model)
        }
        .confirmationDialog(
            deleteTitle(deleting),
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { n in
            Button("Delete Note", role: .destructive) { delete(n) }
        } message: { _ in
            Text("Sessions started from it stay.")
        }
    }

    private func row(_ n: NoteModel, showWorkspace: Bool) -> some View {
        NavigationLink(value: Route.note(n.noteId)) {
            NoteRow(note: n, workspace: showWorkspace ? workspaceTitle(model, n.workspaceId) : nil) {
                setDone(n, !n.isDone)
            }
        }
        .swipeActions(edge: .leading, allowsFullSwipe: true) {
            Button {
                starting = n
            } label: {
                Label("Start Session", systemImage: "play")
            }
            .tint(.blue)
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            Button(role: .destructive) {
                deleting = n
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
        .contextMenu {
            Button {
                starting = n
            } label: {
                Label("Start Session", systemImage: "play")
            }
            Button {
                setDone(n, !n.isDone)
            } label: {
                Label(n.isDone ? "Reopen" : "Mark Done", systemImage: n.isDone ? "circle" : "checkmark.circle")
            }
            Button(role: .destructive) {
                deleting = n
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    private func load() async {
        guard model.link.isOnline || model.isDemo else { return }
        do {
            try await model.loadNotes()
            loadError = nil
        } catch {
            loadError = userMessage(for: error)
        }
    }

    private func setDone(_ n: NoteModel, _ done: Bool) {
        actionError = nil
        Task {
            do {
                try await model.setNoteDone(n.noteId, done)
            } catch {
                actionError = userMessage(for: error)
            }
        }
    }

    private func delete(_ n: NoteModel) {
        actionError = nil
        Task {
            do {
                try await model.deleteNote(n.noteId)
            } catch {
                actionError = userMessage(for: error)
            }
        }
    }
}

/// Which workspace the New Note sheet starts in (nil: the first one).
struct NewNoteTarget: Identifiable {
    var workspaceId: String?
    var id: String { workspaceId ?? "" }
}

private func deleteTitle(_ n: NoteModel?) -> String {
    guard let n else { return "Delete Note?" }
    return "Delete \u{201c}\(n.title)\u{201d}?"
}

/// A note in the list: its done circle, title, first line, and when.
struct NoteRow: View {
    var note: NoteModel
    /// Shown for done notes, which are listed together.
    var workspace: String?
    var toggleDone: () -> Void

    private var meta: String {
        var parts: [String] = []
        if let workspace { parts.append(workspace) }
        if let d = note.doneAt {
            parts.append("Done \(relative(d))")
        } else {
            parts.append(relative(note.createdAt))
        }
        let n = note.sessionIds?.count ?? 0
        if n > 0 { parts.append("\(n) \(n == 1 ? "session" : "sessions")") }
        return parts.joined(separator: " · ")
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Button(action: toggleDone) {
                Image(systemName: note.isDone ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(note.isDone ? Color.accentColor : Color.secondary)
            }
            .buttonStyle(.borderless)
            .accessibilityLabel(note.isDone ? "Reopen" : "Mark Done")
            VStack(alignment: .leading, spacing: 2) {
                Text(note.title)
                    .foregroundStyle(note.isDone ? Color.secondary : Color.primary)
                    .lineLimit(2)
                if !note.isDone, let p = note.preview {
                    Text(p)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Text(meta)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
    }
}

// MARK: - One note

/// A note's text, edited in place and saved as you go (a second after you
/// stop typing, and when you leave); the sessions started from it; Start
/// Session at the bottom.
struct NoteView: View {
    @EnvironmentObject private var model: AppModel
    let noteId: String

    @State private var text = ""
    /// The text as last loaded or saved: edits are what differs from it.
    @State private var base = ""
    @State private var loaded = false
    @State private var saveTask: Task<Void, Never>?
    @State private var error: String?
    @State private var starting = false
    @State private var deleting = false
    @Environment(\.dismiss) private var dismiss

    private var note: NoteModel? { model.note(noteId) }
    private var dirty: Bool {
        let t = NoteModel.clean(text)
        return t != base && !NoteModel.title(of: t).isEmpty
    }

    var body: some View {
        Group {
            if let note {
                editor(note)
            } else if model.notes == nil {
                ProgressView()
            } else {
                ContentUnavailableView("Note Deleted", systemImage: "note.text",
                                       description: Text("This note is no longer on \(model.hostName)."))
            }
        }
        .navigationTitle(note.map { workspaceTitle(model, $0.workspaceId) } ?? "Note")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            if model.notes == nil { await model.refreshNotes() }
            load()
        }
        // Changed on another device: shown here unless you've edited it.
        .onChange(of: note?.text) { _, new in
            guard let new, new != base else { return }
            if NoteModel.clean(text) == base {
                text = new
            }
            base = new
        }
        .onChange(of: text) { _, _ in scheduleSave() }
        .onDisappear { saveNow() }
    }

    private func editor(_ n: NoteModel) -> some View {
        List {
            Section {
                TextEditor(text: $text)
                    .frame(minHeight: 240)
                    .scrollContentBackground(.hidden)
                    .accessibilityLabel("Note")
            } footer: {
                Text(footer(n))
            }
            if let error {
                Section {
                    Text(error).foregroundStyle(Palette.red)
                }
            }
            let ids = (n.sessionIds ?? []).reversed()
            if !ids.isEmpty {
                Section {
                    ForEach(Array(ids), id: \.self) { id in
                        if let s = model.session(id) {
                            NavigationLink(value: Route.session(id)) {
                                SessionRow(session: s)
                            }
                        } else {
                            Text("An archived session")
                                .foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    GroupHeader("Sessions from this note")
                } footer: {
                    Text("Archiving a session started from this note marks the note done.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        setDone(!n.isDone)
                    } label: {
                        Label(n.isDone ? "Reopen" : "Mark Done", systemImage: n.isDone ? "circle" : "checkmark.circle")
                    }
                    Button(role: .destructive) {
                        deleting = true
                    } label: {
                        Label("Delete", systemImage: "trash")
                    }
                } label: {
                    ToolbarIcon(systemName: "ellipsis")
                }
                .accessibilityLabel("More")
            }
        }
        .safeAreaInset(edge: .bottom) {
            StartSessionButton {
                saveNow()
                starting = true
            }
            .disabled(!model.link.isOnline && !model.isDemo)
            .padding(.bottom, 8)
        }
        .sheet(isPresented: $starting) {
            NewSessionView(note: n.withText(NoteModel.clean(text).isEmpty ? n.text : NoteModel.clean(text))) { created in
                starting = false
                model.openSessionRequest = created.sessionId
            }
            .environmentObject(model)
        }
        .confirmationDialog(deleteTitle(n), isPresented: $deleting, titleVisibility: .visible) {
            Button("Delete Note", role: .destructive) { delete() }
        } message: {
            Text("Sessions started from it stay.")
        }
    }

    private func footer(_ n: NoteModel) -> String {
        let when = n.doneAt.map { "Done \(relative($0))" } ?? "Added \(relative(n.createdAt))"
        return "\(when) · The first line is the title."
    }

    private func load() {
        guard !loaded, let n = note else { return }
        loaded = true
        text = n.text
        base = n.text
    }

    private func scheduleSave() {
        guard loaded, dirty else { return }
        saveTask?.cancel()
        saveTask = Task {
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            guard !Task.isCancelled else { return }
            await save()
        }
    }

    private func saveNow() {
        saveTask?.cancel()
        guard loaded, dirty else { return }
        Task { await save() }
    }

    private func save() async {
        guard dirty else { return }
        let t = NoteModel.clean(text)
        do {
            try await model.updateNote(noteId, text: t)
            base = t
            error = nil
        } catch {
            self.error = userMessage(for: error)
        }
    }

    private func setDone(_ done: Bool) {
        saveNow()
        Task {
            do {
                try await model.setNoteDone(noteId, done)
            } catch {
                self.error = userMessage(for: error)
            }
        }
    }

    private func delete() {
        saveTask?.cancel()
        Task {
            do {
                try await model.deleteNote(noteId)
                dismiss()
            } catch {
                self.error = userMessage(for: error)
            }
        }
    }
}

extension NoteModel {
    /// The note with the editor's latest text, for starting a session.
    func withText(_ t: String) -> NoteModel {
        var n = self
        n.text = t
        return n
    }
}

/// A prominent "Start Session", in glass on iOS 26.
private struct StartSessionButton: View {
    var action: () -> Void

    var body: some View {
        if #available(iOS 26, *) {
            Button(action: action) {
                Label("Start Session", systemImage: "play.fill")
                    .fontWeight(.semibold)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
        } else {
            Button(action: action) {
                Label("Start Session", systemImage: "play.fill")
                    .fontWeight(.semibold)
                    .padding(.horizontal, 6)
            }
            .buttonStyle(.borderedProminent)
            .buttonBorderShape(.capsule)
            .controlSize(.large)
        }
    }
}

// MARK: - New note

/// A workspace and the text; Add saves it.
struct NewNoteView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    /// The workspace it starts in (nil: the first one).
    var workspaceId: String?

    @State private var workspace: String = ""
    @State private var text = ""
    @State private var saving = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var canAdd: Bool {
        !workspace.isEmpty && !NoteModel.title(of: text).isEmpty && !saving && (model.link.isOnline || model.isDemo)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("Workspace", selection: $workspace) {
                        ForEach(noteWorkspaces(model)) { w in
                            Text(w.isQuickTasks ? "Quick tasks" : w.name).tag(w.workspaceId)
                        }
                    }
                }
                Section {
                    TextEditor(text: $text)
                        .frame(minHeight: 200)
                        .focused($focused)
                        .accessibilityLabel("Note")
                } footer: {
                    Text("The first line is the title. Start a session from the note when it's ready.")
                }
                if let error {
                    Section {
                        Text(error).foregroundStyle(Palette.red)
                    }
                }
            }
            .navigationTitle("New Note")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button {
                        dismiss()
                    } label: {
                        ToolbarIcon(systemName: "xmark")
                    }
                    .accessibilityLabel("Cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else {
                        addButton
                    }
                }
            }
            .onAppear {
                workspace = workspaceId ?? noteWorkspaces(model).first?.workspaceId ?? ""
                focused = true
                #if DEBUG
                if model.isDemo, DemoData.requestedScreen == .noteNew, text.isEmpty {
                    text = "Show the relay's latency in Settings\nNext to Connected, in ms."
                }
                #endif
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }

    @ViewBuilder
    private var addButton: some View {
        if #available(iOS 26, *) {
            Button("Add", action: add)
                .buttonStyle(.glassProminent)
                .fontWeight(.semibold)
                .disabled(!canAdd)
        } else {
            Button(action: add) {
                Text("Add")
                    .fontWeight(.semibold)
                    .padding(.horizontal, 4)
            }
            .buttonStyle(.borderedProminent)
            .buttonBorderShape(.capsule)
            .disabled(!canAdd)
        }
    }

    private func add() {
        saving = true
        error = nil
        Task {
            do {
                try await model.createNote(workspaceId: workspace, text: text)
                saving = false
                dismiss()
            } catch {
                saving = false
                self.error = userMessage(for: error)
            }
        }
    }
}
