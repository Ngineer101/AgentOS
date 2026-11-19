import Foundation

// Push notifications (`push.*`, protocol 4). Mirrors `PushStatus` and
// friends in `crates/proto/src/types.rs`.

extension RPC {
    static let pushStatus = "push.status"
    static let pushSetKey = "push.set_key"
    static let pushRegister = "push.register"
    static let pushUnregister = "push.unregister"
    static let pushSetPrefs = "push.set_prefs"
    static let pushTest = "push.test"
    /// Hosts on this protocol version or newer send push notifications.
    static let pushProtocolVersion: UInt32 = 4
}

/// What this iPhone is notified about.
struct PushPrefsModel: Codable, Hashable {
    /// A tool call waits for approval (on by default).
    var approvals = true
    /// The agent finished its turn.
    var turnDone = false
    /// A session failed.
    var failed = false

    enum CodingKeys: String, CodingKey {
        case approvals
        case turnDone = "turn_done"
        case failed
    }

    init(approvals: Bool = true, turnDone: Bool = false, failed: Bool = false) {
        self.approvals = approvals
        self.turnDone = turnDone
        self.failed = failed
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        approvals = try c.decodeIfPresent(Bool.self, forKey: .approvals) ?? true
        turnDone = try c.decodeIfPresent(Bool.self, forKey: .turnDone) ?? false
        failed = try c.decodeIfPresent(Bool.self, forKey: .failed) ?? false
    }

    /// As JSON-RPC params.
    var params: [String: Any] {
        ["approvals": approvals, "turn_done": turnDone, "failed": failed]
    }
}

struct PushDeviceModel: Codable, Hashable {
    var deviceId: String
    var name: String
    /// `sandbox` or `production`.
    var environment: String
    var prefs: PushPrefsModel
    var registeredAt: Int64
    /// The last time APNs refused a notification for it, and why.
    var lastError: String?

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case name, environment, prefs
        case registeredAt = "registered_at"
        case lastError = "last_error"
    }
}

struct PushStatusModel: Codable, Hashable {
    /// The host has an APNs key.
    var configured: Bool
    var keyId: String?
    var teamId: String?
    /// This iPhone's registration (nil: not registered).
    var thisDevice: PushDeviceModel?
    var devices: [PushDeviceModel] = []

    enum CodingKeys: String, CodingKey {
        case configured
        case keyId = "key_id"
        case teamId = "team_id"
        case thisDevice = "this_device"
        case devices
    }

    init(configured: Bool, keyId: String? = nil, teamId: String? = nil,
         thisDevice: PushDeviceModel? = nil, devices: [PushDeviceModel] = []) {
        self.configured = configured
        self.keyId = keyId
        self.teamId = teamId
        self.thisDevice = thisDevice
        self.devices = devices
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        configured = try c.decode(Bool.self, forKey: .configured)
        keyId = try c.decodeIfPresent(String.self, forKey: .keyId)
        teamId = try c.decodeIfPresent(String.self, forKey: .teamId)
        thisDevice = try c.decodeIfPresent(PushDeviceModel.self, forKey: .thisDevice)
        devices = try c.decodeIfPresent([PushDeviceModel].self, forKey: .devices) ?? []
    }
}

struct PushTestResultModel: Codable, Hashable {
    var sent: UInt32
    var errors: [String]

    init(sent: UInt32, errors: [String] = []) {
        self.sent = sent
        self.errors = errors
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sent = try c.decode(UInt32.self, forKey: .sent)
        errors = try c.decodeIfPresent([String].self, forKey: .errors) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case sent, errors
    }
}
