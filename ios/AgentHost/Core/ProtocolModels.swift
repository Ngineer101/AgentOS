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
    static let eventsSubscribe = "events.subscribe"
    static let eventsUnsubscribe = "events.unsubscribe"
    static let approvalList = "approval.list"
    static let approvalRespond = "approval.respond"
    static let deviceList = "device.list"

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

    static func label(_ s: String) -> String {
        switch s {
        case waitingApproval: return "Needs approval"
        default: return s.prefix(1).uppercased() + s.dropFirst()
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

    var id: String { sessionId }

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
    }
}

struct WorkspaceInfo: Codable, Identifiable, Hashable {
    var workspaceId: String
    var name: String
    var path: String
    var kind: String
    var createdAt: Int64

    var id: String { workspaceId }

    enum CodingKeys: String, CodingKey {
        case workspaceId = "workspace_id"
        case name, path, kind
        case createdAt = "created_at"
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

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
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

    var id: String { approvalId }

    enum CodingKeys: String, CodingKey {
        case approvalId = "approval_id"
        case sessionId = "session_id"
        case connectorId = "connector_id"
        case connectorName = "connector_name"
        case tool, arguments
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
