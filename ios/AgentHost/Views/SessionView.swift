import SwiftUI

/// The live transcript of one session, laid out for a narrow screen.
struct SessionView: View {
    @EnvironmentObject private var model: AppModel
    let sessionId: String

    @State private var draft = ""
    @State private var actionError: String?
    @State private var confirmEnd = false
    @State private var approvalShown: PendingApprovalModel?
    @FocusState private var promptFocused: Bool

    private var session: SessionInfo? { model.session(sessionId) }
    private var rows: [TranscriptRow] { buildRows(model.transcripts[sessionId]?.events ?? []) }
    private var pending: [OutgoingPrompt] { model.outbox.filter { $0.sessionId == sessionId } }
    private var isTerminal: Bool { SessionStateName.isTerminal(session?.state ?? "") }
    private var canPrompt: Bool { model.link.isOnline && !isTerminal }
    private var approvalWaiting: Bool {
        model.approvals.contains { $0.sessionId == sessionId }
            || session?.state == SessionStateName.waitingApproval
    }

    var body: some View {
        VStack(spacing: 0) {
            if !model.link.isOnline || approvalWaiting {
                VStack(spacing: 8) {
                    if !model.link.isOnline {
                        ConnectionStatusRow().statusCard()
                    }
                    if approvalWaiting {
                        ApprovalStatusRow(sessionId: sessionId) { first in
                            if let a = first { approvalShown = a } else {
                                Task { await model.refreshApprovals() }
                            }
                        }
                        .statusCard()
                    }
                }
                .padding(.horizontal)
                .padding(.top, 8)
            }

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        if rows.isEmpty && pending.isEmpty {
                            Group {
                                if model.link.isOnline {
                                    ProgressView()
                                } else {
                                    ContentUnavailableView(
                                        "No Cached Transcript",
                                        systemImage: "text.bubble",
                                        description: Text("It will load when the host is reachable.")
                                    )
                                }
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.top, 40)
                        }
                        ForEach(rows) { row in
                            TranscriptRowView(row: row) { a in approvalShown = a }
                        }
                        if let s = model.streaming[sessionId] {
                            StreamingRow(streaming: s)
                        }
                        ForEach(pending) { p in
                            PendingPromptRow(prompt: p)
                        }
                        Color.clear.frame(height: 1).id("bottom")
                    }
                    .padding()
                }
                .scrollDismissesKeyboard(.interactively)
                .onChange(of: rows.count) { _, _ in
                    withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
                }
                .onChange(of: model.streaming[sessionId]?.text) { _, _ in
                    proxy.scrollTo("bottom", anchor: .bottom)
                }
                .onAppear { proxy.scrollTo("bottom", anchor: .bottom) }
            }

            if let err = actionError {
                Text(err)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(.horizontal)
                    .padding(.top, 4)
            }
            promptBar
        }
        .navigationTitle(session.map { agentDisplayName($0.agent) } ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 0) {
                    Text(session.map { agentDisplayName($0.agent) } ?? "Session")
                        .font(.headline)
                    if let s = session {
                        Text("\(model.workspaceName(s.workspaceId)) · \(SessionStateName.label(s.state))")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task { await run { try await model.cancel(sessionId) } }
                    } label: {
                        Label("Cancel turn", systemImage: "stop.circle")
                    }
                    .disabled(!model.link.isOnline || !SessionStateName.isBusy(session?.state ?? ""))
                    Button(role: .destructive) {
                        confirmEnd = true
                    } label: {
                        Label("End session", systemImage: "xmark.circle")
                    }
                    .disabled(!model.link.isOnline || isTerminal)
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .confirmationDialog("End this session?", isPresented: $confirmEnd, titleVisibility: .visible) {
            Button("End session", role: .destructive) {
                Task { await run { try await model.end(sessionId) } }
            }
        } message: {
            Text("The agent stops. The transcript stays available.")
        }
        .sheet(item: $approvalShown) { a in
            ApprovalView(approvalId: a.approvalId, initial: a)
                .environmentObject(model)
        }
        .onAppear { model.open(sessionId) }
    }

    private var promptBar: some View {
        HStack(alignment: .bottom, spacing: 8) {
            // Messages-style field: a rounded capsule on the bar.
            TextField(placeholder, text: $draft, axis: .vertical)
                .lineLimit(1...6)
                .focused($promptFocused)
                .disabled(!canPrompt)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .background(Color(uiColor: .secondarySystemBackground),
                            in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 18, style: .continuous)
                        .strokeBorder(Color(uiColor: .separator), lineWidth: 0.5)
                )
            if SessionStateName.isBusy(session?.state ?? "") && draft.isEmpty {
                Button {
                    Task { await run { try await model.cancel(sessionId) } }
                } label: {
                    Image(systemName: "stop.circle.fill").font(.title)
                }
                .disabled(!model.link.isOnline)
                .accessibilityLabel("Cancel turn")
            } else {
                Button {
                    model.sendPrompt(sessionId: sessionId, text: draft)
                    draft = ""
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title)
                }
                .disabled(!canPrompt || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityLabel("Send prompt")
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var placeholder: String {
        if isTerminal { return "This session has ended" }
        switch model.link {
        case .online: return "Prompt"
        case .hostOffline: return "Host offline — prompts disabled"
        default: return "Offline — prompts disabled"
        }
    }

    private func run(_ op: @escaping () async throws -> Void) async {
        do {
            try await op()
            actionError = nil
        } catch {
            actionError = userMessage(for: error)
        }
    }
}

// MARK: - Rows

struct TranscriptRowView: View {
    let row: TranscriptRow
    var onApproval: (PendingApprovalModel) -> Void

    var body: some View {
        switch row {
        case let .prompt(_, text, device, ts):
            VStack(alignment: .trailing, spacing: 2) {
                Text(text)
                    .textSelection(.enabled)
                    .foregroundStyle(.white)
                    .padding(.horizontal, 13)
                    .padding(.vertical, 9)
                    .background(Color.accentColor, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                HStack(spacing: 4) {
                    if let d = device { Text(d) }
                    Text(dateFromMillis(ts), style: .time)
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
            .padding(.leading, 48)
            .frame(maxWidth: .infinity, alignment: .trailing)

        case let .agentMessage(_, text):
            MarkdownText(text: text)
                .frame(maxWidth: .infinity, alignment: .leading)

        case let .thought(_, text):
            ThoughtRow(text: text)

        case let .tool(t):
            ToolCallRow(tool: t)

        case let .approvalRequested(_, approval, connector, tool):
            ApprovalRequestRow(approval: approval, connector: connector, tool: tool, onOpen: onApproval)

        case let .approvalResolved(_, decision, device, timedOut):
            Label(
                timedOut ? "Approval timed out (denied)" :
                    "\(decision == "approve" ? "Approved" : "Denied")\(device.isEmpty ? "" : " by \(device)")",
                systemImage: decision == "approve" ? "checkmark.seal" : "xmark.seal"
            )
            .font(.caption)
            .foregroundStyle(decision == "approve" ? Color.green : Color.red)

        case let .netDenied(_, host, port, reason):
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "network.slash")
                    .foregroundStyle(.red)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Blocked: \(host)\(port.map { ":\($0)" } ?? "")")
                        .font(.caption.bold().monospaced())
                    if !reason.isEmpty {
                        Text(reason).font(.caption2).foregroundStyle(.secondary)
                    }
                    Text("Add the host to the workspace's profile on the desktop to allow it.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            .transcriptCard()

        case let .state(_, state, reason, fix):
            if state == SessionStateName.failed {
                VStack(alignment: .leading, spacing: 4) {
                    Label("Session failed\(reason.map { ": \($0)" } ?? "")", systemImage: "exclamationmark.octagon")
                        .font(.caption.bold())
                        .foregroundStyle(.red)
                    if let fix {
                        Text("Fix on the host: \(fix)")
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                    }
                }
            } else if state == SessionStateName.ended || state == SessionStateName.created {
                Text(state == SessionStateName.ended ? "Session ended" : "Session created")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
            } else {
                EmptyView()
            }

        case let .turnEnd(_, stop, detail):
            HStack {
                VStack { Divider() }
                if stop != "end_turn" {
                    Text(turnEndLabel(stop, detail))
                        .font(.caption2)
                        .foregroundStyle(stop == "error" ? Color.red : Color.secondary)
                }
                VStack { Divider() }
            }
        }
    }

    private func turnEndLabel(_ stop: String, _ detail: String?) -> String {
        let base: String
        switch stop {
        case "cancelled": base = "Cancelled"
        case "interrupted": base = "Interrupted"
        case "error": base = "Turn failed"
        case "max_tokens": base = "Stopped: token limit"
        case "refusal": base = "Agent refused"
        default: base = stop
        }
        if let d = detail, !d.isEmpty { return "\(base): \(d)" }
        return base
    }
}

/// Agent text, with inline Markdown (bold, code, links) where it parses.
struct MarkdownText: View {
    let text: String

    var body: some View {
        if let attributed = try? AttributedString(
            markdown: text,
            options: AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) {
            Text(attributed).textSelection(.enabled)
        } else {
            Text(text).textSelection(.enabled)
        }
    }
}

struct ThoughtRow: View {
    let text: String
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Label("Thinking", systemImage: "brain")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

/// A tool call: collapsed to name and status; expands to arguments and output.
struct ToolCallRow: View {
    let tool: ToolRow
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 8) {
                if !tool.arguments.isNull {
                    Text("Arguments").font(.caption2.bold()).foregroundStyle(.secondary)
                    CodeBlock(text: tool.arguments.displayText)
                }
                if let out = tool.output, !out.isEmpty {
                    Text("Output\(tool.exitCode.map { " (exit \($0))" } ?? "")")
                        .font(.caption2.bold())
                        .foregroundStyle(.secondary)
                    CodeBlock(text: out + (tool.truncated ? "\n… (truncated)" : ""))
                }
                if tool.sandboxDenied {
                    Label("Blocked by the sandbox", systemImage: "lock.trianglebadge.exclamationmark")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
            .padding(.top, 4)
        } label: {
            HStack(spacing: 8) {
                statusIcon
                VStack(alignment: .leading, spacing: 1) {
                    Text(tool.name)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                    if let c = tool.connectorName {
                        Text(c).font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .transcriptCard()
    }

    @ViewBuilder
    private var statusIcon: some View {
        switch tool.status {
        case nil:
            ProgressView().controlSize(.mini)
        case "completed"?:
            Image(systemName: "checkmark.circle").foregroundStyle(.green)
        case "denied"?:
            Image(systemName: "hand.raised.slash").foregroundStyle(.orange)
        default:
            Image(systemName: tool.sandboxDenied ? "lock.trianglebadge.exclamationmark" : "xmark.circle")
                .foregroundStyle(.red)
        }
    }
}

struct CodeBlock: View {
    let text: String

    var body: some View {
        ScrollView(.horizontal) {
            Text(text)
                .font(.caption2.monospaced())
                .textSelection(.enabled)
                .padding(8)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .tertiarySystemBackground),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }
}

extension View {
    /// The grouped background used for tool calls, blocked hosts and
    /// approval requests in a transcript.
    func transcriptCard() -> some View {
        padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(uiColor: .secondarySystemBackground),
                        in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

struct ApprovalRequestRow: View {
    @EnvironmentObject private var model: AppModel
    let approval: PendingApprovalModel?
    let connector: String
    let tool: String
    var onOpen: (PendingApprovalModel) -> Void

    private var stillPending: PendingApprovalModel? {
        guard let a = approval else { return nil }
        return model.approvals.first { $0.approvalId == a.approvalId }
    }

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "hand.raised.fill")
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 2) {
                Text("Approval requested").font(.footnote.weight(.semibold))
                Text("\(connector): \(tool)")
                    .font(.footnote.monospaced())
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            if let a = stillPending {
                Button("Review") { onOpen(a) }
                    .buttonStyle(.borderedProminent)
                    .buttonBorderShape(.capsule)
                    .controlSize(.small)
            }
        }
        .transcriptCard()
    }
}

struct StreamingRow: View {
    let streaming: StreamingText

    var body: some View {
        if streaming.kind == EventKind.agentThought {
            Label("Thinking…", systemImage: "brain")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            Text(streaming.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .opacity(0.85)
        }
    }
}

struct PendingPromptRow: View {
    @EnvironmentObject private var model: AppModel
    let prompt: OutgoingPrompt

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            // Not yet accepted by the host: the same bubble, dimmed.
            Text(prompt.text)
                .foregroundStyle(.white)
                .padding(.horizontal, 13)
                .padding(.vertical, 9)
                .background(Color.accentColor.opacity(0.5), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            if let f = prompt.failure {
                HStack {
                    Text(f).font(.caption2).foregroundStyle(.red)
                    Button("Retry") { model.retryPrompt(prompt.id) }.font(.caption2)
                    Button("Discard", role: .destructive) { model.discardPrompt(prompt.id) }.font(.caption2)
                }
            } else {
                Label(model.link.isOnline ? "Sending…" : "Waiting for connection", systemImage: "clock")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.leading, 48)
        .frame(maxWidth: .infinity, alignment: .trailing)
    }
}
