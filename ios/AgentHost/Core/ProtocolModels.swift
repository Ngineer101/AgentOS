import Foundation

/// Method and notification names, exactly as in `crates/proto/src/methods.rs`.
/// Only the methods a phone may call are listed (the host refuses the rest
/// with a capability error).
enum RPC {
    static let hostInfo = "host.info"
    static let workspaceList = "workspace.list"
    static let sessionList = "session.list"
    static let sessionGet = "session.get"
    static let sessionCreate = "session.create"
    static let sessionPrompt = "session.prompt"
    static let sessionCancel = "session.cancel"
    static let sessionEnd = "session.end"
    /// Hide a session from the list (ending it first if live), and back.
    static let sessionArchive = "session.archive"
    static let sessionUnarchive = "session.unarchive"
    /// Give a session a title of its own (`title` nil or blank clears it).
    static let sessionRename = "session.rename"
    /// `MAX_SESSION_TITLE` in `crates/proto`: the longest title, in characters.
    static let maxSessionTitle = 120
    /// Change one of the agent's session settings (model, effort, …).
    static let sessionSetConfig = "session.set_config"
    /// Ask a side question (`/btw`) in a fork of the conversation.
    static let sessionSideAsk = "session.side_ask"
    /// The host's connectors (names and icons, for approvals).
    static let connectorList = "connector.list"
    /// The login new sessions in a workspace start with.
    static let workspaceSetDefaultAgent = "workspace.set_default_agent"
    static let eventsSubscribe = "events.subscribe"
    static let eventsUnsubscribe = "events.unsubscribe"
    static let approvalList = "approval.list"
    static let approvalRespond = "approval.respond"
    static let deviceList = "device.list"
    /// New workspaces inside the project folders the desktop allows.
    static let projectBrowse = "project.browse"
    static let projectCreate = "project.create"

    // Notifications (host -> client)
    static let nEvent = "event"
    static let nSessionDelta = "session.delta"
    static let nSessionChanged = "session.changed"
    static let nDeviceRevoked = "device.revoked"

    /// Hosts on this protocol version or newer take attachments
    /// (`upload.*`, `session.prompt` `attachments`) and voice input (`voice.*`).
    static let mediaProtocolVersion: UInt32 = 2
    /// `session.prompt` takes at most this many attachments.
    static let maxAttachments = 10
    /// The largest file `RemoteClient.uploadFile` sends.
    static let maxUploadBytes: Int64 = 25 * 1024 * 1024
}

/// Event types, as in `EventType` in `crates/proto/src/types.rs`.
enum EventKind {
    static let sessionState = "session.state"
    static let prompt = "prompt"
    static let agentMessage = "agent.message"
    static let agentThought = "agent.thought"
    static let toolCall = "tool.call"
    static let toolResult = "tool.result"
    static let approvalRequested = "approval.requested"
    static let approvalResolved = "approval.resolved"
    static let netDenied = "net.denied"
    static let turnEnd = "turn.end"
    /// `{side_id, text}`: a side question (`/btw`) was asked.
    static let sideQuestion = "side.question"
    /// `{side_id, text}`: its answer (streamed as `session.delta` with `side_id`).
    static let sideMessage = "side.message"
    /// `{side_id, stop_reason, error?}`: the side answer is complete.
    static let sideEnd = "side.end"
    /// `{parent_tool_call_id, text}`: what a subagent said (not part of
    /// the main reply). Its tool calls are `tool.call`/`tool.result` with
    /// `parent_tool_call_id`.
    static let subagentMessage = "subagent.message"
}

/// `SessionState` values.
enum SessionStateName {
    static let created = "created"
    static let starting = "starting"
    static let idle = "idle"
    static let running = "running"
    static let waitingApproval = "waiting_approval"
    static let ended = "ended"
    static let failed = "failed"

    static func isTerminal(_ s: String) -> Bool { s == ended || s == failed }
    static func isBusy(_ s: String) -> Bool { s == running || s == waitingApproval }

    /// The status shown for a session state (see `SessionStatus`).
    static func label(_ s: String) -> String {
        SessionStatus(state: s).label
    }
}

/// The one status vocabulary, the same in the list and the session:
/// Working (spinner), Needs you (orange), Failed (red), Your turn (tint
/// dot: the agent finished its turn and waits for you), Ended (grey).
enum SessionStatus: Equatable {
    case working
    case needsYou
    case failed
    case yourTurn
    case ended

    init(state: String) {
        switch state {
        case SessionStateName.created, SessionStateName.starting, SessionStateName.running: self = .working
        case SessionStateName.waitingApproval: self = .needsYou
        case SessionStateName.failed: self = .failed
        case SessionStateName.ended: self = .ended
        default: self = .yourTurn
        }
    }

    var label: String {
        switch self {
        case .working: return "Working"
        case .needsYou: return "Needs approval"
        case .failed: return "Failed"
        case .yourTurn: return "Your turn"
        case .ended: return "Ended"
        }
    }
}

func agentDisplayName(_ agent: String) -> String {
    switch agent {
    case "codex": return "Codex"
    case "claude": return "Claude Code"
    case "opencode": return "OpenCode"
    case "cursor": return "Cursor"
    case "gemini": return "Gemini CLI"
    case "fake": return "Fake agent"
    default: return agent
    }
}

func dateFromMillis(_ ms: Int64) -> Date {
    Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
}

// MARK: - Models (field names as the host sends them)

struct SessionInfo: Codable, Identifiable, Hashable {
    var sessionId: String
    var workspaceId: String
    var agent: String
    /// The agent profile the session runs as (nil: the default login).
    var profileId: String?
    var state: String
    var stateReason: String?
    var title: String?
    var createdAt: Int64
    var updatedAt: Int64
    var lastSeq: Int64
    /// When it was archived (hidden from the list); archived sessions have
    /// ended. Absent on older hosts.
    var archivedAt: Int64? = nil
    /// The session's own git worktree (sessions in a git workspace each work
    /// on a branch of their own). Absent on older hosts.
    var worktree: SessionWorktree? = nil
    /// The agent's settings (model, effort, …); nil or empty when it reports none.
    var configOptions: [SessionConfigOption]? = nil
    /// The agent answers side questions (`/btw`, `session.side_ask`).
    var sideQuestions: Bool? = nil

    var id: String { sessionId }
    var isArchived: Bool { archivedAt != nil }
    var branch: String? { worktree.flatMap { $0.branch.isEmpty ? nil : $0.branch } }
    /// The branch without the host's "agenthost/" prefix ("fix-flaky-resume").
    var shortBranch: String? {
        guard let b = branch else { return nil }
        let prefix = "agenthost/"
        return b.hasPrefix(prefix) && b.count > prefix.count ? String(b.dropFirst(prefix.count)) : b
    }
    var canAskSideQuestions: Bool { sideQuestions ?? false }

    /// The setting the composer shows for a category (`model`, `thought_level`).
    func configOption(category: String) -> SessionConfigOption? {
        configOptions?.first { $0.category == category && !$0.options.isEmpty }
    }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case workspaceId = "workspace_id"
        case profileId = "profile_id"
        case agent, state
        case stateReason = "state_reason"
        case title
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case lastSeq = "last_seq"
        case archivedAt = "archived_at"
        case worktree
        case configOptions = "config_options"
        case sideQuestions = "side_questions"
    }
}

struct SessionWorktree: Codable, Hashable {
    var path: String
    var branch: String
}

/// One of an agent's session settings (ACP `configOptions`, `select` kind).
struct SessionConfigOption: Codable, Hashable, Identifiable {
    var id: String
    var name: String
    var description: String?
    /// `model`, `thought_level` (effort), `mode`, …
    var category: String?
    var currentValue: String
    var options: [SessionConfigValue]

    var currentName: String {
        options.first { $0.value == currentValue }?.name ?? currentValue
    }

    enum CodingKeys: String, CodingKey {
        case id, name, description, category, options
        case currentValue = "current_value"
    }
}

struct SessionConfigValue: Codable, Hashable, Identifiable {
    var value: String
    var name: String
    var description: String?

    var id: String { value }
}

/// What the app needs of a connector (`connector.list`): its name and icon.
struct ConnectorInfo: Codable, Identifiable, Hashable {
    var connectorId: String
    var name: String
    /// A `data:image/png;base64,…` URL.
    var icon: String?

    var id: String { connectorId }

    enum CodingKeys: String, CodingKey {
        case connectorId = "connector_id"
        case name, icon
    }
}

/// `session.side_ask` result.
struct SideAskResultModel: Codable {
    var sideId: String

    enum CodingKeys: String, CodingKey {
        case sideId = "side_id"
    }
}

/// One side conversation (`/btw`), built from the session's `side.*` events.
struct SideConversation: Identifiable, Equatable {
    struct Turn: Equatable {
        var question: String
        var answer = ""
        /// `side.end` arrived for this question.
        var done = false
        var error: String?
        /// Sent from this phone, not yet seen as `side.question`.
        var pending = false
    }

    var id: String
    var turns: [Turn]

    /// The first question, for the tab.
    var title: String { turns.first?.question ?? "Side question" }
    var isAnswering: Bool { turns.last.map { !$0.done && $0.error == nil } ?? false }
}

/// Side conversations from a session's events, oldest first.
func buildSideConversations(_ events: [HostEvent]) -> [SideConversation] {
    var list: [SideConversation] = []
    var index: [String: Int] = [:]
    for e in events {
        guard e.type.hasPrefix("side."), let sid = e.payload["side_id"].string else { continue }
        let text = e.payload["text"].string ?? ""
        switch e.type {
        case EventKind.sideQuestion:
            if let i = index[sid] {
                list[i].turns.append(.init(question: text))
            } else {
                index[sid] = list.count
                list.append(SideConversation(id: sid, turns: [.init(question: text)]))
            }
        case EventKind.sideMessage:
            guard let i = index[sid], !list[i].turns.isEmpty else { continue }
            list[i].turns[list[i].turns.count - 1].answer += text
        case EventKind.sideEnd:
            guard let i = index[sid], !list[i].turns.isEmpty else { continue }
            let last = list[i].turns.count - 1
            list[i].turns[last].done = true
            let stop = e.payload["stop_reason"].string ?? "end_turn"
            if let err = e.payload["error"].string, !err.isEmpty {
                list[i].turns[last].error = err
            } else if stop == "error" || stop == "refusal" {
                list[i].turns[last].error = stop == "error" ? "The answer failed." : "The agent refused."
            }
        default:
            break
        }
    }
    return list
}

/// One subfolder from `project.browse`.
struct ProjectFolderInfo: Codable, Identifiable, Hashable {
    var name: String
    var path: String
    /// Set when the folder is already a workspace.
    var workspaceId: String?

    var id: String { path }

    enum CodingKeys: String, CodingKey {
        case name, path
        case workspaceId = "workspace_id"
    }
}

/// `project.browse`: the project folders (no `path`), or one folder's subfolders.
struct ProjectBrowseInfo: Codable, Hashable {
    var roots: [String]
    var path: String?
    var parent: String?
    var folders: [ProjectFolderInfo]
}

struct WorkspaceInfo: Codable, Identifiable, Hashable {
    var workspaceId: String
    var name: String
    var path: String
    var kind: String
    var createdAt: Int64
    /// The login new sessions here start with (set by the host on the first
    /// session; `defaultProfileId` nil: the agent's default login). Absent on
    /// older hosts.
    var defaultAgent: String? = nil
    var defaultProfileId: String? = nil

    var id: String { workspaceId }

    /// The built-in "Quick tasks": no project folder (a fresh one per session).
    var isQuickTasks: Bool { kind == "scratch" }

    enum CodingKeys: String, CodingKey {
        case workspaceId = "workspace_id"
        case name, path, kind
        case createdAt = "created_at"
        case defaultAgent = "default_agent"
        case defaultProfileId = "default_profile_id"
    }
}

struct AgentStatusInfo: Codable, Hashable, Identifiable {
    var agent: String
    var displayName: String
    var installed: Bool
    var version: String?
    var loggedIn: Bool
    var available: Bool
    var problem: String?
    var fix: String?
    /// Which login: the agent's name for its default one, or a profile id
    /// (absent on hosts before agent profiles).
    var profileId: String?
    /// "Work" for a profile; nil for the default login.
    var profileName: String?
    var configDir: String?

    var id: String { profileId ?? agent }

    /// "Claude Code" or "Claude Code · Work".
    var label: String {
        profileName.map { "\(displayName) · \($0)" } ?? displayName
    }

    enum CodingKeys: String, CodingKey {
        case agent
        case displayName = "display_name"
        case installed, version
        case loggedIn = "logged_in"
        case available, problem, fix
        case profileId = "profile_id"
        case profileName = "profile_name"
        case configDir = "config_dir"
    }
}

struct SandboxHealthInfo: Codable, Hashable {
    var backend: String
    var ok: Bool
    var runtimeVersion: String?
    var problem: String?

    enum CodingKeys: String, CodingKey {
        case backend, ok
        case runtimeVersion = "runtime_version"
        case problem
    }
}

struct RelayStatusInfo: Codable, Hashable {
    var configured: Bool
    var connected: Bool
    var url: String?
    var error: String?
}

struct HostInfoModel: Codable, Hashable {
    var hostId: String
    var version: String
    var os: String
    var protocolVersion: UInt32
    var agents: [AgentStatusInfo]
    var sandbox: SandboxHealthInfo
    var relay: RelayStatusInfo

    enum CodingKeys: String, CodingKey {
        case hostId = "host_id"
        case version, os
        case protocolVersion = "protocol_version"
        case agents, sandbox, relay
    }
}

struct HostEvent: Codable, Identifiable, Hashable {
    var sessionId: String
    var seq: Int64
    var type: String
    var payload: JSONValue
    var ts: Int64

    var id: Int64 { seq }

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case seq, type, payload, ts
    }
}

struct SessionDeltaModel: Codable {
    var sessionId: String
    var kind: String
    var text: String
    /// The side conversation a `side.message` chunk belongs to.
    var sideId: String?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case sideId = "side_id"
        case kind, text
    }
}

struct PendingApprovalModel: Codable, Identifiable, Hashable {
    var approvalId: String
    var sessionId: String
    var connectorId: String
    var connectorName: String
    var tool: String
    var arguments: JSONValue
    var requestedAt: Int64
    var expiresAt: Int64
    /// The host's one-line description, when it has one ("Install
    /// agent-desktop 1.3.1 with npm, on this computer").
    var summary: String?

    var id: String { approvalId }

    /// An agent asking to install a tool on the computer (the host's own
    /// tool, not a connector): runs as the user, outside the sandbox.
    var isInstall: Bool { connectorId == "agenthost" }

    private func argument(_ key: String) -> String? {
        guard case let .object(o) = arguments else { return nil }
        return o[key]?.string
    }

    /// The exact command an install runs.
    var command: String? { argument("command") }
    /// Why the agent asks.
    var reason: String? { argument("reason").flatMap { $0.isEmpty ? nil : $0 } }
    /// What the approve button does ("Install", "Push"); older hosts only
    /// ever asked to install.
    var approveLabel: String { argument("approve_label") ?? "Install" }
    /// What to watch for before approving.
    var note: String {
        argument("note") ?? "The package manager may run the package's install scripts with your permissions. Install only what you expected."
    }
    /// The sheet's title ("Install a Tool?", "Push to GitHub?").
    var askTitle: String { argument("ask_title") ?? "Install a Tool?" }

    /// One line for lists and banners.
    var listLine: String { summary ?? "\(connectorName) · \(tool)" }

    enum CodingKeys: String, CodingKey {
        case approvalId = "approval_id"
        case sessionId = "session_id"
        case connectorId = "connector_id"
        case connectorName = "connector_name"
        case tool, arguments, summary
        case requestedAt = "requested_at"
        case expiresAt = "expires_at"
    }
}

struct DeviceModel: Codable, Identifiable, Hashable {
    var deviceId: String
    var name: String
    var capabilities: [String]
    var createdAt: Int64
    var lastSeenAt: Int64?
    var revokedAt: Int64?

    var id: String { deviceId }

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case name, capabilities
        case createdAt = "created_at"
        case lastSeenAt = "last_seen_at"
        case revokedAt = "revoked_at"
    }
}

struct PromptResultModel: Codable {
    var seq: Int64
    var accepted: Bool
}

/// A file uploaded to the host for a prompt (`upload.finish` result, and the
/// entries of a `prompt` event's `attachments`).
struct AttachmentInfo: Codable, Hashable, Identifiable {
    var attachmentId: String
    var name: String
    var mime: String
    var size: Int64

    var id: String { attachmentId }
    var isImage: Bool { mime.hasPrefix("image/") }

    enum CodingKeys: String, CodingKey {
        case attachmentId = "attachment_id"
        case name, mime, size
    }

    /// The `attachments` array of an event payload. Read field by field
    /// because `JSONValue` keeps numbers as `Double`.
    static func list(from value: JSONValue) -> [AttachmentInfo] {
        guard case let .array(items) = value else { return [] }
        return items.compactMap { item in
            guard let id = item["attachment_id"].string else { return nil }
            return AttachmentInfo(
                attachmentId: id,
                name: item["name"].string ?? "Attachment",
                mime: item["mime"].string ?? "application/octet-stream",
                size: item["size"].int ?? 0
            )
        }
    }
}

/// "1.2 MB", for attachment sizes.
func formatBytes(_ bytes: Int64) -> String {
    ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
}

struct SubscribeResultModel: Codable {
    var subscriptionId: String
    var replayed: Int

    enum CodingKeys: String, CodingKey {
        case subscriptionId = "subscription_id"
        case replayed
    }
}

// MARK: - Errors from the Rust layer

extension AgentHostError {
    /// Text suitable for showing to the user.
    var userMessage: String {
        let k = protocolConstants()
        switch self {
        case let .Rpc(code, detail, _):
            if code == k.errorCapabilityDenied {
                return "This phone is not allowed to do that. Use the desktop app. (\(detail))"
            }
            if code == k.errorUpdateRequired {
                return "Update required: this app is older than the host. (\(detail))"
            }
            return detail
        case let .Closed(code, reason):
            if code == k.closeDeviceRevoked { return "This device was removed." }
            if code == k.closeHostOffline { return "The host is offline." }
            if code == k.closeNotAllowed {
                return "Not allowed: this device is unknown to the host, or the pairing code was already used or has expired."
            }
            return reason.isEmpty ? "Connection closed (\(code))." : reason
        case let .Pairing(detail):
            return "Pairing failed: \(detail)"
        case let .InvalidInput(detail):
            return detail
        case let .Transport(detail):
            return "Connection failed: \(detail)"
        case .NotConnected:
            return "Not connected."
        }
    }
}

func userMessage(for error: Error) -> String {
    if let e = error as? AgentHostError { return e.userMessage }
    return error.localizedDescription
}
