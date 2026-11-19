import Foundation

// Screen sharing (`screen.*`, protocol 3): the host's screen through
// screen-relay. Mirrors `ScreenStatus` and friends in `crates/proto`.

extension RPC {
    static let screenStatus = "screen.status"
    static let screenStart = "screen.start"
    static let screenStop = "screen.stop"
    static let nScreenChanged = "screen.changed"
    /// Hosts on this protocol version or newer can share their screen.
    static let screenProtocolVersion: UInt32 = 3
}

enum ScreenModeName {
    /// The display you are logged in to.
    static let desktop = "desktop"
    /// A separate desktop on the host with only a browser in it (Linux).
    static let browser = "browser"
}

struct ScreenPolicyModel: Codable, Hashable {
    var enabled: Bool
    var allowControl: Bool

    enum CodingKeys: String, CodingKey {
        case enabled
        case allowControl = "allow_control"
    }
}

struct ScreenModeInfoModel: Codable, Hashable, Identifiable {
    var mode: String
    var available: Bool
    var detail: String

    var id: String { mode }

    var title: String {
        mode == ScreenModeName.browser ? "Browser on Host" : "Your Desktop"
    }

    var systemImage: String {
        mode == ScreenModeName.browser ? "globe" : "desktopcomputer"
    }
}

struct ScreenRunningModel: Codable, Hashable {
    var mode: String
    var control: Bool
    var startedAt: Int64
    var startedBy: String
    var viewers: UInt32

    enum CodingKeys: String, CodingKey {
        case mode, control, viewers
        case startedAt = "started_at"
        case startedBy = "started_by"
    }
}

struct ScreenStatusModel: Codable, Hashable {
    var policy: ScreenPolicyModel
    var installed: Bool
    var modes: [ScreenModeInfoModel]
    var running: ScreenRunningModel?
    var error: String?
}

struct ScreenStartResultModel: Codable, Hashable {
    var ticket: String
    var expiresInMs: Int64
    var mode: String
    var control: Bool

    enum CodingKeys: String, CodingKey {
        case ticket, mode, control
        case expiresInMs = "expires_in_ms"
    }
}
