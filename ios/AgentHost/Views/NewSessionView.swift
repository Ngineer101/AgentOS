import SwiftUI

/// Start a session in an existing workspace with an available agent.
struct NewSessionView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    var onCreated: (SessionInfo) -> Void

    @State private var workspaceId: String?
    @State private var agent: String?
    @State private var starting = false
    @State private var error: String?

    private var agents: [AgentStatusInfo] {
        model.hostInfo?.agents ?? []
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Workspace") {
                    if model.workspaces.isEmpty {
                        Text("No workspaces. Add one on the desktop (`agenthost workspace add`).")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(model.workspaces) { w in
                        Button {
                            workspaceId = w.workspaceId
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(w.name).foregroundStyle(.primary)
                                    Text(w.path)
                                        .font(.caption2.monospaced())
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                        .truncationMode(.middle)
                                }
                                Spacer()
                                if workspaceId == w.workspaceId {
                                    Image(systemName: "checkmark").foregroundStyle(.tint)
                                }
                            }
                        }
                    }
                }

                Section("Agent") {
                    if agents.isEmpty {
                        Text("Agent status unavailable.").foregroundStyle(.secondary)
                    }
                    ForEach(agents) { a in
                        Button {
                            if a.available { agent = a.agent }
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(a.displayName)
                                        .foregroundStyle(a.available ? Color.primary : Color.secondary)
                                    if !a.available {
                                        Text(a.problem ?? "Unavailable")
                                            .font(.caption)
                                            .foregroundStyle(.orange)
                                        if let fix = a.fix {
                                            Text("Fix on the host: \(fix)")
                                                .font(.caption2.monospaced())
                                                .foregroundStyle(.secondary)
                                        }
                                    } else if let v = a.version {
                                        Text(v).font(.caption2).foregroundStyle(.secondary)
                                    }
                                }
                                Spacer()
                                if agent == a.agent {
                                    Image(systemName: "checkmark").foregroundStyle(.tint)
                                }
                            }
                        }
                        .disabled(!a.available)
                    }
                }

                if let error {
                    Section {
                        Text(error).foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if starting {
                        ProgressView()
                    } else {
                        Button("Start") { start() }
                            .disabled(workspaceId == nil || agent == nil || !model.link.isOnline)
                    }
                }
            }
            .task {
                // Fresh agent availability and workspaces.
                await model.refreshAll()
                if workspaceId == nil, model.workspaces.count == 1 {
                    workspaceId = model.workspaces.first?.workspaceId
                }
                if agent == nil {
                    agent = agents.first(where: { $0.available })?.agent
                }
            }
        }
    }

    private func start() {
        guard let wid = workspaceId, let ag = agent else { return }
        starting = true
        error = nil
        Task {
            do {
                let s = try await model.createSession(workspaceId: wid, agent: ag)
                starting = false
                onCreated(s)
            } catch {
                starting = false
                self.error = userMessage(for: error)
            }
        }
    }
}
