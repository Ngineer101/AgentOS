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
                if model.sessions.isEmpty {
                    Section {
                        VStack(spacing: 8) {
                            Image(systemName: "rectangle.stack")
                                .font(.largeTitle)
                                .foregroundStyle(.secondary)
                            Text("No sessions yet")
                                .font(.headline)
                            Text("Start one in an existing workspace.")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 24)
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
            .safeAreaInset(edge: .top) {
                if !model.link.isOnline || model.anyWaitingApproval {
                    VStack(spacing: 6) {
                        ConnectionBanner()
                        ApprovalBanner(sessionId: nil) { first in
                            openApprovals(first)
                        }
                    }
                    .padding(.vertical, 6)
                    .background(.bar)
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
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(session.title ?? agentDisplayName(session.agent))
                    .font(.body.weight(.medium))
                    .lineLimit(1)
                Spacer()
                StateBadge(state: session.state)
            }
            HStack(spacing: 6) {
                Text(agentDisplayName(session.agent))
                Text("·")
                Text(dateFromMillis(session.updatedAt), style: .relative)
                Text("ago")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            if let reason = session.stateReason, session.state == SessionStateName.failed {
                Text(reason)
                    .font(.caption)
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
                if model.approvals.isEmpty {
                    Text("Nothing is waiting for approval.")
                        .foregroundStyle(.secondary)
                }
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
