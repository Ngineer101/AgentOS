import SwiftUI

/// Settings → Archived Sessions: sessions hidden from the list. Tap one to
/// read its transcript; swipe to unarchive. They can't be deleted (the
/// host's event log is append-only).
struct ArchivedSessionsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var search = ""
    @State private var error: String?

    private var sessions: [SessionInfo] {
        let all = model.archivedSessions ?? []
        let q = search.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return all }
        return all.filter {
            ($0.title ?? "").localizedCaseInsensitiveContains(q)
                || model.agentLabel($0).localizedCaseInsensitiveContains(q)
                || model.workspaceName($0.workspaceId).localizedCaseInsensitiveContains(q)
        }
    }

    var body: some View {
        List {
            if let error {
                Section {
                    Text(error).foregroundStyle(Palette.red)
                }
            }
            Section {
                ForEach(sessions) { s in
                    NavigationLink(value: Route.session(s.sessionId)) {
                        ArchivedSessionRow(session: s)
                    }
                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                        Button {
                            unarchive(s)
                        } label: {
                            Label("Unarchive", systemImage: "tray.and.arrow.up")
                        }
                        .tint(.accentColor)
                    }
                    .contextMenu {
                        Button {
                            unarchive(s)
                        } label: {
                            Label("Unarchive", systemImage: "tray.and.arrow.up")
                        }
                    }
                }
            } footer: {
                if !(model.archivedSessions ?? []).isEmpty && search.isEmpty {
                    Text("Archived sessions have ended. Unarchive one to show it in the list again.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .searchable(text: $search, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search archived")
        .navigationTitle("Archived Sessions")
        .navigationBarTitleDisplayMode(.inline)
        .overlay {
            if model.archivedSessions == nil {
                if model.link.isOnline {
                    ProgressView()
                } else {
                    ContentUnavailableView(
                        "Not Connected",
                        systemImage: "wifi.slash",
                        description: Text("Archived sessions load when the host is reachable.")
                    )
                }
            } else if model.archivedSessions?.isEmpty == true {
                ContentUnavailableView(
                    "No Archived Sessions",
                    systemImage: "archivebox",
                    description: Text("Swipe a session in the list to archive it.")
                )
            } else if sessions.isEmpty {
                ContentUnavailableView.search(text: search)
            }
        }
        .task { await model.refreshArchived() }
        .refreshable { await model.refreshArchived() }
    }

    private func unarchive(_ s: SessionInfo) {
        Task {
            do {
                try await model.unarchive(s.sessionId)
                error = nil
            } catch {
                self.error = userMessage(for: error)
            }
        }
    }
}

struct ArchivedSessionRow: View {
    @EnvironmentObject private var model: AppModel
    let session: SessionInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(model.sessionTitle(session))
                .lineLimit(1)
            Text(detail)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    /// "Codex · relay · Archived 12 Nov".
    private var detail: String {
        let place = model.isQuickTasks(session.workspaceId) ? "Quick task" : model.workspaceName(session.workspaceId)
        var parts = [model.agentLabel(session), place]
        if let at = session.archivedAt {
            parts.append("Archived \(dateFromMillis(at).formatted(.dateTime.day().month(.abbreviated)))")
        }
        return parts.joined(separator: " · ")
    }
}

extension View {
    /// Archive `candidate` when set, at once, live or ended (archiving a
    /// live session ends it). `onArchived` runs after the host confirmed.
    func archiveFlow(_ candidate: Binding<SessionInfo?>, onArchived: @escaping () -> Void = {}) -> some View {
        modifier(ArchiveFlow(candidate: candidate, onArchived: onArchived))
    }
}

private struct ArchiveFlow: ViewModifier {
    @EnvironmentObject private var model: AppModel
    @Binding var candidate: SessionInfo?
    var onArchived: () -> Void
    @State private var error: String?

    func body(content: Content) -> some View {
        content
            .onChange(of: candidate) { _, s in
                guard let s else { return }
                candidate = nil
                archive(s)
            }
            .alert("Couldn't Archive", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(error ?? "")
            }
    }

    private func archive(_ s: SessionInfo) {
        Task {
            do {
                try await model.archive(s.sessionId)
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                onArchived()
            } catch {
                self.error = userMessage(for: error)
            }
        }
    }
}
