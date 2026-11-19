import AVFoundation
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// The live transcript of one session, laid out for a narrow screen: glass
/// back and menu buttons, the title (with a menu: agent, workspace, branch,
/// rename) and "status · branch" under it, and the composer at the bottom.
struct SessionView: View {
    @EnvironmentObject private var model: AppModel
    let sessionId: String

    @State private var draft = ""
    @State private var actionError: String?
    @State private var archiving: SessionInfo?
    @Environment(\.dismiss) private var dismiss
    @State private var approvalShown: PendingApprovalModel?
    @State private var promptFocused = false
    /// The side questions sheet (`/btw`) and the conversation it shows.
    @State private var showSides = false
    @State private var sideSelection: String?
    /// Something is being dragged over the session (images, files).
    @State private var dropTargeted = false
    @Environment(\.openURL) private var openURL

    // Attachments (protocol 2 hosts) and voice input (iOS dictation).
    @StateObject private var composer = ComposerModel()
    @StateObject private var dictation = SpeechDictation()
    @State private var showPhotoPicker = false
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var showFileImporter = false
    /// Why voice input can't start, for an alert that offers Settings.
    @State private var voiceAccessProblem: String?
    /// The "Select model" sheet (model, effort, fast mode).
    @State private var showModelSheet = false
    /// "Rename Session…" (an alert with a text field).
    @State private var renaming = false
    @State private var renameText = ""
    /// The subagent whose sheet is open.
    @State private var subagentShown: SubagentRef?

    private var session: SessionInfo? { model.session(sessionId) }
    private var rows: [TranscriptRow] { buildRows(model.transcripts[sessionId]?.events ?? []) }
    private var pending: [OutgoingPrompt] { model.outbox.filter { $0.sessionId == sessionId } }
    private var isArchived: Bool { session?.isArchived ?? false }
    private var isTerminal: Bool { SessionStateName.isTerminal(session?.state ?? "") }
    private var isBusy: Bool { SessionStateName.isBusy(session?.state ?? "") }
    private var canPrompt: Bool { model.link.isOnline && !isTerminal }
    private var sides: [SideConversation] { model.sideConversations(sessionId) }

    /// The question when the draft is a side question (`/btw …`) and the
    /// agent takes them.
    private var sideQuestion: String? {
        guard session?.canAskSideQuestions == true else { return nil }
        let t = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = t.lowercased()
        guard lower == "/btw" || lower.hasPrefix("/btw ") || lower.hasPrefix("/btw\n") else { return nil }
        return String(t.dropFirst(4)).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// An approval is waiting that the transcript has no card for (the
    /// request event is not loaded yet): show a banner instead.
    private func approvalBannerNeeded(_ rows: [TranscriptRow]) -> Bool {
        let waiting = model.approvals.filter { $0.sessionId == sessionId }
        if waiting.isEmpty { return session?.state == SessionStateName.waitingApproval && !rows.contains(where: \.isApprovalRequest) }
        let shown = Set(rows.compactMap(\.approvalId))
        return waiting.contains { !shown.contains($0.approvalId) }
    }

    /// The session's own title, else its workspace ("Quick task").
    private var title: String {
        guard let s = session else { return "Session" }
        return model.sessionTitle(s)
    }

    /// "● Working · ⑂ fix-flaky-resume" with a worktree, else
    /// "● Working · Claude Code (Personal)". The dot has the status's
    /// colour, as in the list. "Your turn" is the resting state, so then
    /// only the detail shows: "⑂ fix-flaky-resume".
    private var subtitle: Text? {
        guard let s = session else { return nil }
        let status = SessionStatus(state: s.state)
        let label = s.isArchived ? "Archived" : status.label
        let dotColor: Color = s.isArchived || status == .ended ? Color(uiColor: .tertiaryLabel) : status.color
        let dot = Text(Image(systemName: "circle.fill"))
            .font(.system(size: 6))
            .foregroundColor(dotColor)
            .baselineOffset(1)
        let detail: Text
        if let branch = s.shortBranch {
            detail = Text(Image(systemName: "arrow.triangle.branch")).font(.caption2) + Text(" \(branch)")
        } else {
            detail = Text(model.agentLoginLabel(s))
        }
        if status == .yourTurn && !s.isArchived { return detail }
        return dot + Text(" \(label) · ") + detail
    }

    /// The title menu: what the session runs as and where, then Copy
    /// Branch Name and Rename Session….
    @ViewBuilder
    private var titleMenu: some View {
        if let s = session {
            let who = model.agentAndLogin(s)
            let quick = model.isQuickTasks(s.workspaceId)
            Section {
                Button {} label: {
                    Text(who.agent)
                    Text(who.login.map { "\($0) login" } ?? "Default login")
                }
                Button {} label: {
                    Text(quick ? "Quick task" : model.workspaceName(s.workspaceId))
                    Text(quick ? "A fresh folder of its own" : abbreviatedPath(model.workspace(s.workspaceId)?.path ?? ""))
                }
                if let branch = s.branch {
                    Button {} label: {
                        Text(branch)
                        Text("Own worktree")
                        Image(systemName: "arrow.triangle.branch")
                    }
                }
            }
            Section {
                if let branch = s.branch {
                    Button {
                        UIPasteboard.general.string = branch
                    } label: {
                        Label("Copy Branch Name", systemImage: "doc.on.doc")
                    }
                }
                Button {
                    startRename()
                } label: {
                    Label("Rename Session…", systemImage: "pencil")
                }
                // Archived sessions are read only here.
                .disabled(isArchived || !model.link.isOnline)
            }
        }
    }

    private func startRename() {
        renameText = session?.title ?? ""
        renaming = true
    }

    /// "/home/nico/Code/relay" → "~/Code/relay".
    private func abbreviatedPath(_ path: String) -> String {
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        if parts.count >= 3, parts[0].isEmpty, parts[1] == "home" || parts[1] == "Users" {
            return (["~"] + parts.dropFirst(3)).joined(separator: "/")
        }
        return path
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
                    ForEach(groupActivity(rows)) { item in
                        switch item {
                        case let .row(row):
                            TranscriptRowView(row: row, onApproval: { a in approvalShown = a },
                                              onSubagent: { id in subagentShown = SubagentRef(id: id) })
                        case let .activity(_, group):
                            ActivityGroupRow(rows: group)
                        }
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
            .onDrop(of: [.image, .fileURL, .data], isTargeted: $dropTargeted) { providers in
                guard canAttach else { return false }
                reportSkipped(composer.addItemProviders(providers, model: model))
                return true
            }
            .overlay {
                if dropTargeted && canAttach {
                    RoundedRectangle(cornerRadius: 24, style: .continuous)
                        .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2, dash: [8, 6]))
                        .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 24, style: .continuous))
                        .overlay(Label("Drop to Attach", systemImage: "paperclip").font(.headline))
                        .padding(12)
                        .allowsHitTesting(false)
                }
            }
            .bottomGlassBar {
                if isArchived {
                    archivedBar
                } else {
                    promptBar
                }
            }
        }
        .background(Color.black)
        .navigationBarTitleDisplayMode(.inline)
        .sessionTitleBar(title, subtitle: subtitle) { titleMenu }
        .toolbar {
            if !sides.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        if sideSelection == nil || !sides.contains(where: { $0.id == sideSelection }) {
                            sideSelection = sides.last?.id
                        }
                        showSides = true
                    } label: {
                        ToolbarIcon(systemName: "bubble.left.and.text.bubble.right")
                    }
                    .accessibilityLabel("Side questions")
                    .accessibilityValue("\(sides.count)")
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task { await run { try await model.cancel(sessionId) } }
                    } label: {
                        Label("Stop Turn", systemImage: "stop.circle")
                    }
                    .disabled(!model.link.isOnline || !isBusy)
                    Divider()
                    if isArchived {
                        Button {
                            Task { await run { try await model.unarchive(sessionId) } }
                        } label: {
                            Label("Unarchive", systemImage: "tray.and.arrow.up")
                        }
                        .disabled(!model.link.isOnline)
                    } else {
                        Button {
                            archiving = session
                        } label: {
                            Label("Archive", systemImage: "archivebox")
                        }
                        .disabled(!model.link.isOnline || session == nil)
                    }
                } label: {
                    ToolbarIcon(systemName: "ellipsis")
                }
                .accessibilityLabel("Session actions")
            }
        }
        .archiveFlow($archiving) { dismiss() }
        .sheet(isPresented: $showSides) {
            SideQuestionsSheet(sessionId: sessionId, selection: $sideSelection)
                .environmentObject(model)
        }
        .sheet(item: $approvalShown) { a in
            ApprovalView(approvalId: a.approvalId, initial: a)
                .environmentObject(model)
        }
        .sheet(item: $subagentShown) { ref in
            SubagentSheet(sessionId: sessionId, callId: ref.id)
                .environmentObject(model)
        }
        .onAppear {
            model.open(sessionId)
            #if DEBUG
            if model.isDemo, DemoData.requestedScreen == .dictating {
                dictation.showDemo(text: DemoData.dictationText)
            }
            if model.isDemo, DemoData.requestedScreen == .paste, composer.attachments.isEmpty {
                composer.showDemoImage(DemoData.pastedImage(), name: "Pasted Image.png")
                draft = "Why does this layout overflow on the iPhone SE?"
            }
            if model.isDemo, DemoData.opensModelSheet {
                showModelSheet = true
            }
            if model.isDemo, DemoData.requestedScreen == .side {
                sideSelection = sides.first?.id
                showSides = true
            }
            if model.isDemo, DemoData.requestedScreen == .btw {
                draft = DemoData.sideDraft
            }
            if model.isDemo, DemoData.opensRename {
                startRename()
            }
            if model.isDemo, DemoData.opensTitleMenu {
                DemoMenuOpener.openTitleMenu()
            }
            if model.isDemo, let id = DemoData.opensSubagent {
                subagentShown = SubagentRef(id: id)
            }
            #endif
        }
        .onDisappear { dictation.cancel() }
        .alert("Rename Session", isPresented: $renaming) {
            TextField(session.map { model.isQuickTasks($0.workspaceId) ? "Quick task" : model.workspaceName($0.workspaceId) } ?? "Title",
                      text: $renameText)
                .submitLabel(.done)
            Button("Cancel", role: .cancel) {}
            Button("Rename") {
                let t = renameText
                Task { await run { try await model.rename(sessionId, title: t) } }
            }
        } message: {
            Text("Leave it empty to name the session after its workspace.")
        }
        .onChange(of: renameText) { _, t in
            if t.count > RPC.maxSessionTitle { renameText = String(t.prefix(RPC.maxSessionTitle)) }
        }
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

    /// In place of the composer for an archived session: read only.
    private var archivedBar: some View {
        VStack(spacing: 8) {
            if let err = actionError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(Palette.red)
            }
            Text("Archived — read only")
                .font(.footnote)
                .foregroundStyle(.secondary)
            Button {
                Task { await run { try await model.unarchive(sessionId) } }
            } label: {
                Label("Unarchive", systemImage: "tray.and.arrow.up")
            }
            .buttonStyle(CapsuleButtonStyle(fill: .accentColor, height: 50))
            .disabled(!model.link.isOnline)
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 6)
    }

    private var promptBar: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let err = actionError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(Palette.red)
                    .padding(.horizontal, 6)
            }
            let agents = runningAgents
            if !agents.isEmpty && !dictation.isListening && !dictation.isFinishing {
                SubagentsRunningPill(agents: agents) { id in subagentShown = SubagentRef(id: id) }
                    .padding(.leading, 4)
            }
            if dictation.isListening || dictation.isFinishing {
                DictationPanel(
                    dictation: dictation,
                    onCancel: { cancelDictation() },
                    onStop: { stopDictation() }
                )
            } else {
                composerBox
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
        .sheet(isPresented: $showModelSheet) {
            ModelSheet(sessionId: sessionId, isLive: canPrompt)
                .environmentObject(model)
        }
    }

    /// One rounded box with only what you act on: the attachments, the
    /// field, and a row with attach, dictate, the model and send. (Where
    /// the session works is in the title and its menu.) A side question's
    /// leading `/btw` is shown as a tinted token in the field.
    private var composerBox: some View {
        let shape = RoundedRectangle(cornerRadius: 22, style: .continuous)
        let hasChips = !composer.attachments.isEmpty
        return VStack(alignment: .leading, spacing: 0) {
            if hasChips {
                AttachmentChipsRow(attachments: composer.attachments) { composer.remove($0) }
                    .contentMargins(.horizontal, 12, for: .scrollContent)
                    .padding(.top, 12)
            }
            ComposerTextView(
                text: $draft,
                isFocused: $promptFocused,
                placeholder: placeholder,
                isEnabled: canPrompt,
                insets: UIEdgeInsets(top: hasChips ? 6 : 13, left: 16, bottom: 6, right: 16),
                tokenPrefix: session?.canAskSideQuestions == true ? "/btw" : nil,
                onPasteItems: canAttach ? { providers in
                    reportSkipped(composer.addItemProviders(providers, model: model))
                } : nil
            )
            composerControls
                .padding(.leading, 6)
                .padding(.trailing, 8)
                .padding(.bottom, 8)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .secondarySystemBackground), in: shape)
        .overlay(shape.strokeBorder(Color.white.opacity(0.1), lineWidth: 0.5))
        .contentShape(shape)
        .onTapGesture {
            if canPrompt { promptFocused = true }
        }
    }

    private var composerControls: some View {
        HStack(spacing: 2) {
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
                        .font(.title3)
                        .foregroundStyle(.primary)
                        .frame(width: 36, height: 36)
                        .contentShape(Circle())
                }
                .tint(.primary)
                .disabled(!canPrompt || composer.remainingSlots == 0)
                .accessibilityLabel("Attach")
            }
            // iOS dictation on the phone: works with any host.
            Button {
                micTapped()
            } label: {
                Image(systemName: "mic")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .frame(width: 36, height: 36)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .disabled(!canPrompt)
            .accessibilityLabel("Dictate")

            Spacer(minLength: 8)

            if let s = session, let chip = ModelChip(session: s) {
                Button {
                    showModelSheet = true
                } label: {
                    chip
                }
                .buttonStyle(.plain)
                .disabled(!canPrompt)
                .padding(.trailing, 6)
            }

            if session?.state == SessionStateName.running && draft.isEmpty && composer.attachments.isEmpty {
                Button {
                    Task { await run { try await model.cancel(sessionId) } }
                } label: {
                    Image(systemName: "stop.fill")
                        .font(.system(size: 13, weight: .bold))
                        .foregroundStyle(.black)
                        .frame(width: 32, height: 32)
                        .background(Color.white, in: Circle())
                }
                .buttonStyle(.plain)
                .disabled(!model.link.isOnline)
                .accessibilityLabel("Stop turn")
            } else {
                Button {
                    send()
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(.black)
                        .frame(width: 32, height: 32)
                        .background(Color.white, in: Circle())
                        .opacity(canSend ? 1 : 0.3)
                }
                .buttonStyle(.plain)
                .disabled(!canSend)
                .accessibilityLabel(sideQuestion != nil ? "Ask side question" : "Send")
            }
        }
    }

    /// Subagents still at work (none once the session has stopped).
    private var runningAgents: [SubagentRun] {
        guard !isTerminal, !isArchived else { return [] }
        return rows.compactMap { if case let .subagent(a) = $0, a.isRunning { return a }; return nil }
    }

    private var canSend: Bool {
        guard canPrompt else { return false }
        if let q = sideQuestion { return !q.isEmpty }
        guard !composer.isUploading else { return false }
        return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !composer.ready.isEmpty
    }

    /// Attachments: hosts on protocol 2, while the session takes prompts.
    private var canAttach: Bool {
        model.hostSupportsMedia && canPrompt && composer.remainingSlots > 0
    }

    /// A prompt, or a side question for `/btw …` (attachments stay for the
    /// next prompt: side questions take none).
    private func send() {
        if let q = sideQuestion {
            let text = draft
            draft = ""
            Task {
                do {
                    let sid = try await model.sideAsk(sessionId: sessionId, sideId: nil, text: q)
                    actionError = nil
                    sideSelection = sid
                    showSides = true
                } catch {
                    if draft.isEmpty { draft = text }
                    actionError = userMessage(for: error)
                }
            }
            return
        }
        model.sendPrompt(sessionId: sessionId, text: draft, attachments: composer.ready)
        draft = ""
        composer.clear()
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
    var onSubagent: (String) -> Void = { _ in }

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

        case let .subagent(a):
            SubagentCard(run: a) { onSubagent(a.callId) }

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
        VStack(alignment: .leading, spacing: 4) {
            Button {
                withAnimation(.snappy(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "brain")
                        .font(.caption)
                        .frame(width: 16)
                    Text("Thinking")
                        .font(.footnote)
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                    Spacer(minLength: 0)
                }
                .foregroundStyle(.secondary)
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if expanded {
                Text(text)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.leading, 24)
            }
        }
    }
}

/// The user's prompt: light text on a dark grey bubble (white on the
/// tint colour was too low-contrast to read on a phone).
struct PromptBubble: View {
    let text: String
    var pending = false

    var body: some View {
        Text(text)
            .textSelection(.enabled)
            .foregroundStyle(pending ? Color.secondary : Color.primary)
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Color(uiColor: .systemGray5),
                        in: RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}

/// A tool call as one compact row (icon, command or summary, status);
/// tap to see the arguments and output.
struct ToolCallRow: View {
    @EnvironmentObject private var model: AppModel
    let tool: ToolRow
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button {
                withAnimation(.snappy(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    if let c = tool.connectorName, model.connectorIcon(id: nil, name: c) != nil {
                        ConnectorIcon(id: nil, name: c, size: 16)
                    } else {
                        Image(systemName: summary.icon)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .frame(width: 16)
                    }
                    Text(summary.text)
                        .font(summary.monospaced ? .caption.monospaced() : .footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if tool.status != "completed" { statusIcon }
                    Spacer(minLength: 4)
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
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var summary: ToolSummary { tool.summary }

    private var statusLabel: String {
        tool.statusLabel
    }

    @ViewBuilder
    private var statusIcon: some View {
        ToolStatusIcon(tool: tool)
    }
}

/// What a tool call did, in a few words, and which kind of action it was
/// (for "Ran 3 commands, opened 2 pages").
struct ToolSummary {
    enum Kind { case command, page, file, connector, other }
    var icon: String
    var text: String
    var monospaced: Bool
    var kind: Kind
}

extension ToolRow {
    var summary: ToolSummary {
        let args = arguments
        if let cmd = args["command"].string ?? args["cmd"].string {
            return ToolSummary(icon: "apple.terminal", text: cmd, monospaced: true, kind: .command)
        }
        if case let .array(parts) = args["command"] {
            return ToolSummary(icon: "apple.terminal", text: parts.compactMap(\.string).joined(separator: " "),
                               monospaced: true, kind: .command)
        }
        if let url = args["url"].string {
            let host = URL(string: url)?.host() ?? url
            return ToolSummary(icon: "globe", text: status == nil ? "Opening \(host)…" : "Opened \(host)",
                               monospaced: false, kind: .page)
        }
        if let path = args["path"].string ?? args["file_path"].string {
            return ToolSummary(icon: "doc.text", text: "\(name) \(URL(fileURLWithPath: path).lastPathComponent)",
                               monospaced: true, kind: .file)
        }
        if let c = connectorName {
            return ToolSummary(icon: "puzzlepiece.extension", text: "\(c) · \(name)", monospaced: false, kind: .connector)
        }
        return ToolSummary(icon: "wrench.and.screwdriver", text: name, monospaced: true, kind: .other)
    }

    var statusLabel: String {
        switch status {
        case nil: return "running"
        case "completed"?: return "done"
        case "denied"?: return "denied"
        default: return "failed"
        }
    }

    var failed: Bool { status != nil && status != "completed" }
}

/// Running, done, denied or failed, as a small symbol.
struct ToolStatusIcon: View {
    let tool: ToolRow

    var body: some View {
        switch tool.status {
        case nil:
            ProgressView().controlSize(.mini)
        case "completed"?:
            Image(systemName: "checkmark").font(.caption2.weight(.semibold)).foregroundStyle(.green)
        case "denied"?:
            Image(systemName: "hand.raised.slash").font(.caption2).foregroundStyle(.orange)
        default:
            // Muted, not red: most failed calls don't change the outcome.
            Image(systemName: tool.sandboxDenied ? "lock.trianglebadge.exclamationmark" : "xmark")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(tool.sandboxDenied ? AnyShapeStyle(.orange) : AnyShapeStyle(.tertiary))
        }
    }
}

/// A transcript row, or a run of tool calls and thinking shown as one line.
enum TranscriptItem: Identifiable {
    case row(TranscriptRow)
    case activity(id: String, rows: [TranscriptRow])

    var id: String {
        switch self {
        case let .row(r): return r.id
        case let .activity(id, _): return id
        }
    }
}

/// Fold each run of two or more tool calls and thoughts into one item, the
/// way Claude's app shows a turn's work.
func groupActivity(_ rows: [TranscriptRow]) -> [TranscriptItem] {
    var items: [TranscriptItem] = []
    var run: [TranscriptRow] = []
    func flush() {
        if run.count >= 2, run.contains(where: { if case .tool = $0 { return true }; return false }) {
            items.append(.activity(id: "g" + run[0].id, rows: run))
        } else {
            items.append(contentsOf: run.map(TranscriptItem.row))
        }
        run = []
    }
    for r in rows {
        switch r {
        case .tool, .thought: run.append(r)
        default:
            flush()
            items.append(.row(r))
        }
    }
    flush()
    return items
}

/// "Ran 3 commands, opened 2 pages ›": tap to see each call. While a call
/// runs, the line says what it is doing.
struct ActivityGroupRow: View {
    let rows: [TranscriptRow]
    @State private var expanded = false

    private var tools: [ToolRow] {
        rows.compactMap { if case let .tool(t) = $0 { return t }; return nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Button {
                withAnimation(.snappy(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    if let running = tools.last(where: { $0.status == nil }) {
                        ProgressView().controlSize(.mini).frame(width: 16)
                        Text(running.summary.text)
                            .font(running.summary.monospaced ? .caption.monospaced() : .footnote)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    } else {
                        Image(systemName: "checklist")
                            .font(.caption)
                            .frame(width: 16)
                        // No failure count here: a failed call (a grep with
                        // no match, a missing file) is routine and the agent
                        // carries on. Each call still shows its own status.
                        Text(title)
                            .font(.footnote)
                            .lineLimit(1)
                    }
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                    Spacer(minLength: 0)
                }
                .foregroundStyle(.secondary)
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(title)
            .accessibilityHint(expanded ? "Hides the steps" : "Shows each step")

            if expanded {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(rows) { r in
                        switch r {
                        case let .tool(t): ToolCallRow(tool: t)
                        case let .thought(_, text): ThoughtRow(text: text)
                        default: EmptyView()
                        }
                    }
                }
                .padding(.leading, 8)
                .overlay(alignment: .leading) {
                    Rectangle().fill(.quaternary).frame(width: 1)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// "Ran 3 commands, opened 2 pages, used 1 tool".
    private var title: String {
        func n(_ k: ToolSummary.Kind) -> Int { tools.filter { $0.summary.kind == k }.count }
        func plural(_ c: Int, _ one: String, _ many: String) -> String { c == 1 ? "1 \(one)" : "\(c) \(many)" }
        var parts: [String] = []
        if n(.command) > 0 { parts.append("ran " + plural(n(.command), "command", "commands")) }
        if n(.page) > 0 { parts.append("opened " + plural(n(.page), "page", "pages")) }
        if n(.file) > 0 { parts.append("worked on " + plural(n(.file), "file", "files")) }
        let other = n(.connector) + n(.other)
        if other > 0 { parts.append("used " + plural(other, "tool", "tools")) }
        if parts.isEmpty { return "Thought" }
        let s = parts.joined(separator: ", ")
        return s.prefix(1).uppercased() + s.dropFirst()
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
        if let install = pending ?? approval, install.isInstall {
            installCard(install, pending: pending)
        } else {
            connectorCard(pending)
        }
    }

    /// An agent asking to install a tool on the computer: what, why, the
    /// exact command, and that it runs as you outside the sandbox.
    private func installCard(_ a: PendingApprovalModel, pending: PendingApprovalModel?) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "terminal")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Palette.orange)
                    .frame(width: 30, height: 30)
                    .background(Palette.orange.opacity(0.18), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(a.summary ?? "Run a command on this computer?")
                        .font(.headline)
                    Text(pending == nil ? "Answered" : "Runs as you, outside the sandbox · \(expiresText(a))")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            if let reason = a.reason {
                Text("“\(reason)”").font(.subheadline)
            }
            if let details = a.details {
                MarkdownView(text: details)
                    .lineLimit(8)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Color.secondary.opacity(0.3), lineWidth: 0.5))
            }
            if let command = a.command {
                Text("$ \(command)")
                    .font(.footnote.monospaced())
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(Color.black.opacity(0.35), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .textSelection(.enabled)
            }
            if let pending {
                Text(a.note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack(spacing: 10) {
                    Button("Deny") { answer(pending, approve: false) }
                        .buttonStyle(CapsuleButtonStyle(fill: Color.white.opacity(0.1), foreground: Palette.red))
                    Button(a.approveLabel) { answer(pending, approve: true) }
                        .buttonStyle(CapsuleButtonStyle(fill: .accentColor))
                }
                .disabled(sending || !model.link.isOnline)
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Palette.red)
                }
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

    private func expiresText(_ a: PendingApprovalModel) -> String {
        "expires " + dateFromMillis(a.expiresAt).formatted(.relative(presentation: .numeric, unitsStyle: .abbreviated))
    }

    private func connectorCard(_ pending: PendingApprovalModel?) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                ConnectorIcon(id: approval?.connectorId, name: connector, size: 30)
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

// MARK: - Model and effort

extension SessionInfo {
    /// The agent's model choice (`model` category).
    var modelOption: SessionConfigOption? { configOption(category: "model") }
    /// Effort (`thought_level` category).
    var effortOption: SessionConfigOption? { configOption(category: "thought_level") }

    /// Fast mode: an on/off setting with id `fast` (or category
    /// `model_config`), as the Claude adapter reports it.
    var fastOption: SessionConfigOption? {
        configOptions?.first { o in
            (o.id == "fast" || o.category == "model_config")
                && Set(o.options.map(\.value)) == ["on", "off"] && o.options.count == 2
        }
    }

    var isFastMode: Bool { fastOption?.currentValue == "on" }
    var hasModelSettings: Bool { modelOption != nil || effortOption != nil || fastOption != nil }
}

/// The label of the composer's model button: the model ("Opus 4.5",
/// "Default" for "Default (recommended)"), a bolt when fast mode is on,
/// and a small chevron. The Select model sheet has the full names.
struct ModelChip: View {
    let title: String
    let fast: Bool
    @Environment(\.isEnabled) private var isEnabled

    /// Nil when the session has no model, effort or fast setting.
    init?(session s: SessionInfo) {
        guard s.hasModelSettings else { return nil }
        title = s.modelOption.map { Self.shortName($0.currentName) }
            ?? s.effortOption.map { "Effort: \($0.currentName)" } ?? "Model"
        fast = s.isFastMode
    }

    /// "Default (recommended)" → "Default"; a name that is only a
    /// parenthetical stays as it is.
    static func shortName(_ name: String) -> String {
        let t = name.trimmingCharacters(in: .whitespaces)
        guard t.hasSuffix(")"), let open = t.lastIndex(of: "(") else { return t }
        let head = t[..<open].trimmingCharacters(in: .whitespaces)
        return head.isEmpty ? t : head
    }

    var body: some View {
        HStack(spacing: 4) {
            Text(title)
                .font(.subheadline)
                .lineLimit(1)
            if fast {
                Image(systemName: "bolt.fill")
                    .font(.caption)
            }
            Image(systemName: "chevron.down")
                .font(.caption2.weight(.semibold))
                .imageScale(.small)
        }
        .foregroundStyle(.secondary)
        .opacity(isEnabled ? 1 : 0.5)
        .padding(.horizontal, 6)
        .frame(minHeight: 36)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Model")
        .accessibilityValue(fast ? "\(title), fast mode" : title)
        .accessibilityHint("Choose the model and effort")
    }
}

/// "Select model": the agent's models, effort as a stepped slider, and
/// fast mode. Each change is sent at once; the sheet stays open.
struct ModelSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let sessionId: String
    var isLive: Bool
    @State private var error: String?

    private var session: SessionInfo? { model.session(sessionId) }

    var body: some View {
        NavigationStack {
            List {
                if let error {
                    Section {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.footnote)
                            .foregroundStyle(Palette.red)
                    }
                }
                if let m = session?.modelOption {
                    Section {
                        ForEach(m.options) { o in
                            modelRow(o, selected: o.value == m.currentValue) {
                                guard o.value != m.currentValue else { return }
                                set(m, o.value)
                            }
                        }
                    }
                }
                if let s = session, s.effortOption != nil || s.fastOption != nil {
                    Section {
                        if let e = s.effortOption {
                            VStack(alignment: .leading, spacing: 10) {
                                HStack {
                                    Text("Effort")
                                    Spacer()
                                    Text(e.currentName)
                                        .foregroundStyle(.secondary)
                                }
                                .accessibilityHidden(true)
                                EffortSlider(option: e) { value in set(e, value) }
                            }
                            .padding(.vertical, 4)
                        }
                        if let f = s.fastOption {
                            Toggle("Fast mode", isOn: Binding(
                                get: { f.currentValue == "on" },
                                set: { on in set(f, on ? "on" : "off") }
                            ))
                        }
                    } footer: {
                        if let d = s.fastOption?.description, !d.isEmpty {
                            Text(d)
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .listSectionSpacing(16)
            .contentMargins(.top, 4, for: .scrollContent)
            .environment(\.defaultMinListRowHeight, 44)
            .disabled(!isLive)
            .navigationTitle("Select model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        dismiss()
                    } label: {
                        ToolbarIcon(systemName: "xmark")
                    }
                    .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private func modelRow(_ o: SessionConfigValue, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: "cpu")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: 1) {
                    Text(o.name)
                        .foregroundStyle(.primary)
                    if let d = o.description, !d.isEmpty {
                        Text(d)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 8)
                if selected {
                    Image(systemName: "checkmark")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Color.accentColor)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func set(_ option: SessionConfigOption, _ value: String) {
        UISelectionFeedbackGenerator().selectionChanged()
        Task {
            do {
                try await model.setConfig(sessionId: sessionId, configId: option.id, value: value)
                error = nil
            } catch {
                self.error = userMessage(for: error)
            }
        }
    }
}

/// A stepped slider over an option's values: one stop per value with a
/// small dot, a white knob, the part before it filled. Sends the value
/// once, when the finger lifts.
struct EffortSlider: View {
    let option: SessionConfigOption
    var onCommit: (String) -> Void
    @Environment(\.isEnabled) private var isEnabled
    /// The stop under the finger while dragging.
    @State private var dragIndex: Int?

    private let height: CGFloat = 30
    private let inset: CGFloat = 2

    private var values: [SessionConfigValue] { option.options }
    private var currentIndex: Int { values.firstIndex { $0.value == option.currentValue } ?? 0 }
    private var index: Int { dragIndex ?? currentIndex }

    var body: some View {
        GeometryReader { geo in
            let knob = height - inset * 2
            let first = inset + knob / 2
            let last = geo.size.width - inset - knob / 2
            let count = max(values.count, 1)
            let step = count > 1 ? (last - first) / CGFloat(count - 1) : 0
            let x = first + step * CGFloat(index)
            ZStack(alignment: .leading) {
                Capsule().fill(Color(uiColor: .tertiarySystemFill))
                Capsule()
                    .fill(Color(uiColor: .systemGray2))
                    .frame(width: x + knob / 2 + inset)
                ForEach(values.indices, id: \.self) { i in
                    Circle()
                        .fill(Color.white.opacity(i <= index ? 0.55 : 0.28))
                        .frame(width: 5, height: 5)
                        .position(x: first + step * CGFloat(i), y: height / 2)
                }
                Circle()
                    .fill(Color.white)
                    .shadow(color: .black.opacity(0.3), radius: 3, y: 1)
                    .frame(width: knob, height: knob)
                    .position(x: x, y: height / 2)
            }
            .frame(height: height)
            .opacity(isEnabled ? 1 : 0.5)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { g in
                        guard isEnabled, step > 0 || count == 1 else { return }
                        let i = step > 0 ? Int(((g.location.x - first) / step).rounded()) : 0
                        let clamped = min(max(i, 0), count - 1)
                        if clamped != index { UISelectionFeedbackGenerator().selectionChanged() }
                        withAnimation(.snappy(duration: 0.15)) { dragIndex = clamped }
                    }
                    .onEnded { _ in
                        guard let i = dragIndex else { return }
                        commit(i)
                    }
            )
            .animation(.snappy(duration: 0.2), value: index)
        }
        .frame(height: height)
        .onChange(of: option.currentValue) { _, _ in dragIndex = nil }
        .accessibilityElement()
        .accessibilityLabel("Effort")
        .accessibilityValue(values.indices.contains(index) ? values[index].name : option.currentName)
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: commit(min(currentIndex + 1, values.count - 1))
            case .decrement: commit(max(currentIndex - 1, 0))
            @unknown default: break
            }
        }
    }

    /// The knob stays on the new stop until the session reports it (or
    /// puts the old value back).
    private func commit(_ i: Int) {
        guard values.indices.contains(i), values[i].value != option.currentValue else {
            dragIndex = nil
            return
        }
        dragIndex = i
        onCommit(values[i].value)
    }
}

// MARK: - Side questions

/// Side conversations (`/btw`): one tab each, the question, the streamed
/// answer and a field for follow-ups. Asked in a fork of the conversation,
/// so the session itself is not interrupted.
struct SideQuestionsSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let sessionId: String
    @Binding var selection: String?

    @State private var followUp = ""
    @State private var sending = false
    @State private var error: String?

    private var sides: [SideConversation] { model.sideConversations(sessionId) }
    private var current: SideConversation? {
        sides.first { $0.id == selection } ?? sides.last
    }
    private var canAsk: Bool {
        guard let s = model.session(sessionId) else { return false }
        return model.link.isOnline && !SessionStateName.isTerminal(s.state) && !s.isArchived
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                tabs
                Divider().opacity(0.5)
                if let c = current {
                    conversation(c)
                } else {
                    ContentUnavailableView(
                        "No Side Questions",
                        systemImage: "bubble.left.and.text.bubble.right",
                        description: Text("Start one with /btw and your question.")
                    )
                }
            }
            .navigationTitle("Side Questions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        dismiss()
                    } label: {
                        ToolbarIcon(systemName: "xmark")
                    }
                    .accessibilityLabel("Close")
                }
            }
            .safeAreaInset(edge: .bottom) {
                if current != nil { followUpBar }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        .onChange(of: sides.map(\.id)) { _, ids in
            if ids.isEmpty { dismiss() }
        }
    }

    /// One pill per conversation; the selected one closes with its ×.
    private var tabs: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(sides) { side in
                        let selected = side.id == current?.id
                        HStack(spacing: 6) {
                            if side.isAnswering {
                                ProgressView().controlSize(.mini)
                            }
                            Text(side.title)
                                .font(.subheadline.weight(selected ? .semibold : .regular))
                                .lineLimit(1)
                                .truncationMode(.tail)
                                .frame(maxWidth: 180, alignment: .leading)
                                .fixedSize(horizontal: true, vertical: false)
                            if selected {
                                Button {
                                    close(side.id)
                                } label: {
                                    Image(systemName: "xmark.circle.fill")
                                        .symbolRenderingMode(.hierarchical)
                                        .font(.subheadline)
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Close “\(side.title)”")
                            }
                        }
                        .foregroundStyle(selected ? Color.white : Color.primary)
                        .padding(.leading, 14)
                        .padding(.trailing, selected ? 8 : 14)
                        .frame(height: 34)
                        .background(selected ? Color.accentColor : Palette.cell, in: Capsule())
                        .contentShape(Capsule())
                        .onTapGesture { selection = side.id }
                        .accessibilityElement(children: .contain)
                        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
                        .id(side.id)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
            }
            .onAppear { if let id = current?.id { proxy.scrollTo(id) } }
            .onChange(of: selection) { _, id in
                if let id { withAnimation { proxy.scrollTo(id) } }
            }
        }
    }

    private func conversation(_ c: SideConversation) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    Text("Asked in a fork of the conversation. The session carries on undisturbed.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .multilineTextAlignment(.center)
                    ForEach(Array(c.turns.enumerated()), id: \.offset) { _, turn in
                        PromptBubble(text: turn.question, pending: turn.pending)
                            .padding(.leading, 56)
                            .frame(maxWidth: .infinity, alignment: .trailing)
                        if !turn.answer.isEmpty {
                            MarkdownView(text: turn.answer, streaming: !turn.done)
                                .equatable()
                        } else if turn.error == nil && !turn.done {
                            Label(turn.pending ? "Sending…" : "Thinking…", systemImage: "ellipsis")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .symbolEffect(.variableColor.iterative, options: .repeating)
                        }
                        if let err = turn.error {
                            Label(err, systemImage: "exclamationmark.triangle")
                                .font(.caption)
                                .foregroundStyle(Palette.red)
                        }
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(16)
            }
            .scrollDismissesKeyboard(.interactively)
            .onAppear { proxy.scrollTo("bottom", anchor: .bottom) }
            .onChange(of: c) { _, _ in proxy.scrollTo("bottom", anchor: .bottom) }
        }
    }

    private var followUpBar: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let error {
                Text(error).font(.footnote).foregroundStyle(Palette.red).padding(.horizontal, 6)
            }
            HStack(alignment: .bottom, spacing: 4) {
                TextField(canAsk ? "Ask a follow-up" : "Follow-ups need a live session", text: $followUp, axis: .vertical)
                    .lineLimit(1...4)
                    .disabled(!canAsk)
                    .padding(.vertical, 11)
                    .padding(.leading, 16)
                    .submitLabel(.send)
                Button {
                    ask()
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(width: 32, height: 32)
                        .background(Color.accentColor, in: Circle())
                }
                .buttonStyle(.plain)
                .padding(.bottom, 6)
                .disabled(!canSendFollowUp)
                .opacity(canSendFollowUp ? 1 : 0.4)
                .accessibilityLabel("Send follow-up")
            }
            .padding(.trailing, 6)
            .frame(minHeight: 44)
            .glassSurface(RoundedRectangle(cornerRadius: 22, style: .continuous))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    private var canSendFollowUp: Bool {
        canAsk && !sending && !(current?.isAnswering ?? true)
            && !followUp.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func ask() {
        guard let c = current, canSendFollowUp else { return }
        let text = followUp
        followUp = ""
        sending = true
        Task {
            do {
                try await model.sideAsk(sessionId: sessionId, sideId: c.id, text: text)
                error = nil
            } catch {
                if followUp.isEmpty { followUp = text }
                self.error = userMessage(for: error)
            }
            sending = false
        }
    }

    private func close(_ id: String) {
        let ids = sides.map(\.id)
        if let i = ids.firstIndex(of: id) {
            let rest = ids.filter { $0 != id }
            selection = rest.isEmpty ? nil : rest[min(i, rest.count - 1)]
        }
        withAnimation { model.closeSide(id) }
    }
}

// MARK: - Subagents

/// Which subagent's sheet is open (its call id).
struct SubagentRef: Identifiable, Hashable {
    let id: String
}

/// "general-purpose agent · 4 steps · 38s"; the time counts up while it runs.
struct SubagentMetaText: View {
    let run: SubagentRun
    var showsSteps = true
    var showsState = false

    var body: some View {
        if run.isRunning {
            TimelineView(.periodic(from: .now, by: 1)) { ctx in
                Text(text(now: Int64(ctx.date.timeIntervalSince1970 * 1000)))
            }
        } else {
            Text(text(now: run.endedAt ?? run.startedAt))
        }
    }

    private func text(now: Int64) -> String {
        var parts = [run.agentLabel]
        if showsState {
            parts.append(run.isRunning ? "running" : run.status == "completed" ? "done"
                         : run.status == "interrupted" ? "stopped" : "failed")
        }
        if showsSteps, run.stepCount > 0 {
            parts.append(run.stepCount == 1 ? "1 step" : "\(run.stepCount) steps")
        }
        if run.background { parts.append("background") }
        parts.append(formatDuration(ms: run.elapsed(now: now)))
        return parts.joined(separator: " · ")
    }
}

/// Running, done or failed, for a subagent.
struct SubagentStatusIcon: View {
    let run: SubagentRun

    var body: some View {
        if run.isRunning {
            ProgressView().controlSize(.mini)
        } else if run.failed {
            Image(systemName: "xmark").font(.caption.weight(.semibold)).foregroundStyle(.red)
        } else {
            Image(systemName: "checkmark").font(.caption.weight(.semibold)).foregroundStyle(.green)
        }
    }
}

/// A subagent as one card in the conversation: its task, type, steps,
/// time and status. Running, the step it's on; done, the start of its
/// report. Tap for its steps and full report.
struct SubagentCard: View {
    let run: SubagentRun
    var onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "sparkle")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .frame(width: 28, height: 28)
                    .background(Palette.fill, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                VStack(alignment: .leading, spacing: 3) {
                    HStack(alignment: .center, spacing: 8) {
                        Text(run.title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(.primary)
                            .lineLimit(2)
                        Spacer(minLength: 4)
                        SubagentStatusIcon(run: run)
                            .frame(width: 16, height: 16)
                        Image(systemName: "chevron.right")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.tertiary)
                    }
                    SubagentMetaText(run: run)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    preview
                        .padding(.top, 5)
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.cell, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(Color.white.opacity(0.06), lineWidth: 0.5))
            .contentShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Subagent: \(run.title), \(run.isRunning ? "running" : run.failed ? "failed" : "done")")
        .accessibilityHint("Shows its steps and report")
    }

    @ViewBuilder
    private var preview: some View {
        if run.isRunning {
            if let line = latestLine {
                HStack(spacing: 6) {
                    Image(systemName: "play.fill").font(.system(size: 7))
                    Text(line)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .foregroundStyle(.secondary)
            }
        } else if let report = run.report, !report.isEmpty {
            Text(markdownPreview(report))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .multilineTextAlignment(.leading)
        }
    }

    /// What it is doing now: its latest step.
    private var latestLine: String? {
        switch run.steps.last {
        case let .tool(t)?: return t.summary.text
        case let .message(_, text)?: return markdownPreview(text, lines: 1)
        case nil: return nil
        }
    }
}

/// "1 agent running" above the composer: tap to open it (or, with
/// several, pick one).
struct SubagentsRunningPill: View {
    let agents: [SubagentRun]
    var onOpen: (String) -> Void

    var body: some View {
        Group {
            if agents.count == 1, let a = agents.first {
                Button { onOpen(a.callId) } label: { label }
                    .buttonStyle(.plain)
            } else {
                Menu {
                    ForEach(agents, id: \.callId) { a in
                        Button { onOpen(a.callId) } label: {
                            Text(a.title)
                            Text(a.agentLabel)
                        }
                    }
                } label: { label }
                .buttonStyle(.plain)
            }
        }
        .accessibilityLabel(title)
        .accessibilityHint(agents.count == 1 ? "Opens it" : "Choose one to open")
    }

    private var title: String {
        agents.count == 1 ? "1 agent running" : "\(agents.count) agents running"
    }

    private var label: some View {
        HStack(spacing: 7) {
            ProgressView().controlSize(.mini)
            Text(title)
                .font(.footnote)
                .foregroundStyle(.primary)
            Image(systemName: "chevron.up")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 12)
        .frame(height: 32)
        .background(Palette.cell, in: Capsule())
        .overlay(Capsule().strokeBorder(Color.white.opacity(0.1), lineWidth: 0.5))
        .contentShape(Capsule())
    }
}

/// One subagent: its task, each step it took and what it said, and once
/// it's done, its report. Updates live.
struct SubagentSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let sessionId: String
    let callId: String
    @State private var taskExpanded = false

    private var run: SubagentRun? {
        for r in buildRows(model.transcripts[sessionId]?.events ?? []) {
            if case let .subagent(a) = r, a.callId == callId { return a }
        }
        return nil
    }

    var body: some View {
        NavigationStack {
            Group {
                if let run {
                    content(run)
                } else {
                    ContentUnavailableView("Subagent Not Found", systemImage: "sparkle")
                }
            }
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
                ToolbarItem(placement: .principal) {
                    if let run {
                        VStack(spacing: 1) {
                            Text(run.title)
                                .font(.headline)
                                .lineLimit(1)
                            SubagentMetaText(run: run, showsSteps: false, showsState: true)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private func content(_ run: SubagentRun) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if !run.prompt.isEmpty { taskBox(run.prompt) }

                VStack(alignment: .leading, spacing: 6) {
                    sectionLabel("Steps")
                    if run.steps.isEmpty {
                        Text(run.isRunning ? "Starting…" : "No steps")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(run.steps.enumerated()), id: \.offset) { _, step in
                        switch step {
                        case let .tool(t):
                            ToolCallRow(tool: t)
                        case let .message(_, text):
                            MarkdownView(text: text)
                                .equatable()
                                .font(.footnote)
                                .padding(.vertical, 4)
                        }
                    }
                }

                if run.isRunning {
                    Text("The report appears here when it finishes.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .multilineTextAlignment(.center)
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        sectionLabel(run.failed ? "Result" : "Report")
                        if let report = run.report, !report.isEmpty {
                            MarkdownView(text: report)
                                .equatable()
                        } else {
                            Text(run.status == "interrupted" ? "Stopped before it reported." : "No report.")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Text("Stopping the turn (\(Image(systemName: "stop.fill")) in the composer) stops its agents too.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity)
                    .multilineTextAlignment(.center)
                    .padding(.top, 8)
            }
            .padding(16)
        }
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.caption2.weight(.semibold))
            .foregroundStyle(.secondary)
            .accessibilityAddTraits(.isHeader)
    }

    private func taskBox(_ prompt: String) -> some View {
        let long = prompt.count > 280 || prompt.split(separator: "\n").count > 6
        return VStack(alignment: .leading, spacing: 6) {
            sectionLabel("Task")
            Text(prompt)
                .font(.subheadline)
                .lineLimit(taskExpanded || !long ? nil : 6)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if long {
                Button(taskExpanded ? "Show less" : "Show more") {
                    withAnimation(.snappy(duration: 0.2)) { taskExpanded.toggle() }
                }
                .font(.footnote.weight(.medium))
            }
        }
        .padding(14)
        .background(Palette.fill, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

// MARK: - Title bar

extension View {
    /// The session's title with a menu behind it (a small chevron by the
    /// title) and a styled subtitle. A principal `Menu` rather than the
    /// system title menu and `navigationSubtitle`, which drop the
    /// subtitle's coloured status dot and branch glyph.
    func sessionTitleBar<M: View>(_ title: String, subtitle: Text?, @ViewBuilder menu: () -> M) -> some View {
        let content = menu()
        return navigationTitle(title)
            .toolbar {
                SessionTitleItem(title: title, subtitle: subtitle, menu: content)
            }
    }
}

/// The principal toolbar item of `sessionTitleBar`, without the glass
/// capsule iOS 26 would put behind it.
struct SessionTitleItem<M: View>: ToolbarContent {
    let title: String
    let subtitle: Text?
    let menu: M

    var body: some ToolbarContent {
        if #available(iOS 26, *) {
            item.sharedBackgroundVisibility(.hidden)
        } else {
            item
        }
    }

    private var item: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            Menu {
                menu
            } label: {
                VStack(spacing: 1) {
                    HStack(spacing: 4) {
                        Text(title)
                            .font(.headline)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Image(systemName: "chevron.down")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.secondary)
                    }
                    if let subtitle {
                        subtitle
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                .foregroundStyle(.primary)
                .padding(.horizontal, 8)
                .contentShape(Rectangle())
            }
            .accessibilityHint("Shows the session's details")
        }
    }
}

#if DEBUG
/// Screenshots: open the navigation bar's title menu, which can't be
/// opened from code otherwise. DEBUG builds only.
enum DemoMenuOpener {
    @MainActor
    static func openTitleMenu(attempt: Int = 0) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
            guard let window = UIApplication.shared.connectedScenes
                .compactMap({ ($0 as? UIWindowScene)?.keyWindow }).first else { return }
            let target = window.bounds.width / 2
            var best: (UIView, UIContextMenuInteraction, CGFloat)?
            func visit(_ v: UIView) {
                if let i = v.interactions.compactMap({ $0 as? UIContextMenuInteraction }).first {
                    let f = v.convert(v.bounds, to: window)
                    if f.minY < 140, f.width < window.bounds.width * 0.8 {
                        let d = abs(f.midX - target)
                        if best == nil || d < best!.2 { best = (v, i, d) }
                    }
                }
                v.subviews.forEach(visit)
            }
            visit(window)
            guard let (view, interaction, _) = best else {
                if attempt < 5 { openTitleMenu(attempt: attempt + 1) }
                return
            }
            let sel = NSSelectorFromString("_presentMenuAtLocation:")
            if interaction.responds(to: sel) {
                typealias Present = @convention(c) (AnyObject, Selector, CGPoint) -> Void
                let imp = interaction.method(for: sel)
                unsafeBitCast(imp, to: Present.self)(interaction, sel, CGPoint(x: view.bounds.midX, y: 0))
            } else if #available(iOS 17.4, *), let control = view as? UIControl {
                control.performPrimaryAction()
            }
        }
    }
}
#endif
