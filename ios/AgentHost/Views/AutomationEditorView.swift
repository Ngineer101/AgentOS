import SwiftUI

/// New and Edit Automation: a form in a sheet. A phone makes schedule and
/// by-hand automations; a webhook trigger (and the tools runs use without
/// asking) show read only, set up on the desktop. Saving an edit sends only
/// the fields that changed. Design: "iOS — New automation (sheet)".
struct AutomationEditorView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let existing: AutomationModel?
    var onSaved: (AutomationModel) -> Void

    enum StartsBy: Hashable {
        case schedule, manual
    }

    @State private var name: String
    @State private var prompt: String
    @State private var startsBy: StartsBy
    @State private var schedule: ScheduleDraft
    @State private var workspaceId: String?
    /// The chosen login (`AgentStatusInfo.id`: a profile id, or the agent's
    /// name for its default login).
    @State private var loginId: String?
    /// The user picked a login: a workspace change keeps it.
    @State private var pickedLogin = false
    @State private var maxMinutes: Int
    /// `automation.check_schedule` for `checkedCron`.
    @State private var check: ScheduleCheckModel? = nil
    @State private var checkedCron: String? = nil
    @State private var saving = false
    @State private var error: String? = nil

    init(existing: AutomationModel? = nil, onSaved: @escaping (AutomationModel) -> Void = { _ in }) {
        self.existing = existing
        self.onSaved = onSaved
        _name = State(initialValue: existing?.name ?? "")
        _prompt = State(initialValue: existing?.prompt ?? "")
        _startsBy = State(initialValue: existing?.trigger == AutomationTriggerModel.manual ? StartsBy.manual : StartsBy.schedule)
        _schedule = State(initialValue: existing?.trigger.cron.map { ScheduleDraft(cron: $0) } ?? ScheduleDraft())
        _workspaceId = State(initialValue: existing?.workspaceId)
        _loginId = State(initialValue: existing.map { $0.profileId ?? $0.agent })
        _pickedLogin = State(initialValue: existing != nil)
        _maxMinutes = State(initialValue: existing?.maxMinutes ?? 60)
    }

    private var isNew: Bool { existing == nil }

    /// An existing webhook (or unknown) trigger: shown, not edited.
    private var fixedTrigger: AutomationTriggerModel? {
        guard let t = existing?.trigger else { return nil }
        switch t {
        case .webhook, .other: return t
        case .manual, .schedule: return nil
        }
    }

    private var agents: [AgentStatusInfo] { model.hostInfo?.agents ?? [] }

    /// Quick tasks first, then the most recently used, then by name (as in
    /// New Session).
    private var workspaces: [WorkspaceInfo] {
        var last: [String: Int64] = [:]
        for s in model.sessions { last[s.workspaceId] = max(last[s.workspaceId] ?? 0, s.updatedAt) }
        return model.workspaces.sorted {
            if $0.isQuickTasks != $1.isQuickTasks { return $0.isQuickTasks }
            let a = last[$0.workspaceId] ?? 0, b = last[$1.workspaceId] ?? 0
            if a != b { return a > b }
            return $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }

    /// The trigger as it would be saved (nil for a fixed one).
    private var trigger: AutomationTriggerModel? {
        if fixedTrigger != nil { return nil }
        return startsBy == .manual ? .manual : .schedule(cron: schedule.cron)
    }

    /// The cron expression to check (nil when there is no schedule to edit).
    private var cronToCheck: String? {
        guard fixedTrigger == nil, startsBy == .schedule else { return nil }
        let c = schedule.cron
        return c.isEmpty ? nil : c
    }

    /// The host's answer for the schedule on screen.
    private var currentCheck: ScheduleCheckModel? {
        guard let cron = cronToCheck, checkedCron == cron else { return nil }
        return check
    }

    /// The picker's row for a login not (or not yet) in host info.
    private var unlistedLoginLabel: String {
        guard loginId != nil, let existing else { return "Choose" }
        return model.agentLabel(of: existing)
    }

    private var trimmedName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var canSave: Bool {
        guard model.link.isOnline || model.isDemo, !saving,
              !trimmedName.isEmpty, trimmedName.count <= RPC.maxAutomationName,
              !prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              prompt.count <= RPC.maxAutomationPrompt,
              workspaceId != nil, loginId != nil
        else { return false }
        if fixedTrigger == nil, startsBy == .schedule {
            if schedule.cron.isEmpty { return false }
            if currentCheck?.error != nil { return false }
        }
        return true
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $name)
                        .textInputAutocapitalization(.sentences)
                }

                Section {
                    TextField("What each run should do", text: $prompt, axis: .vertical)
                        .lineLimit(4...12)
                } header: {
                    HStack {
                        GroupHeader("Prompt")
                        Spacer()
                        if isNew && startsBy == .schedule {
                            Menu("Examples") {
                                ForEach(AutomationExamples.schedule.indices, id: \.self) { i in
                                    Button(AutomationExamples.schedule[i].title) { useExample(i) }
                                }
                            }
                            .font(.footnote)
                            .textCase(nil)
                        }
                    }
                }

                triggerSection

                Section {
                    Picker("Workspace", selection: $workspaceId) {
                        if workspaceId == nil {
                            Text("Choose").tag(String?.none)
                        }
                        ForEach(workspaces) { w in
                            Text(w.isQuickTasks ? "Quick tasks" : w.name).tag(Optional(w.workspaceId))
                        }
                    }
                    .pickerStyle(.navigationLink)
                    Picker("Agent", selection: Binding(
                        get: { loginId },
                        set: { loginId = $0; pickedLogin = true }
                    )) {
                        if loginId == nil || !agents.contains(where: { $0.id == loginId }) {
                            Text(unlistedLoginLabel).tag(loginId)
                        }
                        ForEach(agents) { a in
                            Text(a.available ? a.label : "\(a.label) (\(a.problem ?? "unavailable"))")
                                .tag(Optional(a.id))
                        }
                    }
                    .pickerStyle(.navigationLink)
                    Stepper {
                        Text("Max run time: \(RunTimeSteps.label(maxMinutes))")
                    } onIncrement: {
                        maxMinutes = min(RPC.maxAutomationMinutes, RunTimeSteps.up(maxMinutes))
                    } onDecrement: {
                        maxMinutes = max(1, RunTimeSteps.down(maxMinutes))
                    }
                } header: {
                    GroupHeader("Runs as")
                } footer: {
                    Text(isNew
                         ? "A run still going after this long is stopped. Webhook automations, and tools runs may use without asking, are set up on the desktop."
                         : "A run still going after this long is stopped.")
                }

                if let tools = existing?.tools, !tools.isEmpty {
                    Section {
                        LabeledContent("Open pull requests",
                                       value: tools.contains(AutomationTool.createPullRequest) ? "Yes" : "Asks")
                        LabeledContent("Push its own branch",
                                       value: tools.contains(AutomationTool.gitPush) ? "Yes" : "Asks")
                    } header: {
                        GroupHeader("Runs without asking")
                    } footer: {
                        Text("Set up on the desktop.")
                    }
                }

                if let error {
                    Section {
                        Text(error).foregroundStyle(Palette.red)
                    }
                }
            }
            .navigationTitle(isNew ? "New Automation" : "Edit Automation")
            .navigationBarTitleDisplayMode(.inline)
            .modifier(AutomationEditorToolbar(saving: saving, canSave: canSave, cancel: { dismiss() }, save: save))
            .onChange(of: workspaceId) { _, _ in chooseLogin() }
            .onChange(of: schedule.preset) { old, new in
                // Custom starts from the schedule shown so far.
                if new == .custom, old != .custom {
                    var d = schedule
                    d.preset = old
                    schedule.custom = d.cron
                }
            }
            // Debounced: the host describes the schedule and its next times.
            .task(id: cronToCheck) { await checkSchedule() }
            .task {
                // Fresh agent availability and workspaces.
                if !model.isDemo { await model.refreshAll() }
                if workspaceId == nil {
                    let projects = model.workspaces.filter { !$0.isQuickTasks }
                    if projects.count == 1 { workspaceId = projects.first?.workspaceId }
                    #if DEBUG
                    if model.isDemo { workspaceId = "ws-relay" }
                    #endif
                }
                chooseLogin()
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }

    // MARK: Trigger

    @ViewBuilder
    private var triggerSection: some View {
        if let fixed = fixedTrigger {
            Section {
                LabeledContent("Starts on", value: fixedLabel(fixed))
            } header: {
                GroupHeader("Starts")
            } footer: {
                Text("Set up on the desktop.")
            }
        } else {
            Section {
                Picker("Starts", selection: $startsBy) {
                    Text("On a Schedule").tag(StartsBy.schedule)
                    Text("By Hand").tag(StartsBy.manual)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                if startsBy == .schedule {
                    Picker("Repeat", selection: $schedule.preset) {
                        ForEach(SchedulePreset.allCases) { p in
                            Text(p.label).tag(p)
                        }
                    }
                    scheduleFields
                }
            } header: {
                GroupHeader("Starts")
            } footer: {
                scheduleFooter
            }
        }
    }

    @ViewBuilder
    private var scheduleFields: some View {
        switch schedule.preset {
        case .hourly:
            Picker("Minute", selection: $schedule.minute) {
                ForEach(minuteOptions, id: \.self) { m in
                    Text(String(format: ":%02d", m)).tag(m)
                }
            }
        case .daily, .weekdays:
            DatePicker("Time", selection: time, displayedComponents: .hourAndMinute)
        case .weekly:
            Picker("Day", selection: $schedule.weekday) {
                ForEach([1, 2, 3, 4, 5, 6, 0], id: \.self) { d in
                    Text(weekdayName(d)).tag(d)
                }
            }
            DatePicker("Time", selection: time, displayedComponents: .hourAndMinute)
        case .monthly:
            Picker("Day", selection: $schedule.dayOfMonth) {
                ForEach(Array(1...28), id: \.self) { d in
                    Text("\(d)").tag(d)
                }
            }
            DatePicker("Time", selection: time, displayedComponents: .hourAndMinute)
        case .custom:
            TextField("Cron", text: $schedule.custom, prompt: Text("0 9 * * 1-5"))
                .font(.body.monospaced())
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
        }
    }

    @ViewBuilder
    private var scheduleFooter: some View {
        if startsBy == .manual {
            Text("Runs only when you tap Run Now.")
        } else if let err = currentCheck?.error {
            Text(err).foregroundStyle(Palette.red)
        } else {
            let text = currentCheck?.text ?? (schedule.preset == .custom
                ? "Five fields: minute, hour, day of the month, month, day of the week."
                : schedule.summary)
            let next = (currentCheck?.next ?? []).prefix(3).map(formatRunTime)
            Text(next.isEmpty
                 ? "\(text). In \(model.hostName)'s time zone."
                 : "\(text). Next: \(next.joined(separator: ", ")). In \(model.hostName)'s time zone.")
        }
    }

    private func fixedLabel(_ t: AutomationTriggerModel) -> String {
        switch t {
        case let .webhook(w): return w.label
        case let .other(kind): return kind.capitalized
        case .manual: return "By hand"
        case .schedule: return existing?.triggerText ?? "Schedule"
        }
    }

    /// Every 5 minutes, plus the one set (an existing schedule may use any).
    private var minuteOptions: [Int] {
        Array(Set(Array(stride(from: 0, to: 60, by: 5)) + [schedule.minute])).sorted()
    }

    private func weekdayName(_ d: Int) -> String {
        let names = Calendar.current.weekdaySymbols
        return names.count == 7 ? names[d] : ScheduleDraft.weekdayName(d)
    }

    /// The schedule's hour and minute as a date today (DatePicker shows only
    /// the time).
    private var time: Binding<Date> {
        Binding(
            get: {
                Calendar.current.date(bySettingHour: schedule.hour, minute: schedule.minute, second: 0, of: Date()) ?? Date()
            },
            set: { d in
                let c = Calendar.current.dateComponents([.hour, .minute], from: d)
                schedule.hour = c.hour ?? schedule.hour
                schedule.minute = c.minute ?? schedule.minute
            }
        )
    }

    private func checkSchedule() async {
        guard let cron = cronToCheck else {
            check = nil
            checkedCron = nil
            return
        }
        try? await Task.sleep(nanoseconds: 400_000_000)
        guard !Task.isCancelled, model.link.isOnline || model.isDemo else { return }
        do {
            let r = try await model.checkSchedule(cron)
            guard !Task.isCancelled else { return }
            check = r
            checkedCron = cron
        } catch {
            // Offline or an older host: the local description stays.
        }
    }

    private func useExample(_ i: Int) {
        let ex = AutomationExamples.schedule[i]
        prompt = ex.prompt
        if trimmedName.isEmpty { name = ex.title }
    }

    // MARK: Login

    /// The workspace's default login (`AgentStatusInfo`), if set and on the host.
    private func defaultLogin(_ w: WorkspaceInfo?) -> AgentStatusInfo? {
        guard let w, let agentKind = w.defaultAgent else { return nil }
        let pid = w.defaultProfileId ?? agentKind
        return agents.first { $0.agent == agentKind && ($0.profileId ?? $0.agent) == pid }
            ?? (w.defaultProfileId == nil ? agents.first { $0.agent == agentKind && $0.profileName == nil } : nil)
    }

    /// New automations: the workspace's default login, else the current
    /// choice, else the first available. A login the user picked stays.
    private func chooseLogin() {
        if pickedLogin, loginId != nil { return }
        if let d = defaultLogin(workspaceId.flatMap { model.workspace($0) }), d.available {
            loginId = d.id
        } else if let current = loginId, agents.contains(where: { $0.id == current && $0.available }) {
            return
        } else {
            loginId = agents.first(where: { $0.available })?.id
        }
    }

    // MARK: Saving

    private func save() {
        guard let wid = workspaceId, let lid = loginId else { return }
        let login = agents.first { $0.id == lid }
        saving = true
        error = nil
        Task {
            do {
                let saved: AutomationModel
                if let existing {
                    let diff = changes(from: existing, workspaceId: wid, login: login)
                    if diff.isEmpty {
                        saved = existing
                    } else {
                        saved = try await model.updateAutomation(existing.automationId, changes: diff)
                    }
                } else {
                    guard let login, let trigger else {
                        throw ComposerError(message: "Choose an agent.")
                    }
                    saved = try await model.createAutomation(
                        name: trimmedName, workspaceId: wid, agent: login.agent, profileId: login.profileId,
                        prompt: prompt, trigger: trigger, maxMinutes: maxMinutes)
                }
                saving = false
                onSaved(saved)
                dismiss()
            } catch {
                saving = false
                self.error = userMessage(for: error)
            }
        }
    }

    /// The fields of `automation.update` that differ from `a`.
    private func changes(from a: AutomationModel, workspaceId wid: String, login: AgentStatusInfo?) -> [String: Any] {
        var c: [String: Any] = [:]
        if trimmedName != a.name { c["name"] = trimmedName }
        if prompt != a.prompt { c["prompt"] = prompt }
        if wid != a.workspaceId { c["workspace_id"] = wid }
        if let login {
            let newPid = login.profileId ?? login.agent
            if login.agent != a.agent {
                c["agent"] = login.agent
                // The host clears the login when the agent changes; "" or
                // the agent's name is its default login.
                c["profile_id"] = newPid
            } else if newPid != (a.profileId ?? a.agent) {
                c["profile_id"] = newPid
            }
        }
        if let t = trigger, t != a.trigger, let p = t.params { c["trigger"] = p }
        if maxMinutes != a.maxMinutes { c["max_minutes"] = maxMinutes }
        return c
    }
}

/// Close (glass circle) and a prominent Save, as in New Session.
private struct AutomationEditorToolbar: ViewModifier {
    var saving: Bool
    var canSave: Bool
    var cancel: () -> Void
    var save: () -> Void

    func body(content: Content) -> some View {
        content.toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button(action: cancel) {
                    ToolbarIcon(systemName: "xmark")
                }
                .accessibilityLabel("Cancel")
            }
            ToolbarItem(placement: .confirmationAction) {
                if saving {
                    ProgressView()
                } else {
                    saveButton
                }
            }
        }
    }

    @ViewBuilder
    private var saveButton: some View {
        if #available(iOS 26, *) {
            Button("Save", action: save)
                .buttonStyle(.glassProminent)
                .fontWeight(.semibold)
                .disabled(!canSave)
        } else {
            Button(action: save) {
                Text("Save")
                    .fontWeight(.semibold)
                    .padding(.horizontal, 4)
            }
            .buttonStyle(.borderedProminent)
            .buttonBorderShape(.capsule)
            .disabled(!canSave)
        }
    }
}
