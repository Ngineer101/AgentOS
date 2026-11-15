import SwiftUI

/// One pending MCP write call: connector, tool and arguments; approve or deny.
struct ApprovalView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let approvalId: String
    let initial: PendingApprovalModel

    @State private var sending = false
    @State private var error: String?

    /// Nil once answered here or on another device.
    private var current: PendingApprovalModel? {
        model.approvals.first { $0.approvalId == approvalId }
    }

    var body: some View {
        NavigationStack {
            List {
                Section("Connector") {
                    Text(initial.connectorName)
                }
                Section("Tool") {
                    Text(initial.tool).font(.body.monospaced())
                }
                Section("Arguments") {
                    CodeBlock(text: initial.arguments.isNull ? "(none)" : initial.arguments.prettyJSON)
                        .listRowInsets(EdgeInsets(top: 8, leading: 8, bottom: 8, trailing: 8))
                }
                Section("Session") {
                    if let s = model.session(initial.sessionId) {
                        Text("\(agentDisplayName(s.agent)) in \(model.workspaceName(s.workspaceId))")
                    } else {
                        Text(initial.sessionId).font(.caption.monospaced())
                    }
                    HStack {
                        Text("Expires")
                        Spacer()
                        Text(dateFromMillis(initial.expiresAt), style: .relative)
                            .foregroundStyle(.secondary)
                    }
                }
                if current == nil {
                    Section {
                        Label("This request was already answered or has expired.", systemImage: "checkmark.circle")
                            .foregroundStyle(.secondary)
                    }
                }
                if let error {
                    Section {
                        Text(error).foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle("Approve tool call?")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .safeAreaInset(edge: .bottom) {
                HStack(spacing: 12) {
                    Button(role: .destructive) {
                        answer(false)
                    } label: {
                        Text("Deny").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    Button {
                        answer(true)
                    } label: {
                        Text("Approve").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                }
                .controlSize(.large)
                .disabled(sending || current == nil || !model.link.isOnline)
                .padding()
                .background(.bar)
            }
        }
    }

    private func answer(_ approve: Bool) {
        sending = true
        error = nil
        Task {
            do {
                try await model.respond(approvalId, approve: approve)
                sending = false
                dismiss()
            } catch {
                sending = false
                self.error = userMessage(for: error)
                await model.refreshApprovals()
            }
        }
    }
}
