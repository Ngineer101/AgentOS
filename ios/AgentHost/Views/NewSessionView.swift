import SwiftUI

/// Start a session: Quick Task first, then the workspaces and "New
/// Project…", an agent login (a default login or a profile), and an optional
/// first message. A sheet with a prominent Start.
struct NewSessionView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    var onCreated: (SessionInfo) -> Void

    @State private var workspaceId: String?
    /// The chosen login (`AgentStatusInfo.id`).
    @State private var agent: String?
    @State private var firstMessage = ""
    @State private var starting = false
    @State private var pickingProject = false
    @State private var error: String?

    private var agents: [AgentStatusInfo] {
        model.hostInfo?.agents ?? []
    }

    /// Quick tasks first, then the most recently used, then by name.
    private var workspaces: [WorkspaceInfo] {
        var last: [String: Int64] = [:]
        for s in model.sessions { last[s.workspaceId] = max(last[s.workspaceId] ?? 0, s.updatedAt) }
        return model.workspaces.sorted {
            if $0.isQuickTasks != $1.isQuickTasks { return $0.isQuickTasks }
            let a = last[$0.workspaceId] ?? 0, b = last[$1.workspaceId] ?? 0
            if a != b { return a > b }
            return $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }

    private var canStart: Bool {
        workspaceId != nil && agent != nil && model.link.isOnline && !starting
    }

    var body: some View {
        NavigationStack {
            List {
                Section(caps: "Workspace") {
                    if model.workspaces.isEmpty {
                        Text("No workspaces yet. Add a project below, or on the computer.")
                            .foregroundStyle(Color.secondary)
                    }
                    ForEach(workspaces) { w in
                        Button {
                            workspaceId = w.workspaceId
                        } label: {
                            WorkspaceChoiceRow(workspace: w, selected: workspaceId == w.workspaceId)
                        }
                    }
                    Button {
                        pickingProject = true
                    } label: {
                        Label {
                            Text("New Project…")
                        } icon: {
                            Image(systemName: "folder.badge.plus")
                        }
                    }
                    .disabled(!model.link.isOnline)
                }

                Section(caps: "Agent") {
                    if agents.isEmpty {
                        Text("Agent status appears once connected.").foregroundStyle(Color.secondary)
                    }
                    ForEach(agents) { a in
                        Button {
                            if a.available { agent = a.id }
                        } label: {
                            AgentChoiceRow(agent: a, selected: agent == a.id)
                        }
                        .disabled(!a.available)
                    }
                }

                Section(caps: "First message") {
                    TextField("Optional — you can also type it after starting", text: $firstMessage, axis: .vertical)
                        .lineLimit(2...6)
                }

                if let error {
                    Section {
                        Text(error).foregroundStyle(Palette.red)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .sheet(isPresented: $pickingProject) {
                ProjectPickerView { w in workspaceId = w.workspaceId }
                    .environmentObject(model)
            }
            .navigationTitle("New Session")
            .navigationBarTitleDisplayMode(.inline)
            .modifier(NewSessionToolbar(starting: starting, canStart: canStart, cancel: { dismiss() }, start: start))
            .task {
                // Fresh agent availability and workspaces.
                await model.refreshAll()
                if workspaceId == nil {
                    workspaceId = workspaces.first(where: \.isQuickTasks)?.workspaceId
                        ?? (model.workspaces.count == 1 ? model.workspaces.first?.workspaceId : nil)
                }
                if agent == nil {
                    agent = agents.first(where: { $0.available })?.id
                }
                #if DEBUG
                if model.isDemo, DemoData.requestedScreen != nil {
                    workspaceId = "ws-relay"
                    if DemoData.requestedScreen == .picker { pickingProject = true }
                }
                #endif
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }

    private func start() {
        guard let wid = workspaceId, let key = agent,
              let login = agents.first(where: { $0.id == key }) else { return }
        starting = true
        error = nil
        let message = firstMessage
        Task {
            do {
                let s = try await model.createSession(
                    workspaceId: wid, agent: login.agent, profileId: login.profileId)
                starting = false
                if !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    model.sendPrompt(sessionId: s.sessionId, text: message)
                }
                onCreated(s)
            } catch {
                starting = false
                self.error = userMessage(for: error)
            }
        }
    }
}

/// Close (glass circle) and a prominent Start.
private struct NewSessionToolbar: ViewModifier {
    var starting: Bool
    var canStart: Bool
    var cancel: () -> Void
    var start: () -> Void

    func body(content: Content) -> some View {
        content.toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button(action: cancel) {
                    ToolbarIcon(systemName: "xmark")
                }
                .accessibilityLabel("Cancel")
            }
            ToolbarItem(placement: .confirmationAction) {
                if starting {
                    ProgressView()
                } else {
                    startButton
                }
            }
        }
    }

    @ViewBuilder
    private var startButton: some View {
        if #available(iOS 26, *) {
            Button("Start", action: start)
                .buttonStyle(.glassProminent)
                .fontWeight(.semibold)
                .disabled(!canStart)
        } else {
            Button(action: start) {
                Text("Start")
                    .fontWeight(.semibold)
                    .padding(.horizontal, 4)
            }
            .buttonStyle(.borderedProminent)
            .buttonBorderShape(.capsule)
            .disabled(!canStart)
        }
    }
}

struct WorkspaceChoiceRow: View {
    @EnvironmentObject private var model: AppModel
    let workspace: WorkspaceInfo
    let selected: Bool

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: workspace.isQuickTasks ? "bolt.fill" : "folder")
                .foregroundStyle(workspace.isQuickTasks ? Palette.orange : Color.accentColor)
                .frame(width: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text(workspace.isQuickTasks ? "Quick Task" : workspace.name)
                    .foregroundStyle(Color.primary)
                detail
                    .font(.footnote)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
            Spacer(minLength: 8)
            if selected {
                Image(systemName: "checkmark")
                    .font(.body.weight(.semibold))
                    .foregroundStyle(.tint)
            }
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    @ViewBuilder
    private var detail: some View {
        let live = model.liveSessionCount(workspace.workspaceId)
        if workspace.isQuickTasks {
            Text("No folder · for questions and lookups").foregroundStyle(Color.secondary)
        } else if live > 0 {
            Text("Busy · \(live) live \(live == 1 ? "session" : "sessions")").foregroundStyle(Palette.orange)
        } else {
            Text(abbreviatedPath(workspace.path)).foregroundStyle(Color.secondary)
        }
    }
}

struct AgentChoiceRow: View {
    let agent: AgentStatusInfo
    let selected: Bool

    var body: some View {
        HStack(spacing: 12) {
            AgentBadge(agent: agent.agent, dimmed: !agent.available)
            VStack(alignment: .leading, spacing: 2) {
                Text(agent.label)
                    .foregroundStyle(agent.available ? Color.primary : Color.secondary)
                if !agent.available {
                    Text(agent.problem ?? "Unavailable")
                        .font(.footnote)
                        .foregroundStyle(Color.secondary)
                } else if let dir = agent.configDir {
                    Text(dir)
                        .font(.footnote)
                        .foregroundStyle(Color.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
            Spacer(minLength: 8)
            if selected {
                Image(systemName: "checkmark")
                    .font(.body.weight(.semibold))
                    .foregroundStyle(.tint)
            }
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}
