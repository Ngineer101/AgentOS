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
                if initial.isInstall {
                    Section(caps: "Install") {
                        Text(initial.summary ?? "A tool on this computer")
                    }
                    if let reason = initial.reason {
                        Section(caps: "Why") {
                            Text(reason)
                        }
                    }
                    Section {
                        CodeBlock(text: "$ " + (initial.command ?? "(no command)"))
                            .listRowInsets(EdgeInsets(top: 8, leading: 8, bottom: 8, trailing: 8))
                    } header: {
                        Text("RUNS")
                    } footer: {
                        Text("On this computer, as you, outside the sandbox. The package manager may run the package's install scripts with your permissions.")
                    }
                } else {
                    Section(caps: "Connector") {
                        HStack(spacing: 12) {
                            ConnectorIcon(id: initial.connectorId, name: initial.connectorName, size: 28)
                            Text(initial.connectorName)
                        }
                    }
                    Section(caps: "Tool") {
                        Text(initial.tool).font(.body.monospaced())
                    }
                    Section(caps: "Arguments") {
                        CodeBlock(text: initial.arguments.isNull ? "(none)" : initial.arguments.prettyJSON)
                            .listRowInsets(EdgeInsets(top: 8, leading: 8, bottom: 8, trailing: 8))
                    }
                }
                Section(caps: "Session") {
                    if let s = model.session(initial.sessionId) {
                        Text("\(model.agentLabel(s)) in \(model.workspaceName(s.workspaceId))")
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
            .navigationTitle(initial.isInstall ? "Install a Tool?" : "\(ApprovalRequestRow.humanize(initial.tool)) on \(initial.connectorName)?")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button {
                        dismiss()
                    } label: {
                        ToolbarIcon(systemName: "xmark")
                    }
                    .accessibilityLabel("Close")
                }
            }
            .safeAreaInset(edge: .bottom) {
                HStack(spacing: 10) {
                    Button("Deny") { answer(false) }
                        .buttonStyle(CapsuleButtonStyle(fill: Color.white.opacity(0.1), foreground: Palette.red, height: 50))
                    Button(initial.isInstall ? "Install" : "Approve") { answer(true) }
                        .buttonStyle(CapsuleButtonStyle(fill: .accentColor, height: 50))
                }
                .disabled(sending || current == nil || !model.link.isOnline)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
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
