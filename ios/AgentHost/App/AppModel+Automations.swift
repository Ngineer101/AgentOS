import Foundation

/// Automations (protocol 5): saved sessions that start on a schedule, from
/// a webhook, or by hand. A phone may list and view them and their runs,
/// create and edit schedule and by-hand automations, switch any on or off,
/// edit the name, prompt, workspace, agent and max run time of any, run one
/// now and delete one. Webhook triggers, their secrets and URLs, and the
/// tools runs use without asking are the desktop's (the host refuses them
/// from a phone).
extension AppModel {
    var hostSupportsAutomations: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.automationProtocolVersion
    }

    func automation(_ id: String) -> AutomationModel? {
        automations?.first { $0.automationId == id }
    }

    /// The automation a session is a run of, if it is one and it's loaded.
    func automation(for session: SessionInfo) -> AutomationModel? {
        session.automationId.flatMap { automation($0) }
    }

    /// `automation.list`. Throws, for the list screen to show why.
    func loadAutomations() async throws {
        guard hostSupportsAutomations else {
            automations = nil
            return
        }
        #if DEBUG
        if isDemo { return }
        #endif
        let list: [AutomationModel] = try await call(RPC.automationList, [:])
        automations = Self.sortedAutomations(list)
    }

    /// `automation.list`, quietly (after reconnecting).
    func refreshAutomations() async {
        try? await loadAutomations()
    }

    static func sortedAutomations(_ list: [AutomationModel]) -> [AutomationModel] {
        list.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    /// `automation.changed`. Ignored until the list is loaded (the list
    /// screen loads it whole).
    func applyAutomationChanged(_ data: Data) {
        guard let c = try? JSONDecoder().decode(AutomationChangedModel.self, from: data) else { return }
        if c.removed == true {
            automations?.removeAll { $0.automationId == c.automationId }
        } else if let a = c.automation {
            upsertAutomation(a)
        }
    }

    func upsertAutomation(_ a: AutomationModel) {
        guard var list = automations else { return }
        if let i = list.firstIndex(where: { $0.automationId == a.automationId }) {
            list[i] = a
        } else {
            list.append(a)
        }
        automations = Self.sortedAutomations(list)
    }

    // MARK: Editing

    /// `automation.create` (schedule or by hand; on).
    func createAutomation(name: String, workspaceId: String, agent: String, profileId: String?,
                          prompt: String, trigger: AutomationTriggerModel, maxMinutes: Int) async throws -> AutomationModel {
        guard let triggerParams = trigger.params else {
            throw ComposerError(message: "Set up webhook automations on the desktop.")
        }
        #if DEBUG
        if isDemo {
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            let a = AutomationModel(
                automationId: "auto-\(UUID().uuidString.prefix(8).lowercased())", name: name, enabled: true,
                workspaceId: workspaceId, agent: agent, profileId: profileId == agent ? nil : profileId,
                prompt: prompt, trigger: trigger, autoApprove: nil, maxMinutes: maxMinutes,
                createdAt: now, updatedAt: now, scheduleText: trigger.cron.map { ScheduleDraft(cron: $0).summary },
                nextRunAt: trigger.cron == nil ? nil : now + 3_600_000, lastRun: nil, webhook: nil)
            upsertAutomation(a)
            return a
        }
        #endif
        var params: [String: Any] = [
            "name": name,
            "workspace_id": workspaceId,
            "agent": agent,
            "prompt": prompt,
            "trigger": triggerParams,
            "max_minutes": maxMinutes,
        ]
        if let profileId, profileId != agent { params["profile_id"] = profileId }
        let a: AutomationModel = try await call(RPC.automationCreate, params)
        upsertAutomation(a)
        return a
    }

    /// `automation.update` with only the fields that change (`changes`:
    /// `name`, `enabled`, `workspace_id`, `agent`, `profile_id`, `prompt`,
    /// `trigger`, `max_minutes`, as JSON-RPC values).
    @discardableResult
    func updateAutomation(_ id: String, changes: [String: Any]) async throws -> AutomationModel {
        #if DEBUG
        if isDemo, var a = automation(id) {
            if let v = changes["name"] as? String { a.name = v }
            if let v = changes["enabled"] as? Bool { a.enabled = v }
            if let v = changes["workspace_id"] as? String { a.workspaceId = v }
            if let v = changes["agent"] as? String { a.agent = v }
            if let v = changes["profile_id"] as? String { a.profileId = v.isEmpty || v == a.agent ? nil : v }
            if let v = changes["prompt"] as? String { a.prompt = v }
            if let v = changes["max_minutes"] as? Int { a.maxMinutes = v }
            if let t = changes["trigger"] as? [String: Any], let kind = t["kind"] as? String {
                if kind == "schedule", let cron = t["cron"] as? String {
                    a.trigger = .schedule(cron: cron)
                    a.scheduleText = ScheduleDraft(cron: cron).summary
                } else if kind == "manual" {
                    a.trigger = .manual
                    a.scheduleText = nil
                    a.nextRunAt = nil
                }
            }
            a.updatedAt = Int64(Date().timeIntervalSince1970 * 1000)
            upsertAutomation(a)
            return a
        }
        #endif
        var params = changes
        params["automation_id"] = id
        let a: AutomationModel = try await call(RPC.automationUpdate, params)
        upsertAutomation(a)
        return a
    }

    /// On or off. Shown at once; put back if the host refuses.
    func setAutomationEnabled(_ id: String, _ on: Bool) async throws {
        guard let before = automation(id), before.enabled != on else { return }
        var a = before
        a.enabled = on
        upsertAutomation(a)
        do {
            try await updateAutomation(id, changes: ["enabled": on])
        } catch {
            if automation(id)?.enabled == on { upsertAutomation(before) }
            throw error
        }
    }

    func deleteAutomation(_ id: String) async throws {
        #if DEBUG
        if isDemo {
            automations?.removeAll { $0.automationId == id }
            return
        }
        #endif
        try await send(RPC.automationDelete, ["automation_id": id])
        automations?.removeAll { $0.automationId == id }
    }

    /// Start a run now. The host refuses while a run is still going.
    @discardableResult
    func runAutomationNow(_ id: String) async throws -> AutomationRunModel {
        #if DEBUG
        if isDemo, var a = automation(id) {
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            let run = AutomationRunModel(
                runId: "run-\(UUID().uuidString.prefix(8).lowercased())", automationId: id,
                sessionId: DemoData.quickSession, trigger: RunTriggerName.manual, scheduledFor: nil,
                startedAt: now, finishedAt: nil, outcome: RunOutcomeName.running, summary: nil, detail: nil)
            a.lastRun = run
            upsertAutomation(a)
            return run
        }
        #endif
        let run: AutomationRunModel = try await call(RPC.automationRunNow, ["automation_id": id])
        if var a = automation(id), a.lastRun?.runId != run.runId,
           (a.lastRun?.startedAt ?? 0) <= run.startedAt {
            a.lastRun = run
            upsertAutomation(a)
        }
        return run
    }

    /// An automation's runs, newest first (at most `limit`).
    func automationRuns(_ id: String, limit: Int = 50) async throws -> [AutomationRunModel] {
        #if DEBUG
        if isDemo {
            var runs = DemoData.automationRuns[id] ?? []
            if let last = automation(id)?.lastRun, !runs.contains(where: { $0.runId == last.runId }) {
                runs.insert(last, at: 0)
            }
            return runs
        }
        #endif
        return try await call(RPC.automationRuns, ["automation_id": id, "limit": limit])
    }

    /// Whether a cron expression is valid, in words, and its next times.
    func checkSchedule(_ cron: String) async throws -> ScheduleCheckModel {
        #if DEBUG
        if isDemo {
            let d = ScheduleDraft(cron: cron)
            let now = Date().timeIntervalSince1970
            let step: TimeInterval
            switch d.preset {
            case .hourly: step = 3600
            case .weekly: step = 7 * 86400
            case .monthly: step = 30 * 86400
            default: step = 86400
            }
            let next = (1...3).map { Int64((now + Double($0) * step) * 1000) }
            return ScheduleCheckModel(error: nil, text: d.summary, next: next)
        }
        #endif
        return try await call(RPC.automationCheckSchedule, ["cron": cron])
    }
}
