import CryptoKit
import Foundation
import Security

// Compiled into both the app and the notification service extension
// (`AgentHostNotifications`): the key the host encrypts each notification's
// content with, and the code that opens it.

/// The decrypted content of a notification: `PushContent` in
/// `crates/proto/src/types.rs`.
struct PushContent: Codable, Equatable {
    /// `approval`, `turn_done`, `failed`, `automation` (title: the
    /// automation's name; opens the run's session like the others) or `test`.
    var kind: String
    var title: String
    var body: String
    var sessionId: String?
    var approvalId: String?

    enum CodingKeys: String, CodingKey {
        case kind, title, body
        case sessionId = "session_id"
        case approvalId = "approval_id"
    }
}

/// Keys in a delivered notification's `userInfo`, set by the extension.
enum PushUserInfo {
    static let sessionId = "session_id"
    static let approvalId = "approval_id"
    static let kind = "kind"
}

enum PushCryptoError: Error {
    case malformed
}

/// The APNs payload carries `"v": 1` and `"e"`: base64url (no padding) of a
/// 12-byte nonce, the ciphertext and a 16-byte tag, ChaCha20-Poly1305 with
/// the phone's key and the AAD "agenthost-push-v1". Apple only sees the
/// generic alert next to it.
enum PushCrypto {
    static let version = 1
    static let associatedData = Data("agenthost-push-v1".utf8)

    static func decrypt(_ sealed: String, key: Data) throws -> PushContent {
        guard key.count == 32,
              let combined = base64URLDecode(sealed),
              combined.count > 12 + 16
        else { throw PushCryptoError.malformed }
        let box = try ChaChaPoly.SealedBox(combined: combined)
        let plain = try ChaChaPoly.open(box, using: SymmetricKey(data: key), authenticating: associatedData)
        return try JSONDecoder().decode(PushContent.self, from: plain)
    }

    static func base64URLEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func base64URLDecode(_ text: String) -> Data? {
        var s = text
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let rest = s.count % 4
        if rest == 1 { return nil }
        if rest > 0 { s += String(repeating: "=", count: 4 - rest) }
        return Data(base64Encoded: s)
    }
}

/// The 32-byte notification key, in the Keychain access group the app and
/// the extension share (`$(AppIdentifierPrefix)dev.agenthost.shared`).
///
/// Separate from the device credentials and never behind Face ID:
/// `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, because notifications
/// arrive while the iPhone is locked. It only opens notification content.
enum PushKeyStore {
    private static let service = "dev.agenthost.push"
    private static let account = "content-key"

    /// "<team id>.dev.agenthost.shared", from `AppIdentifierPrefix` in
    /// Info.plist (nil when the build was not signed with a team).
    static var accessGroup: String? {
        guard let prefix = Bundle.main.object(forInfoDictionaryKey: "AppIdentifierPrefix") as? String,
              prefix.count > 1, !prefix.hasPrefix("$(")
        else { return nil }
        let p = prefix.hasSuffix(".") ? prefix : prefix + "."
        return p + "dev.agenthost.shared"
    }

    private static func baseQuery() -> [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        if let group = accessGroup { q[kSecAttrAccessGroup as String] = group }
        return q
    }

    static func load() -> Data? {
        var q = baseQuery()
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess,
              let data = out as? Data, data.count == 32
        else { return nil }
        return data
    }

    /// The stored key, or a new random one (saved first).
    static func loadOrCreate() throws -> Data {
        if let key = load() { return key }
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw PushKeyStoreError.keychain(errSecAllocate)
        }
        let key = Data(bytes)
        delete()
        var attrs = baseQuery()
        attrs[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        attrs[kSecValueData as String] = key
        let status = SecItemAdd(attrs as CFDictionary, nil)
        guard status == errSecSuccess else { throw PushKeyStoreError.keychain(status) }
        return key
    }

    static func delete() {
        SecItemDelete(baseQuery() as CFDictionary)
    }
}

enum PushKeyStoreError: LocalizedError {
    case keychain(OSStatus)

    var errorDescription: String? {
        switch self {
        case let .keychain(status): return "Keychain error \(status) while saving the notification key."
        }
    }
}
