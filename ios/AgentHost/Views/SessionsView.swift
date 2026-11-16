import SwiftUI

enum Route: Hashable {
    case session(String)
    case host
}

/// Sessions grouped by workspace, with agent, state and last activity.
struct SessionsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var path: [Route] = []
    @State private var showNew = false
    @State private var approvalShown: PendingApprovalModel?
    @State private var showApprovalList = false

    var body: some View {
        NavigationStack(path: $path) {
            List {
                if !model.link.isOnline {
                    Section {
                        ConnectionStatusRow()
                    }
                }
                if model.anyWaitingApproval || !model.approvals.isEmpty {
                    Section {
                        ApprovalStatusRow(sessionId: nil) { first in
                            openApprovals(first)
                        }
                    }
                }

                ForEach(model.groupedSessions) { group in
                    Section(group.workspaceName) {
                        ForEach(group.sessions) { s in
                            NavigationLink(value: Route.session(s.sessionId)) {
                                SessionRow(session: s)
                            }
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .overlay {
                if model.sessions.isEmpty {
                    ContentUnavailableView {
                        Label("No Sessions", systemImage: "rectangle.stack")
                    } description: {
                        Text("Start one in an existing workspace with the + button.")
                    }
                }
            }
            .refreshable {
                if model.link.isOnline {
                    await model.refreshAll()
                } else {
                    model.retryNow()
                }
            }
            .navigationTitle(model.credentials?.hostName ?? "Sessions")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        path.append(.host)
                    } label: {
                        Image(systemName: model.link.isOnline ? "desktopcomputer" : "desktopcomputer.trianglebadge.exclamationmark")
                    }
                    .accessibilityLabel("Host")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showNew = true
                    } label: {
                        Image(systemName: "plus")
                    }
                    .disabled(!model.link.isOnline)
                    .accessibilityLabel("New session")
                }
            }
            .navigationDestination(for: Route.self) { route in
                switch route {
                case let .session(id):
                    SessionView(sessionId: id)
                case .host:
                    HostView()
                }
            }
            .sheet(isPresented: $showNew) {
                NewSessionView { created in
                    showNew = false
                    path.append(.session(created.sessionId))
                }
                .environmentObject(model)
            }
            .sheet(item: $approvalShown) { a in
                ApprovalView(approvalId: a.approvalId, initial: a)
                .environmentObject(model)
            }
            .sheet(isPresented: $showApprovalList) {
                ApprovalListView()
                    .environmentObject(model)
            }
        }
    }

    private func openApprovals(_ first: PendingApprovalModel?) {
        if model.approvals.count == 1, let a = first {
            approvalShown = a
        } else {
            showApprovalList = true
            Task { await model.refreshApprovals() }
        }
    }
}

struct SessionRow: View {
    @EnvironmentObject private var model: AppModel
    var session: SessionInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline) {
                Text(session.title ?? agentDisplayName(session.agent))
                    .lineLimit(1)
                Spacer(minLength: 8)
                StateText(state: session.state)
            }
            Text("\(agentDisplayName(session.agent)) · \(dateFromMillis(session.updatedAt).formatted(.relative(presentation: .named)))")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            if let reason = session.stateReason, session.state == SessionStateName.failed {
                Text(reason)
                    .font(.subheadline)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
        }
        .padding(.vertical, 2)
    }
}

/// All pending approvals (when more than one is waiting).
struct ApprovalListView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var selected: PendingApprovalModel?

    var body: some View {
        NavigationStack {
            List {
                ForEach(model.approvals) { a in
                    Button {
                        selected = a
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(a.connectorName): \(a.tool)")
                                .font(.body.weight(.medium))
                                .foregroundStyle(.primary)
                            Text(agentLabel(a)).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .overlay {
                if model.approvals.isEmpty {
                    ContentUnavailableView(
                        "Nothing Waiting",
                        systemImage: "checkmark.circle",
                        description: Text("No tool calls are waiting for approval.")
                    )
                }
            }
            .sheet(item: $selected) { a in
                ApprovalView(approvalId: a.approvalId, initial: a)
                .environmentObject(model)
            }
            .navigationTitle("Approvals")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func agentLabel(_ a: PendingApprovalModel) -> String {
        guard let s = model.session(a.sessionId) else { return "Session \(a.sessionId)" }
        return "\(agentDisplayName(s.agent)) in \(model.workspaceName(s.workspaceId))"
    }
}
