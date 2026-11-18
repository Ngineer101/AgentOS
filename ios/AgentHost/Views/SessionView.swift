import AVFoundation
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// The live transcript of one session, laid out for a narrow screen: glass
/// back and menu buttons, the title with "agent · login · workspace" under
/// it, and a glass composer at the bottom.
struct SessionView: View {
    @EnvironmentObject private var model: AppModel
    let sessionId: String

    @State private var draft = ""
    @State private var actionError: String?
    @State private var confirmEnd = false
    @State private var approvalShown: PendingApprovalModel?
    @FocusState private var promptFocused: Bool
    @Environment(\.openURL) private var openURL

    // Attachments (protocol 2 hosts) and voice input (iOS dictation).
    @StateObject private var composer = ComposerModel()
    @StateObject private var dictation = SpeechDictation()
    @State private var showPhotoPicker = false
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var showFileImporter = false
    /// Why voice input can't start, for an alert that offers Settings.
    @State private var voiceAccessProblem: String?

    private var session: SessionInfo? { model.session(sessionId) }
    private var rows: [TranscriptRow] { buildRows(model.transcripts[sessionId]?.events ?? []) }
    private var pending: [OutgoingPrompt] { model.outbox.filter { $0.sessionId == sessionId } }
    private var isTerminal: Bool { SessionStateName.isTerminal(session?.state ?? "") }
    private var isBusy: Bool { SessionStateName.isBusy(session?.state ?? "") }
    private var canPrompt: Bool { model.link.isOnline && !isTerminal }

    /// An approval is waiting that the transcript has no card for (the
    /// request event is not loaded yet): show a banner instead.
    private func approvalBannerNeeded(_ rows: [TranscriptRow]) -> Bool {
        let waiting = model.approvals.filter { $0.sessionId == sessionId }
        if waiting.isEmpty { return session?.state == SessionStateName.waitingApproval && !rows.contains(where: \.isApprovalRequest) }
        let shown = Set(rows.compactMap(\.approvalId))
        return waiting.contains { !shown.contains($0.approvalId) }
    }

    private var title: String {
        guard let s = session else { return "Session" }
        return s.title ?? model.agentLabel(s)
    }

    private var subtitle: String? {
        guard let s = session else { return nil }
        let place = model.isQuickTasks(s.workspaceId) ? "Quick task" : model.workspaceName(s.workspaceId)
        var parts = [model.agentLabel(s), place]
        if s.state == SessionStateName.ended || s.state == SessionStateName.failed {
            parts.append(SessionStateName.label(s.state))
        }
        return parts.joined(separator: " · ")
    }

    var body: some View {
        let rows = self.rows
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if !model.link.isOnline {
                        ConnectionStatusRow().statusCard()
                    }
                    if approvalBannerNeeded(rows) {
                        ApprovalStatusRow(sessionId: sessionId) { first in
                            if let a = first { approvalShown = a } else {
                                Task { await model.refreshApprovals() }
                            }
                        }
                        .statusCard()
                    }
                    if rows.isEmpty && pending.isEmpty {
                        Group {
                            if model.link.isOnline {
                                if isTerminal || model.isDemo {
                                    Text("No messages yet")
                                        .foregroundStyle(.secondary)
                                } else {
                                    ProgressView()
                                }
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
                .padding(.horizontal, 16)
                .padding(.top, 8)
                .padding(.bottom, 12)
            }
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: rows.count) { _, _ in
                withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: model.streaming[sessionId]?.text) { _, _ in
                proxy.scrollTo("bottom", anchor: .bottom)
            }
            .onAppear { proxy.scrollTo("bottom", anchor: .bottom) }
            .bottomGlassBar { promptBar }
        }
        .background(Color.black)
        .navigationBarTitleDisplayMode(.inline)
        .navTitle(title, subtitle: subtitle)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task { await run { try await model.cancel(sessionId) } }
                    } label: {
                        Label("Stop Turn", systemImage: "stop.circle")
                    }
                    .disabled(!model.link.isOnline || !isBusy)
                    Button(role: .destructive) {
                        confirmEnd = true
                    } label: {
                        Label("End Session", systemImage: "xmark.circle")
                    }
                    .disabled(!model.link.isOnline || isTerminal)
                } label: {
                    ToolbarIcon(systemName: "ellipsis")
                }
                .accessibilityLabel("Session actions")
            }
        }
        .confirmationDialog("End this session?", isPresented: $confirmEnd, titleVisibility: .visible) {
            Button("End Session", role: .destructive) {
                Task { await run { try await model.end(sessionId) } }
            }
        } message: {
            Text("The agent stops. The transcript stays available.")
        }
        .sheet(item: $approvalShown) { a in
            ApprovalView(approvalId: a.approvalId, initial: a)
                .environmentObject(model)
        }
        .onAppear {
            model.open(sessionId)
            #if DEBUG
            if model.isDemo, DemoData.requestedScreen == .dictating {
                dictation.showDemo(text: DemoData.dictationText)
            }
            #endif
        }
        .onDisappear { dictation.cancel() }
        .alert("Voice Input Is Off", isPresented: Binding(
            get: { voiceAccessProblem != nil },
            set: { if !$0 { voiceAccessProblem = nil } }
        )) {
            Button("Open Settings") {
                if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(voiceAccessProblem ?? "")
        }
    }

    // MARK: Composer

    private var promptBar: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let err = actionError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(Palette.red)
                    .padding(.horizontal, 6)
            }
            if !composer.attachments.isEmpty {
                AttachmentChipsRow(attachments: composer.attachments) { composer.remove($0) }
            }
            if dictation.isListening || dictation.isFinishing {
                DictationPanel(
                    dictation: dictation,
                    onCancel: { cancelDictation() },
                    onStop: { stopDictation() }
                )
            } else {
                promptRow
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 6)
        .padding(.bottom, 6)
        .photosPicker(
            isPresented: $showPhotoPicker,
            selection: $photoItems,
            maxSelectionCount: max(1, composer.remainingSlots),
            matching: .images
        )
        .onChange(of: photoItems) { _, items in
            guard !items.isEmpty else { return }
            let skipped = composer.addPhotos(items, model: model)
            photoItems = []
            reportSkipped(skipped)
        }
        .fileImporter(
            isPresented: $showFileImporter,
            allowedContentTypes: [.item],
            allowsMultipleSelection: true
        ) { result in
            switch result {
            case let .success(urls):
                reportSkipped(composer.addFiles(urls, model: model))
            case let .failure(error):
                actionError = userMessage(for: error)
            }
        }
    }

    private var promptRow: some View {
        HStack(alignment: .bottom, spacing: 10) {
            if model.hostSupportsMedia {
                Menu {
                    Button {
                        showPhotoPicker = true
                    } label: {
                        Label("Photo Library", systemImage: "photo.on.rectangle")
                    }
                    Button {
                        showFileImporter = true
                    } label: {
                        Label("Files", systemImage: "folder")
                    }
                } label: {
                    Image(systemName: "plus")
                        .font(.title3.weight(.medium))
                        .foregroundStyle(.primary)
                        .frame(width: 44, height: 44)
                        .contentShape(Circle())
                        .glassSurface(Circle(), interactive: true)
                }
                .tint(.primary)
                .disabled(!canPrompt || composer.remainingSlots == 0)
                .accessibilityLabel("Attach")
            }
            // A glass capsule with the field, the mic, and send or stop.
            HStack(alignment: .bottom, spacing: 4) {
                TextField(placeholder, text: $draft, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($promptFocused)
                    .disabled(!canPrompt)
                    .padding(.vertical, 11)
                    .padding(.leading, 16)
                // iOS dictation on the phone: works with any host.
                Button {
                    micTapped()
                } label: {
                    Image(systemName: "mic")
                        .font(.body.weight(.medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 36, height: 44)
                }
                .buttonStyle(.plain)
                .disabled(!canPrompt)
                .accessibilityLabel("Dictate")
                if session?.state == SessionStateName.running && draft.isEmpty && composer.attachments.isEmpty {
                    Button {
                        Task { await run { try await model.cancel(sessionId) } }
                    } label: {
                        Image(systemName: "stop.fill")
                            .font(.footnote)
                            .foregroundStyle(.white)
                            .frame(width: 32, height: 32)
                            .background(Color.white.opacity(0.2), in: Circle())
                    }
                    .buttonStyle(.plain)
                    .padding(.bottom, 6)
                    .disabled(!model.link.isOnline)
                    .accessibilityLabel("Stop turn")
                } else if canSend || composer.isUploading {
                    Button {
                        model.sendPrompt(sessionId: sessionId, text: draft, attachments: composer.ready)
                        draft = ""
                        composer.clear()
                    } label: {
                        Image(systemName: "arrow.up")
                            .font(.body.weight(.semibold))
                            .foregroundStyle(.white)
                            .frame(width: 32, height: 32)
                            .background(Color.accentColor, in: Circle())
                    }
                    .buttonStyle(.plain)
                    .padding(.bottom, 6)
                    .disabled(!canSend)
                    .accessibilityLabel("Send")
                }
            }
            .padding(.trailing, 6)
            .frame(minHeight: 44)
            .glassSurface(RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .glassGroup()
    }

    private var canSend: Bool {
        guard canPrompt, !composer.isUploading else { return false }
        return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !composer.ready.isEmpty
    }

    @MainActor
    private func reportSkipped(_ count: Int) {
        if count > 0 {
            actionError = "Up to \(RPC.maxAttachments) attachments per prompt; \(count) left out."
        }
    }

    @MainActor
    private func micTapped() {
        Task {
            switch await SpeechDictation.requestAccess() {
            case .granted:
                startDictation()
            case let .denied(why):
                voiceAccessProblem = why
            case let .unavailable(why):
                actionError = why
            }
        }
    }

    @MainActor
    private func startDictation() {
        guard canPrompt, !dictation.isListening, !dictation.isFinishing else { return }
        dictation.onAutoStop = { stopDictation() }
        do {
            try dictation.start()
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            actionError = nil
        } catch {
            actionError = "Dictation failed: \(userMessage(for: error))"
        }
    }

    @MainActor
    private func cancelDictation() {
        dictation.cancel()
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    /// Stop, and add the recognized text to the draft.
    @MainActor
    private func stopDictation() {
        guard dictation.isListening else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        Task {
            let text = await dictation.stop().trimmingCharacters(in: .whitespacesAndNewlines)
            if text.isEmpty {
                actionError = "No speech was recognized."
            } else {
                actionError = nil
                if draft.isEmpty || draft.hasSuffix(" ") || draft.hasSuffix("\n") {
                    draft += text
                } else {
                    draft += " " + text
                }
            }
            promptFocused = true
        }
    }

    private var placeholder: String {
        if isTerminal { return "This session has ended" }
        switch model.link {
        case .online: return "Message"
        case .hostOffline: return "Host offline"
        default: return "Offline"
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

extension TranscriptRow {
    var isApprovalRequest: Bool {
        if case .approvalRequested = self { return true }
        return false
    }

    var approvalId: String? {
        if case let .approvalRequested(_, approval, _, _) = self { return approval?.approvalId }
        return nil
    }
}

// MARK: - Rows

struct TranscriptRowView: View {
    @EnvironmentObject private var model: AppModel
    let row: TranscriptRow
    var onApproval: (PendingApprovalModel) -> Void

    var body: some View {
        switch row {
        case let .prompt(_, text, attachments, device, ts):
            VStack(alignment: .trailing, spacing: 4) {
                if !text.isEmpty {
                    PromptBubble(text: text)
                }
                if !attachments.isEmpty {
                    PromptAttachmentsView(attachments: attachments)
                        .padding(.vertical, 2)
                }
                HStack(spacing: 4) {
                    // Only name the device when it was another one.
                    if let d = device, d != model.deviceName { Text(d) }
                    Text(dateFromMillis(ts), style: .time)
                }
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .padding(.trailing, 4)
            }
            .padding(.leading, 72)
            .frame(maxWidth: .infinity, alignment: .trailing)

        case let .agentMessage(_, text):
            MarkdownView(text: text)
                .equatable()

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
            // A normal end of turn needs no marker.
            if stop != "end_turn" {
                HStack {
                    VStack { Divider() }
                    Text(turnEndLabel(stop, detail))
                        .font(.caption2)
                        .foregroundStyle(stop == "error" ? Color.red : Color.secondary)
                    VStack { Divider() }
                }
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

/// The user's prompt: a bubble in the tint colour.
struct PromptBubble: View {
    let text: String
    var pending = false

    var body: some View {
        Text(text)
            .textSelection(.enabled)
            .foregroundStyle(.white)
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Color.accentColor.opacity(pending ? 0.5 : 1),
                        in: RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}

/// A tool call as one compact row (icon, command or summary, status);
/// tap to see the arguments and output.
struct ToolCallRow: View {
    let tool: ToolRow
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button {
                withAnimation(.snappy(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: summary.icon)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .frame(width: 20)
                    Text(summary.text)
                        .font(summary.monospaced ? .footnote.monospaced() : .subheadline)
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    statusIcon
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(summary.text), \(statusLabel)")
            .accessibilityHint(expanded ? "Hides the details" : "Shows the arguments and output")

            if expanded {
                VStack(alignment: .leading, spacing: 8) {
                    Text(tool.connectorName.map { "\($0) · \(tool.name)" } ?? tool.name)
                        .font(.caption2.bold())
                        .foregroundStyle(.secondary)
                    if !tool.arguments.isNull {
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
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.cell, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private var summary: (icon: String, text: String, monospaced: Bool) {
        let args = tool.arguments
        if let cmd = args["command"].string ?? args["cmd"].string {
            return ("apple.terminal", cmd, true)
        }
        if case let .array(parts) = args["command"] {
            return ("apple.terminal", parts.compactMap(\.string).joined(separator: " "), true)
        }
        if let url = args["url"].string {
            let host = URL(string: url)?.host() ?? url
            return ("globe", tool.status == nil ? "Opening \(host)…" : "Opened \(host)", false)
        }
        if let path = args["path"].string ?? args["file_path"].string {
            return ("doc.text", "\(tool.name) \(URL(fileURLWithPath: path).lastPathComponent)", true)
        }
        if let c = tool.connectorName {
            return ("puzzlepiece.extension", "\(c) · \(tool.name)", false)
        }
        return ("wrench.and.screwdriver", tool.name, true)
    }

    private var statusLabel: String {
        switch tool.status {
        case nil: return "running"
        case "completed"?: return "done"
        case "denied"?: return "denied"
        default: return "failed"
        }
    }

    @ViewBuilder
    private var statusIcon: some View {
        switch tool.status {
        case nil:
            ProgressView().controlSize(.mini)
        case "completed"?:
            Image(systemName: "checkmark").font(.caption.weight(.semibold)).foregroundStyle(.green)
        case "denied"?:
            Image(systemName: "hand.raised.slash").font(.caption).foregroundStyle(.orange)
        default:
            Image(systemName: tool.sandboxDenied ? "lock.trianglebadge.exclamationmark" : "xmark")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.red)
        }
    }
}

/// Monospaced text that scrolls sideways instead of wrapping, with an
/// optional language label (Markdown code fences).
struct CodeBlock: View {
    let text: String
    var language: String? = nil
    var font: Font = .caption2.monospaced()

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let language, !language.isEmpty {
                Text(language)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.top, 6)
            }
            ScrollView(.horizontal) {
                Text(text)
                    .font(font)
                    .textSelection(.enabled)
                    .padding(8)
            }
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
            .background(Palette.cell, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

/// The inline approval card: what the agent wants to do, Deny / Approve
/// (44 pt), and the full details in a sheet.
struct ApprovalRequestRow: View {
    @EnvironmentObject private var model: AppModel
    let approval: PendingApprovalModel?
    let connector: String
    let tool: String
    var onOpen: (PendingApprovalModel) -> Void

    @State private var sending = false
    @State private var error: String?

    private var stillPending: PendingApprovalModel? {
        guard let a = approval else { return nil }
        return model.approvals.first { $0.approvalId == a.approvalId }
    }

    var body: some View {
        let pending = stillPending
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                Text(connector.prefix(1).uppercased())
                    .font(.system(size: 15, weight: .bold, design: .rounded))
                    .frame(width: 30, height: 30)
                    .background(Color.white.opacity(0.1), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(Self.humanize(tool)) on \(connector)?")
                        .font(.headline)
                    Text(detailLine(pending))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            if let preview = approval.flatMap({ Self.preview($0.arguments) }) {
                Text(preview)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            if let pending {
                HStack(spacing: 10) {
                    Button("Deny") { answer(pending, approve: false) }
                        .buttonStyle(CapsuleButtonStyle(fill: Color.white.opacity(0.1), foreground: Palette.red))
                    Button("Approve") { answer(pending, approve: true) }
                        .buttonStyle(CapsuleButtonStyle(fill: .accentColor))
                }
                .disabled(sending || !model.link.isOnline)
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Palette.red)
                }
                Button("Show Details") { onOpen(pending) }
                    .font(.footnote)
                    .frame(maxWidth: .infinity)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.cell, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .strokeBorder(Palette.orange.opacity(pending == nil ? 0 : 0.6), lineWidth: 0.75)
        )
    }

    private func detailLine(_ pending: PendingApprovalModel?) -> String {
        guard let pending else { return "\(tool) · answered" }
        let expires = dateFromMillis(pending.expiresAt)
            .formatted(.relative(presentation: .numeric, unitsStyle: .abbreviated))
        return "\(tool) · expires \(expires)"
    }

    private func answer(_ a: PendingApprovalModel, approve: Bool) {
        sending = true
        error = nil
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        Task {
            do {
                try await model.respond(a.approvalId, approve: approve)
            } catch {
                self.error = userMessage(for: error)
                await model.refreshApprovals()
            }
            sending = false
        }
    }

    /// "create_issue" → "Create issue".
    static func humanize(_ tool: String) -> String {
        let words = tool.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ")
        return words.prefix(1).uppercased() + words.dropFirst()
    }

    /// A one-line gist of the arguments: “title” in owner/repo, or the first
    /// short text argument.
    static func preview(_ args: JSONValue) -> String? {
        guard case let .object(o) = args else { return nil }
        let place: String? = o["repo"]?.string.map { repo in
            if let owner = o["owner"]?.string, !repo.contains("/") { return "\(owner)/\(repo)" }
            return repo
        }
        let main = ["title", "name", "subject", "message", "query", "text"]
            .lazy.compactMap { o[$0]?.string }.first
            ?? o.keys.sorted().lazy.compactMap { o[$0]?.string }.first { $0.count <= 120 }
        guard let main else { return place.map { "in \($0)" } }
        return "“\(main)”" + (place.map { " in \($0)" } ?? "")
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
            MarkdownView(text: streaming.text, streaming: true)
                .equatable()
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
            if !prompt.text.isEmpty {
                PromptBubble(text: prompt.text, pending: true)
            }
            if !prompt.attachments.isEmpty {
                PromptAttachmentsView(attachments: prompt.attachments)
                    .opacity(0.7)
            }
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
        .padding(.leading, 56)
        .frame(maxWidth: .infinity, alignment: .trailing)
    }
}
