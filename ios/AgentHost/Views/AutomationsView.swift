import SwiftUI
import UIKit

// Automations (hosts on protocol 5): Settings → Automations, an automation's
// detail with its runs, and (AutomationEditorView.swift) the editor sheet.
// Designs: "iOS — Settings · Automations", "iOS — Automation",
// "iOS — Automation · webhook", "iOS — New automation (sheet)".

// MARK: - Settings → Automations

struct AutomationsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var loadError: String?
    @State private var actionError: String?
    @State private var showNew = false
    /// Asking before deleting.
    @State private var deleting: AutomationModel?

    var body: some View {
        List {
            if let actionError {
                Section {
                    Text(actionError).foregroundStyle(Palette.red)
                }
            }
            if let list = model.automations, !list.isEmpty {
                Section {
                    ForEach(list) { a in
                        NavigationLink(value: Route.automation(a.automationId)) {
                            AutomationRow(automation: a)
                        }
                        .swipeActions(edge: .leading, allowsFullSwipe: true) {
                            Button {
                                setEnabled(a, !a.enabled)
                            } label: {
                                Label(a.enabled ? "Turn Off" : "Turn On",
                                      systemImage: a.enabled ? "pause.circle" : "play.circle")
                            }
                            .tint(a.enabled ? .orange : .green)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button(role: .destructive) {
                                deleting = a
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                        .contextMenu {
                            Button {
                                setEnabled(a, !a.enabled)
                            } label: {
                                Label(a.enabled ? "Turn Off" : "Turn On",
                                      systemImage: a.enabled ? "pause.circle" : "play.circle")
                            }
                            Button(role: .destructive) {
                                deleting = a
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                } footer: {
                    Text("Each run is a session on \(model.hostName); its transcript is the run's output. Swipe right to turn one on or off. Webhook automations are set up on the desktop.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Automations")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    showNew = true
                } label: {
                    ToolbarIcon(systemName: "plus")
                }
                .accessibilityLabel("New Automation")
                .disabled(!model.link.isOnline)
            }
        }
        .overlay {
            if model.automations == nil {
                if let loadError {
                    ContentUnavailableView(
                        "Couldn't Load Automations",
                        systemImage: "exclamationmark.triangle",
                        description: Text(loadError)
                    )
                } else if model.link.isOnline {
                    ProgressView()
                } else {
                    ContentUnavailableView(
                        "Not Connected",
                        systemImage: "wifi.slash",
                        description: Text("Automations load when the host is reachable.")
                    )
                }
            } else if model.automations?.isEmpty == true {
                ContentUnavailableView {
                    Label("No Automations", systemImage: "clock.arrow.circlepath")
                } description: {
                    Text("Start a session on a schedule, like a weekly dependency upgrade.")
                } actions: {
                    Button("New Automation") { showNew = true }
                        .disabled(!model.link.isOnline)
                }
            }
        }
        .task {
            await load()
            #if DEBUG
            if model.isDemo, DemoData.requestedScreen == .automationNew { showNew = true }
            #endif
        }
        .refreshable { await load() }
        .sheet(isPresented: $showNew) {
            AutomationEditorView()
                .environmentObject(model)
        }
        .confirmationDialog(
            automationDeleteTitle(deleting),
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { a in
            Button("Delete Automation", role: .destructive) { delete(a) }
        } message: { _ in
            Text("It stops running. The sessions of its past runs stay.")
        }
    }

    private func load() async {
        guard model.link.isOnline || model.isDemo else { return }
        do {
            try await model.loadAutomations()
            loadError = nil
        } catch {
            loadError = userMessage(for: error)
        }
    }

    private func setEnabled(_ a: AutomationModel, _ on: Bool) {
        Task {
            do {
                try await model.setAutomationEnabled(a.automationId, on)
                actionError = nil
            } catch {
                actionError = userMessage(for: error)
            }
        }
    }

    private func delete(_ a: AutomationModel) {
        Task {
            do {
                try await model.deleteAutomation(a.automationId)
                actionError = nil
            } catch {
                actionError = userMessage(for: error)
            }
        }
    }
}

/// An automation in the list: its name, its trigger in words, and the next
/// or last run.
struct AutomationRow: View {
    let automation: AutomationModel

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(automation.name)
                .foregroundStyle(automation.enabled ? Color.primary : Color.secondary)
                .lineLimit(1)
            Text(automation.triggerText)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            if let status {
                Text(status)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    /// "Next run in 2 days · Last run succeeded", "Off · Last run failed
    /// 3 days ago", "Running now".
    private var status: String? {
        var parts: [String] = []
        if !automation.enabled { parts.append("Off") }
        if automation.isRunning {
            parts.append("Running now")
        } else if automation.enabled, let next = automation.nextRunAt {
            parts.append("Next run \(relative(next))")
        }
        if let r = automation.lastRun, !r.isRunning {
            var last = "Last run \(RunOutcomeName.label(r.outcome).lowercased())"
            if parts.count < 2 { last += " \(relative(r.finishedAt ?? r.startedAt))" }
            parts.append(last)
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private func relative(_ ms: Int64) -> String {
        dateFromMillis(ms).formatted(.relative(presentation: .named))
    }
}

// MARK: - One automation

struct AutomationDetailView: View {
    @EnvironmentObject private var model: AppModel
    let automationId: String

    @State private var runs: [AutomationRunModel]?
    @State private var runsError: String?
    @State private var actionError: String?
    @State private var editing = false
    @State private var confirmDelete = false
    @State private var startingRun = false
    @State private var copied = false
    @Environment(\.dismiss) private var dismiss

    private var automation: AutomationModel? { model.automation(automationId) }

    var body: some View {
        Group {
            if let a = automation {
                content(a)
            } else if model.automations == nil {
                ProgressView()
            } else {
                ContentUnavailableView(
                    "Automation Deleted",
                    systemImage: "trash",
                    description: Text("It is no longer on \(model.hostName).")
                )
            }
        }
        .navigationTitle(automation?.name ?? "Automation")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if automation != nil {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Edit") { editing = true }
                        .disabled(!model.link.isOnline)
                }
            }
        }
        .sheet(isPresented: $editing) {
            if let a = automation {
                AutomationEditorView(existing: a)
                    .environmentObject(model)
            }
        }
        // Every change (a run started or finished, an edit) reloads the runs.
        .task(id: automation) { await loadRuns() }
        .refreshable {
            await model.refreshAutomations()
            await loadRuns()
        }
        .confirmationDialog(automationDeleteTitle(automation),
                            isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete Automation", role: .destructive) { delete() }
        } message: {
            Text("It stops running. The sessions of its past runs stay.")
        }
    }

    private func content(_ a: AutomationModel) -> some View {
        List {
            if let actionError {
                Section {
                    Text(actionError).foregroundStyle(Palette.red)
                }
            }
            statusSection(a)
            triggerSection(a)
            Section(caps: "Prompt") {
                Text(a.prompt)
                    .textSelection(.enabled)
                    .padding(.vertical, 2)
            }
            Section {
                LabeledContent("Workspace", value: workspaceLabel(a.workspaceId))
                LabeledContent("Agent", value: model.agentLabel(of: a))
                LabeledContent("Max run time", value: RunTimeSteps.label(a.maxMinutes))
            } header: {
                GroupHeader("Runs as")
            } footer: {
                Text("A run still going after this long is stopped.")
            }
            Section {
                LabeledContent("Open pull requests",
                               value: a.tools.contains(AutomationTool.createPullRequest) ? "Yes" : "Asks")
                LabeledContent("Push its own branch",
                               value: a.tools.contains(AutomationTool.gitPush) ? "Yes" : "Asks")
            } header: {
                GroupHeader("Runs without asking")
            } footer: {
                Text("Set up on the desktop. Anything else still asks you.")
            }
            runsSection
            Section {
                Button(role: .destructive) {
                    confirmDelete = true
                } label: {
                    Text("Delete Automation")
                        .frame(maxWidth: .infinity)
                }
                .disabled(!model.link.isOnline)
            }
        }
        .listStyle(.insetGrouped)
    }

    @ViewBuilder
    private func statusSection(_ a: AutomationModel) -> some View {
        Section {
            Toggle("On", isOn: Binding(
                get: { a.enabled },
                set: { on in setEnabled(on) }
            ))
            .disabled(!model.link.isOnline)
            if a.enabled, let next = a.nextRunAt {
                LabeledContent("Next run", value: formatRunTime(next))
            }
            if let d = a.webhook?.lastDelivery {
                VStack(alignment: .leading, spacing: 2) {
                    LabeledContent("Last delivery", value: "\(d.resultLabel) · \(shortAge(d.at))")
                    if !d.detail.isEmpty {
                        Text(d.detail)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    }
                }
            }
            Button {
                runNow()
            } label: {
                HStack {
                    Text("Run Now")
                    if startingRun {
                        Spacer()
                        ProgressView()
                    }
                }
            }
            .disabled(a.isRunning || startingRun || !model.link.isOnline)
        } footer: {
            Text(a.isRunning
                 ? "A run is going. Run Now is available again when it ends."
                 : "Run Now starts a run at once.")
        }
    }

    @ViewBuilder
    private func triggerSection(_ a: AutomationModel) -> some View {
        switch a.trigger {
        case let .schedule(cron):
            Section {
                LabeledContent("Repeats", value: a.triggerText)
            } header: {
                GroupHeader("Schedule")
            } footer: {
                Text("In \(model.hostName)'s time zone. Cron: \(cron)")
            }
        case .manual:
            Section {
                LabeledContent("Starts", value: "By hand")
            } header: {
                GroupHeader("Trigger")
            } footer: {
                Text("Runs only when you tap Run Now.")
            }
        case let .webhook(w):
            Section {
                LabeledContent("Starts on", value: w.label)
                if let url = a.webhook?.url {
                    HStack(spacing: 12) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Webhook URL")
                            Text(url)
                                .font(.footnote.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .textSelection(.enabled)
                        }
                        Spacer(minLength: 8)
                        Button {
                            UIPasteboard.general.string = url
                            copied = true
                        } label: {
                            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        }
                        .buttonStyle(.borderless)
                        .accessibilityLabel(copied ? "Copied" : "Copy Webhook URL")
                    }
                } else {
                    LabeledContent("Webhook URL", value: "Needs a relay")
                }
                LabeledContent("Signing secret", value: a.webhook?.hasSecret == true ? "Stored" : "None")
                if !w.filter.isEmpty {
                    LabeledContent("Filters", value: "\(w.filter.count)")
                }
            } header: {
                GroupHeader("Trigger")
            } footer: {
                Text("Set up on the desktop: the sender, its secret, filters and a new URL.")
            }
        case let .other(kind):
            Section {
                LabeledContent("Starts", value: kind)
            } header: {
                GroupHeader("Trigger")
            } footer: {
                Text("Set up on the desktop.")
            }
        }
    }

    private var runsSection: some View {
        Section {
            if let runs {
                if runs.isEmpty {
                    Text("No runs yet.").foregroundStyle(.secondary)
                }
                ForEach(runs) { r in
                    if let sid = r.sessionId {
                        NavigationLink(value: Route.session(sid)) {
                            AutomationRunRow(run: r)
                        }
                    } else {
                        AutomationRunRow(run: r)
                    }
                }
            } else if let runsError {
                Text(runsError).foregroundStyle(Palette.red)
            } else {
                HStack {
                    Spacer()
                    ProgressView()
                    Spacer()
                }
            }
        } header: {
            GroupHeader("Runs")
        } footer: {
            Text("Tap a run to read its session. The newest run's session stays open for follow-up questions.")
        }
    }

    private func workspaceLabel(_ id: String) -> String {
        model.isQuickTasks(id) ? "Quick tasks" : model.workspaceName(id)
    }

    private func loadRuns() async {
        guard automation != nil, model.link.isOnline || model.isDemo else { return }
        do {
            runs = try await model.automationRuns(automationId)
            runsError = nil
        } catch is CancellationError {
        } catch {
            if runs == nil { runsError = userMessage(for: error) }
        }
    }

    private func setEnabled(_ on: Bool) {
        Task {
            do {
                try await model.setAutomationEnabled(automationId, on)
                actionError = nil
            } catch {
                actionError = userMessage(for: error)
            }
        }
    }

    private func runNow() {
        startingRun = true
        Task {
            do {
                try await model.runAutomationNow(automationId)
                actionError = nil
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
            } catch {
                actionError = userMessage(for: error)
            }
            startingRun = false
            await loadRuns()
        }
    }

    private func delete() {
        Task {
            do {
                try await model.deleteAutomation(automationId)
                dismiss()
            } catch {
                actionError = userMessage(for: error)
            }
        }
    }
}

/// "Delete “Nightly upgrade”?"
func automationDeleteTitle(_ a: AutomationModel?) -> String {
    guard let a else { return "Delete this automation?" }
    return "Delete “\(a.name)”?"
}

/// "Mon 24 Nov, 09:00".
func formatRunTime(_ ms: Int64) -> String {
    dateFromMillis(ms).formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated).hour().minute())
}

/// One run: when it started, why and how it went, and what it said.
struct AutomationRunRow: View {
    let run: AutomationRunModel

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            mark
                .frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(formatRunTime(run.startedAt))
                Text(line)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                if let d = run.detail, !d.isEmpty {
                    Text(d)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                if let s = run.summary, !s.isEmpty {
                    Text(s)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(3)
                }
            }
            .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var mark: some View {
        switch run.outcome {
        case RunOutcomeName.running:
            Spinner(size: 18)
        case RunOutcomeName.succeeded:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(Palette.green)
        case RunOutcomeName.failed:
            Image(systemName: "xmark.circle.fill").foregroundStyle(Palette.red)
        case RunOutcomeName.timedOut:
            Image(systemName: "exclamationmark.circle.fill").foregroundStyle(Palette.orange)
        default:
            Image(systemName: "minus.circle.fill").foregroundStyle(.secondary)
        }
    }

    /// "Schedule · Succeeded · 14 min".
    private var line: String {
        var parts = [RunTriggerName.label(run.trigger), RunOutcomeName.label(run.outcome)]
        if let end = run.finishedAt, run.outcome != RunOutcomeName.skipped {
            let minutes = Int((end - run.startedAt) / 60_000)
            parts.append(minutes < 1 ? "under a minute" : RunTimeSteps.label(minutes))
        }
        return parts.joined(separator: " · ")
    }
}

extension AppModel {
    /// "Claude Code · Work", or the agent's name for its default login.
    func agentLabel(of a: AutomationModel) -> String {
        let pid = a.profileId ?? a.agent
        return hostInfo?.agents.first { $0.agent == a.agent && ($0.profileId ?? $0.agent) == pid }?.label
            ?? agentDisplayName(a.agent)
    }
}
