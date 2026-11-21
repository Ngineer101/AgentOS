import Foundation

// Automations (`automation.*`, protocol 5): saved sessions that start on a
// schedule, from a webhook, or by hand. Each run is an ordinary session.
// Mirrors `crates/proto/src/automation.rs`.

extension RPC {
    static let automationList = "automation.list"
    static let automationGet = "automation.get"
    static let automationCreate = "automation.create"
    /// Only the fields given change.
    static let automationUpdate = "automation.update"
    static let automationDelete = "automation.delete"
    static let automationRunNow = "automation.run_now"
    /// Runs of one automation, newest first.
    static let automationRuns = "automation.runs"
    /// Whether a cron expression is valid, in words, and its next times.
    static let automationCheckSchedule = "automation.check_schedule"
    /// `{automation_id, automation?, removed?}`: an automation changed, a run
    /// of it started or finished, or it was deleted.
    static let nAutomationChanged = "automation.changed"
    /// Hosts on this protocol version or newer have automations.
    static let automationProtocolVersion: UInt32 = 5
    /// `MAX_MAX_MINUTES` on the host: the longest a run may last.
    static let maxAutomationMinutes = 720
    /// `MAX_AUTOMATION_PROMPT`: the longest prompt, in characters.
    static let maxAutomationPrompt = 20_000
    /// The longest name, in characters.
    static let maxAutomationName = 120
}

/// Host tools an automation's runs may use without asking (`AUTO_APPROVABLE`).
enum AutomationTool {
    static let createPullRequest = "create_pull_request"
    static let gitPush = "git_push"
}

/// `RunOutcome` values.
enum RunOutcomeName {
    static let running = "running"
    static let succeeded = "succeeded"
    static let failed = "failed"
    static let cancelled = "cancelled"
    static let timedOut = "timed_out"
    static let skipped = "skipped"

    static func label(_ o: String) -> String {
        switch o {
        case running: return "Running"
        case succeeded: return "Succeeded"
        case failed: return "Failed"
        case cancelled: return "Stopped"
        case timedOut: return "Timed out"
        case skipped: return "Skipped"
        default: return o
        }
    }
}

/// `RunTrigger` values: why a run started.
enum RunTriggerName {
    static let schedule = "schedule"
    static let catchUp = "catch_up"
    static let webhook = "webhook"
    static let manual = "manual"

    static func label(_ t: String) -> String {
        switch t {
        case schedule: return "Schedule"
        case catchUp: return "Catch-up"
        case webhook: return "Webhook"
        case manual: return "Run Now"
        default: return t
        }
    }
}

/// `{"path": "payload.action", "equals": "created"}`.
struct WebhookMatchModel: Codable, Hashable {
    var path: String
    var equals: String
}

/// A webhook trigger. Phones only show it: the host refuses to create or
/// change one from a phone.
struct WebhookTriggerModel: Hashable {
    /// `sentry`, `github` or `generic`.
    var source: String
    var filter: [WebhookMatchModel] = []
    var dedupeKey: String?
    var cooldownMinutes: Int = 24 * 60

    /// "Sentry webhook", "GitHub webhook", "Webhook".
    var label: String {
        switch source {
        case "sentry": return "Sentry webhook"
        case "github": return "GitHub webhook"
        default: return "Webhook"
        }
    }
}

/// `AutomationTrigger`, tagged by `kind`.
enum AutomationTriggerModel: Codable, Hashable {
    /// Only "Run Now".
    case manual
    /// Five cron fields in the host's local time, or `@daily` and friends.
    case schedule(cron: String)
    case webhook(WebhookTriggerModel)
    /// A kind this app doesn't know (a newer host).
    case other(String)

    var kind: String {
        switch self {
        case .manual: return "manual"
        case .schedule: return "schedule"
        case .webhook: return "webhook"
        case let .other(k): return k
        }
    }

    var isWebhook: Bool {
        if case .webhook = self { return true }
        return false
    }

    var cron: String? {
        if case let .schedule(c) = self { return c }
        return nil
    }

    /// As JSON-RPC params (schedule and manual only: a phone never sends a
    /// webhook trigger).
    var params: [String: Any]? {
        switch self {
        case .manual: return ["kind": "manual"]
        case let .schedule(c): return ["kind": "schedule", "cron": c]
        case .webhook, .other: return nil
        }
    }

    private enum CodingKeys: String, CodingKey {
        case kind, cron, source, filter
        case dedupeKey = "dedupe_key"
        case cooldownMinutes = "cooldown_minutes"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "manual":
            self = .manual
        case "schedule":
            self = .schedule(cron: try c.decode(String.self, forKey: .cron))
        case "webhook":
            self = .webhook(WebhookTriggerModel(
                source: try c.decode(String.self, forKey: .source),
                filter: try c.decodeIfPresent([WebhookMatchModel].self, forKey: .filter) ?? [],
                dedupeKey: try c.decodeIfPresent(String.self, forKey: .dedupeKey),
                cooldownMinutes: try c.decodeIfPresent(Int.self, forKey: .cooldownMinutes) ?? 24 * 60
            ))
        default:
            self = .other(kind)
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(kind, forKey: .kind)
        switch self {
        case let .schedule(cron):
            try c.encode(cron, forKey: .cron)
        case let .webhook(w):
            try c.encode(w.source, forKey: .source)
            if !w.filter.isEmpty { try c.encode(w.filter, forKey: .filter) }
            try c.encodeIfPresent(w.dedupeKey, forKey: .dedupeKey)
            try c.encode(w.cooldownMinutes, forKey: .cooldownMinutes)
        case .manual, .other:
            break
        }
    }
}

/// The last delivery to a webhook: when, and what became of it.
struct WebhookDeliveryModel: Codable, Hashable {
    var at: Int64
    /// `started`, `ignored` (filter, dedupe, busy) or `refused` (bad signature).
    var result: String
    var detail: String

    var resultLabel: String {
        switch result {
        case "started": return "Started"
        case "ignored": return "Ignored"
        case "refused": return "Refused"
        default: return result.capitalized
        }
    }
}

/// An automation's webhook endpoint, as clients show it.
struct WebhookInfoModel: Codable, Hashable {
    /// The URL to give the sender; nil when the host has no relay.
    var url: String?
    /// A signing secret is stored (never sent back).
    var hasSecret: Bool
    var lastDelivery: WebhookDeliveryModel?

    enum CodingKeys: String, CodingKey {
        case url
        case hasSecret = "has_secret"
        case lastDelivery = "last_delivery"
    }
}

struct AutomationRunModel: Codable, Hashable, Identifiable {
    var runId: String
    var automationId: String
    /// The run's session; nil for a skipped run.
    var sessionId: String?
    /// `RunTriggerName`.
    var trigger: String
    /// The time the schedule asked for (schedule and catch-up runs).
    var scheduledFor: Int64?
    var startedAt: Int64
    var finishedAt: Int64?
    /// `RunOutcomeName`.
    var outcome: String
    /// The agent's last reply, shortened.
    var summary: String?
    /// Why it was skipped or failed; for webhook runs, what fired it.
    var detail: String?

    var id: String { runId }
    var isRunning: Bool { outcome == RunOutcomeName.running }

    enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case automationId = "automation_id"
        case sessionId = "session_id"
        case trigger
        case scheduledFor = "scheduled_for"
        case startedAt = "started_at"
        case finishedAt = "finished_at"
        case outcome, summary, detail
    }
}

struct AutomationModel: Codable, Hashable, Identifiable {
    var automationId: String
    var name: String
    var enabled: Bool
    var workspaceId: String
    /// `AgentKind` (`claude`, `codex`, …).
    var agent: String
    var profileId: String?
    var prompt: String
    var trigger: AutomationTriggerModel
    /// Host tools its runs use without asking (`AutomationTool`). Absent when none.
    var autoApprove: [String]?
    var maxMinutes: Int
    var createdAt: Int64
    var updatedAt: Int64
    /// The schedule in words ("Every day at 09:00").
    var scheduleText: String?
    /// When the schedule fires next (enabled schedules only).
    var nextRunAt: Int64?
    var lastRun: AutomationRunModel?
    var webhook: WebhookInfoModel?

    var id: String { automationId }
    var tools: [String] { autoApprove ?? [] }
    /// The last run is still going: Run Now would be refused.
    var isRunning: Bool { lastRun?.isRunning ?? false }

    /// "Every day at 09:00", "Sentry webhook", "By hand".
    var triggerText: String {
        switch trigger {
        case .manual: return "By hand"
        case let .schedule(cron): return scheduleText ?? ScheduleDraft(cron: cron).summary
        case let .webhook(w): return w.label
        case let .other(k): return k.capitalized
        }
    }

    enum CodingKeys: String, CodingKey {
        case automationId = "automation_id"
        case name, enabled
        case workspaceId = "workspace_id"
        case agent
        case profileId = "profile_id"
        case prompt, trigger
        case autoApprove = "auto_approve"
        case maxMinutes = "max_minutes"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case scheduleText = "schedule_text"
        case nextRunAt = "next_run_at"
        case lastRun = "last_run"
        case webhook
    }
}

/// `automation.changed` params.
struct AutomationChangedModel: Codable {
    var automationId: String
    var automation: AutomationModel?
    var removed: Bool?

    enum CodingKeys: String, CodingKey {
        case automationId = "automation_id"
        case automation, removed
    }
}

/// `automation.check_schedule` result.
struct ScheduleCheckModel: Codable, Hashable {
    var error: String?
    var text: String?
    var next: [Int64]?
}

/// Prompts that suit maintenance on a schedule (`SCHEDULE_EXAMPLES`).
enum AutomationExamples {
    static let schedule: [(title: String, prompt: String)] = [
        ("Upgrade dependencies",
         "Upgrade this project's dependencies to their latest compatible versions. Run the build and the tests; fix what breaks. If everything passes, commit on this branch, push it and open a pull request listing what changed. If an upgrade needs real work, leave it out and say why."),
        ("Routine maintenance",
         "Do a maintenance pass on this repository: run the tests and linters, fix warnings and small problems you find, and remove dead code you're sure is unused. Commit on this branch, push it and open a pull request with a short summary."),
    ]
}

// MARK: - Schedules

/// The repeat choices of the schedule editor; each builds a five-field cron
/// expression (`minute hour day-of-month month day-of-week`).
enum SchedulePreset: String, CaseIterable, Identifiable {
    case hourly, daily, weekdays, weekly, monthly, custom

    var id: String { rawValue }

    var label: String {
        switch self {
        case .hourly: return "Every Hour"
        case .daily: return "Every Day"
        case .weekdays: return "Weekdays"
        case .weekly: return "Every Week"
        case .monthly: return "Every Month"
        case .custom: return "Custom"
        }
    }
}

/// A schedule as the editor shows it. `init(cron:)` reads a cron expression
/// back into a preset when it is one (else `.custom`); `cron` writes it.
struct ScheduleDraft: Equatable {
    var preset: SchedulePreset = .daily
    var hour = 9
    var minute = 0
    /// 0 = Sunday … 6 = Saturday (cron's numbering).
    var weekday = 1
    /// 1…28, so every month has it.
    var dayOfMonth = 1
    /// The expression for `.custom`.
    var custom = ""

    init() {}

    init(cron text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        custom = trimmed
        switch trimmed.lowercased() {
        case "@hourly": preset = .hourly; minute = 0; return
        case "@daily", "@midnight": preset = .daily; hour = 0; minute = 0; return
        case "@weekly": preset = .weekly; weekday = 0; hour = 0; minute = 0; return
        case "@monthly": preset = .monthly; dayOfMonth = 1; hour = 0; minute = 0; return
        default: break
        }
        preset = .custom
        let f = trimmed.split(whereSeparator: { $0 == " " || $0 == "\t" }).map(String.init)
        guard f.count == 5, let m = Int(f[0]), (0...59).contains(m), f[3] == "*" else { return }
        if f[1] == "*" {
            if f[2] == "*" && f[4] == "*" {
                preset = .hourly
                minute = m
            }
            return
        }
        guard let h = Int(f[1]), (0...23).contains(h) else { return }
        switch (f[2], f[4]) {
        case ("*", "*"):
            preset = .daily
        case ("*", "1-5"):
            preset = .weekdays
        case ("*", let w):
            guard let d = Int(w), (0...7).contains(d) else { return }
            preset = .weekly
            weekday = d % 7
        case (let dom, "*"):
            guard let d = Int(dom), (1...28).contains(d) else { return }
            preset = .monthly
            dayOfMonth = d
        default:
            return
        }
        hour = h
        minute = m
    }

    var cron: String {
        switch preset {
        case .hourly: return "\(minute) * * * *"
        case .daily: return "\(minute) \(hour) * * *"
        case .weekdays: return "\(minute) \(hour) * * 1-5"
        case .weekly: return "\(minute) \(hour) * * \(weekday)"
        case .monthly: return "\(minute) \(hour) \(dayOfMonth) * *"
        case .custom: return custom.trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }

    /// The schedule in words, for when the host's description isn't there
    /// (offline, or the demo). Says the same as the host for the presets.
    var summary: String {
        let at = String(format: "%02d:%02d", hour, minute)
        switch preset {
        case .hourly: return minute == 0 ? "Every hour" : String(format: "Every hour at :%02d", minute)
        case .daily: return "Every day at \(at)"
        case .weekdays: return "Weekdays at \(at)"
        case .weekly: return "Every \(Self.weekdayName(weekday)) at \(at)"
        case .monthly: return "Every month on day \(dayOfMonth) at \(at)"
        case .custom: return custom.isEmpty ? "Custom" : "Cron: \(cron)"
        }
    }

    /// "Monday" for 1 (English, like the host's descriptions).
    static func weekdayName(_ d: Int) -> String {
        let names = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
        return names[((d % 7) + 7) % 7]
    }
}

// MARK: - Run time

/// Steps of the "Max run time" stepper, in minutes.
enum RunTimeSteps {
    static let ladder = [5, 10, 15, 30, 45, 60, 90, 120, 180, 240, 360, 480, 720]

    static func up(_ m: Int) -> Int {
        ladder.first { $0 > m } ?? RPC.maxAutomationMinutes
    }

    static func down(_ m: Int) -> Int {
        ladder.last { $0 < m } ?? 1
    }

    /// "45 min", "1 hr", "1 hr 30 min".
    static func label(_ m: Int) -> String {
        if m < 60 { return "\(m) min" }
        let h = m / 60, rest = m % 60
        return rest == 0 ? "\(h) hr" : "\(h) hr \(rest) min"
    }
}
