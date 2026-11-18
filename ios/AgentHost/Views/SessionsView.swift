import SwiftUI

enum Route: Hashable {
    case session(String)
    case settings
    case archived
}

/// Sessions grouped by workspace (Quick tasks first) under a large title with
/// the host's name; Search and Compose in glass at the bottom, Settings at
/// the top right.
struct SessionsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var path: [Route] = []
    @State private var showNew = false
    @State private var approvalShown: PendingApprovalModel?
    @State private var showApprovalList = false
    @State private var search = ""
    /// Asking before archiving a live session.
    @State private var archiving: SessionInfo?

    private var groups: [SessionGroup] {
        let q = search.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return model.groupedSessions }
        return model.groupedSessions.compactMap { g in
            let hits = g.sessions.filter { s in
                (s.title ?? "").localizedCaseInsensitiveContains(q)
                    || model.agentLabel(s).localizedCaseInsensitiveContains(q)
                    || g.workspaceName.localizedCaseInsensitiveContains(q)
            }
            return hits.isEmpty ? nil : SessionGroup(workspaceId: g.workspaceId, workspaceName: g.workspaceName, sessions: hits)
        }
    }

    var body: some View {
        NavigationStack(path: $path) {
            List {
                Section {
                    HostStatusLine()
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets(top: 0, leading: 4, bottom: 0, trailing: 4))
                        .frame(height: 20)
                }
                .listSectionSpacing(14)

                if !model.link.isOnline {
                    Section {
                        ConnectionStatusRow()
                    }
                }
                if model.anyWaitingApproval && search.isEmpty {
                    Section {
                        ApprovalBanner { first in openApprovals(first) }
                            .listRowBackground(
                                RoundedRectangle(cornerRadius: 24, style: .continuous)
                                    .fill(Palette.orange.opacity(0.16))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 24, style: .continuous)
                                            .strokeBorder(Palette.orange.opacity(0.35), lineWidth: 0.5)
                                    )
                            )
                    }
                }

                ForEach(groups) { group in
                    Section {
                        ForEach(group.sessions) { s in
                            NavigationLink(value: Route.session(s.sessionId)) {
                                SessionRow(session: s)
                            }
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button {
                                    archiving = s
                                } label: {
                                    Label("Archive", systemImage: "archivebox")
                                }
                                .tint(.indigo)
                            }
                            .contextMenu {
                                Button {
                                    archiving = s
                                } label: {
                                    Label("Archive", systemImage: "archivebox")
                                }
                            }
                        }
                    } header: {
                        GroupHeader(model.isQuickTasks(group.workspaceId) ? "Quick tasks" : group.workspaceName)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .environment(\.defaultMinListRowHeight, 20)
            .contentMargins(.top, 0, for: .scrollContent)
            .overlay {
                if model.sessions.isEmpty {
                    ContentUnavailableView {
                        Label("No Sessions", systemImage: "rectangle.stack")
                    } description: {
                        Text("Start one with the compose button.")
                    }
                } else if groups.isEmpty {
                    ContentUnavailableView.search(text: search)
                }
            }
            .refreshable {
                if model.link.isOnline {
                    await model.refreshAll()
                } else {
                    model.retryNow()
                }
            }
            .navigationTitle(model.hostName)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        path.append(.settings)
                    } label: {
                        ToolbarIcon(systemName: "gearshape")
                    }
                    .accessibilityLabel("Settings")
                }
            }
            .modifier(SessionsBottomBar(search: $search, canCompose: model.link.isOnline) { showNew = true })
            .navigationDestination(for: Route.self) { route in
                switch route {
                case let .session(id):
                    SessionView(sessionId: id)
                case .settings:
                    SettingsView()
                case .archived:
                    ArchivedSessionsView()
                }
            }
            .archiveFlow($archiving)
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
        #if DEBUG
        .task { openDemoScreen() }
        #endif
    }

    private func openApprovals(_ first: PendingApprovalModel?) {
        if model.approvals.count == 1, let a = first {
            approvalShown = a
        } else {
            showApprovalList = true
            Task { await model.refreshApprovals() }
        }
    }

    #if DEBUG
    private func openDemoScreen() {
        guard model.isDemo, path.isEmpty, let screen = DemoData.requestedScreen else { return }
        switch screen {
        case .session: path = [.session(DemoData.flakySession)]
        case .dictating: path = [.session(DemoData.quickSession)]
        case .settings: path = [.settings]
        case .new, .newLogin, .picker: showNew = true
        case .archived: path = [.settings, .archived]
        case .archivedSession: path = [.settings, .archived, .session(DemoData.archivedSession)]
        case .archiveConfirm: archiving = model.session(DemoData.flakySession)
        case .side, .config, .paste, .quick, .btw: path = [.session(DemoData.quickSession)]
        case .plain: path = [.session(DemoData.plainSession)]
        case .subagents: path = [.session(DemoData.subagentSession)]
        case .sessions, .pair: break
        }
    }
    #endif
}

/// Search and Compose at the bottom: the system search field in the bottom
/// glass toolbar on iOS 26; a material capsule and a tinted circle before.
private struct SessionsBottomBar: ViewModifier {
    @Binding var search: String
    var canCompose: Bool
    var compose: () -> Void

    func body(content: Content) -> some View {
        if #available(iOS 26, *) {
            content
                .searchable(text: $search, prompt: "Search")
                .toolbar {
                    DefaultToolbarItem(kind: .search, placement: .bottomBar)
                    ToolbarSpacer(.fixed, placement: .bottomBar)
                    ToolbarItem(placement: .bottomBar) {
                        Button(action: compose) {
                            Image(systemName: "square.and.pencil")
                        }
                        .buttonStyle(.glassProminent)
                        .disabled(!canCompose)
                        .accessibilityLabel("New session")
                    }
                }
        } else {
            content
                .safeAreaInset(edge: .bottom) {
                    HStack(spacing: 10) {
                        HStack(spacing: 8) {
                            Image(systemName: "magnifyingglass")
                                .foregroundStyle(.secondary)
                            TextField("Search", text: $search)
                                .submitLabel(.search)
                            if !search.isEmpty {
                                Button {
                                    search = ""
                                } label: {
                                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                                }
                                .accessibilityLabel("Clear search")
                            }
                        }
                        .padding(.horizontal, 16)
                        .frame(height: 48)
                        .glassSurface(Capsule())
                        Button(action: compose) {
                            Image(systemName: "square.and.pencil")
                                .foregroundStyle(.white)
                        }
                        .buttonStyle(GlassCircleButtonStyle(size: 48, tint: .accentColor))
                        .disabled(!canCompose)
                        .accessibilityLabel("New session")
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 4)
                }
        }
    }
}

/// "● Connected · 4 sessions" under the large title.
struct HostStatusLine: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        let line = Line(model: model)
        HStack(spacing: 6) {
            Circle().fill(line.color).frame(width: 8, height: 8)
            Text(line.text)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .accessibilityElement(children: .combine)
    }

    @MainActor
    struct Line {
        let model: AppModel

    var color: Color {
        switch model.link {
        case .online: return Palette.green
        case .connecting: return .blue
        case .updateRequired: return Palette.red
        default: return Palette.orange
        }
    }

    var text: String {
        let live = model.sessions.filter { !SessionStateName.isTerminal($0.state) }.count
        switch model.link {
        case .online:
            return "Connected · \(live) \(live == 1 ? "session" : "sessions")"
        default:
            if let at = model.cachedAt {
                return "\(model.link.label) · cached \(at.formatted(.relative(presentation: .named)))"
            }
            return model.link.label
        }
    }
    }
}

/// A grouped-list section header in the classic style: small capitals.
struct GroupHeader: View {
    let title: String
    init(_ title: String) { self.title = title }

    var body: some View {
        Text(title)
            .font(.footnote)
            .textCase(.uppercase)
            .foregroundStyle(.secondary)
    }
}

extension Section where Parent == GroupHeader, Footer == EmptyView, Content: View {
    /// A section with a small-capitals header.
    init(caps title: String, @ViewBuilder content: () -> Content) {
        self.init(content: content, header: { GroupHeader(title) })
    }
}

/// The prominent "N approvals waiting" row at the top of the list.
struct ApprovalBanner: View {
    @EnvironmentObject private var model: AppModel
    var onTap: (PendingApprovalModel?) -> Void

    var body: some View {
        Button {
            onTap(model.approvals.first)
        } label: {
            HStack(spacing: 12) {
                Image(systemName: "bell.fill")
                    .font(.system(size: 15))
                    .foregroundStyle(.black)
                    .frame(width: 34, height: 34)
                    .background(Palette.orange, in: Circle())
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.primary)
                    Group {
                        if let a = model.approvals.first {
                            Text(a.listLine)
                        } else {
                            Text("A session is waiting for a decision")
                        }
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                }
                Spacer(minLength: 4)
                Text("Review")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.black)
                    .padding(.horizontal, 16)
                    .frame(height: 34)
                    .background(Palette.orange, in: Capsule())
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Opens the approval")
    }

    private var title: String {
        let n = model.approvals.count
        return n > 1 ? "\(n) approvals waiting" : "1 approval waiting"
    }
}

struct SessionRow: View {
    @EnvironmentObject private var model: AppModel
    var session: SessionInfo

    private var status: SessionStatus { SessionStatus(state: session.state) }
    private var ended: Bool { status == .ended }

    var body: some View {
        HStack(spacing: 12) {
            SessionStatusIcon(state: session.state)
            VStack(alignment: .leading, spacing: 1) {
                Text(session.title ?? model.agentLabel(session))
                    .foregroundStyle(ended ? Color.secondary : Color.primary)
                    .lineLimit(1)
                Text(subtitle)
                    .font(.footnote)
                    .foregroundStyle(subtitleColor)
                    .lineLimit(1)
                if let branch = session.branch {
                    Label(branch, systemImage: "arrow.triangle.branch")
                        .labelStyle(BranchLabelStyle())
                        .foregroundStyle(ended ? Color(uiColor: .tertiaryLabel) : .secondary)
                        .accessibilityLabel("Branch \(branch)")
                }
                if let reason = session.stateReason, session.state == SessionStateName.failed {
                    Text(reason)
                        .font(.footnote)
                        .foregroundStyle(Palette.red)
                        .lineLimit(2)
                }
            }
            .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    /// "Your turn · Claude Code · Work · 3m" (no age while it needs you).
    private var subtitle: String {
        let agent = model.agentLabel(session)
        if status == .needsYou { return "\(status.label) · \(agent)" }
        // "Working · 2 agents running" while subagents run.
        if status == .working {
            let n = runningSubagentCount(model.transcripts[session.sessionId]?.events ?? [])
            if n > 0 { return "\(status.label) · \(n == 1 ? "1 agent" : "\(n) agents") running" }
        }
        return "\(status.label) · \(agent) · \(shortAge(session.updatedAt))"
    }

    private var subtitleColor: Color {
        switch status {
        case .needsYou, .failed: return status.color
        case .ended: return Color(uiColor: .tertiaryLabel)
        case .working, .yourTurn: return .secondary
        }
    }
}

/// A branch name under a session: a small branch glyph and the name in
/// monospaced footnote, truncated in the middle.
struct BranchLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 4) {
            configuration.icon
                .font(.caption2.weight(.semibold))
            configuration.title
                .font(.footnote.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
        }
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
                        HStack(spacing: 12) {
                            if a.isInstall {
                                Image(systemName: "terminal")
                                    .font(.system(size: 14, weight: .semibold))
                                    .foregroundStyle(Palette.orange)
                                    .frame(width: 30, height: 30)
                                    .background(Palette.orange.opacity(0.18), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                                    .accessibilityHidden(true)
                            } else {
                                ConnectorIcon(id: a.connectorId, name: a.connectorName, size: 30)
                            }
                            VStack(alignment: .leading, spacing: 2) {
                                Text(a.listLine)
                                    .font(.body.weight(.medium))
                                    .foregroundStyle(.primary)
                                Text(agentLabel(a)).font(.caption).foregroundStyle(.secondary)
                            }
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
        return "\(model.agentLabel(s)) in \(model.workspaceName(s.workspaceId))"
    }
}
