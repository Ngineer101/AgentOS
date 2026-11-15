import Foundation
import LocalAuthentication
import Security

/// Codable mirror of the generated `DeviceCredentials` record.
struct StoredCredentials: Codable, Equatable {
    var relayUrl: String
    var relayId: String
    var hostKey: String
    var hostName: String
    var deviceId: String
    var hostId: String
    var noisePrivate: String
    var relayPrivate: String

    init(_ c: DeviceCredentials) {
        relayUrl = c.relayUrl
        relayId = c.relayId
        hostKey = c.hostKey
        hostName = c.hostName
        deviceId = c.deviceId
        hostId = c.hostId
        noisePrivate = c.noisePrivate
        relayPrivate = c.relayPrivate
    }

    var ffi: DeviceCredentials {
        DeviceCredentials(
            relayUrl: relayUrl,
            relayId: relayId,
            hostKey: hostKey,
            hostName: hostName,
            deviceId: deviceId,
            hostId: hostId,
            noisePrivate: noisePrivate,
            relayPrivate: relayPrivate
        )
    }
}

enum CredentialStoreError: LocalizedError {
    case keychain(OSStatus, String)
    case crypto(String)
    case corrupt

    var errorDescription: String? {
        switch self {
        case let .keychain(status, what): return "Keychain error \(status) while \(what)."
        case let .crypto(what): return "Could not protect the device key: \(what)"
        case .corrupt: return "The stored device key is unreadable."
        }
    }
}

/// The device's keys, in the Keychain.
///
/// * The credentials (which include the device's Noise and relay private keys)
///   are stored as one generic-password item whose access control is
///   `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` + `.userPresence`: never
///   backed up or migrated, and released only after Face ID or the passcode.
/// * On hardware with a Secure Enclave, the item's contents are additionally
///   encrypted (ECIES, P-256, AES-GCM) to a Secure Enclave key created with
///   `.privateKeyUsage` + `.userPresence`. The private half never leaves the
///   Secure Enclave, so the credentials cannot be decrypted off this device.
/// * Where there is no Secure Enclave (the Simulator) the item is stored
///   without that extra layer (format byte 0).
///
/// Reads take the `LAContext` the lock screen already evaluated, so the user
/// sees one Face ID prompt, not three.
enum CredentialStore {
    private static let service = "dev.agenthost.ios.device"
    private static let account = "credentials"
    private static let wrapKeyTag = Data("dev.agenthost.ios.wrap-key".utf8)
    private static let algorithm: SecKeyAlgorithm = .eciesEncryptionCofactorVariableIVX963SHA256AESGCM

    private static let formatPlain: UInt8 = 0
    private static let formatSecureEnclave: UInt8 = 1

    // MARK: Public API

    static func save(_ creds: StoredCredentials) throws {
        let plain = try JSONEncoder().encode(creds)
        // A fresh wrapping key per pairing; the old item and key go first.
        deleteAll()
        var blob: Data
        if let pub = try? createWrapKey() {
            var error: Unmanaged<CFError>?
            guard let ct = SecKeyCreateEncryptedData(pub, algorithm, plain as CFData, &error) as Data? else {
                throw CredentialStoreError.crypto(describe(error))
            }
            blob = Data([formatSecureEnclave])
            blob.append(ct)
        } else {
            blob = Data([formatPlain])
            blob.append(plain)
        }
        guard let access = accessControl(flags: [.userPresence]) else {
            throw CredentialStoreError.crypto("access control")
        }
        let attrs: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrAccessControl as String: access,
            kSecValueData as String: blob,
        ]
        let status = SecItemAdd(attrs as CFDictionary, nil)
        guard status == errSecSuccess else { throw CredentialStoreError.keychain(status, "saving") }
    }

    /// Load the credentials, or nil when this device is not paired.
    /// `context` should be one that has just passed `deviceOwnerAuthentication`.
    static func load(context: LAContext) throws -> StoredCredentials? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
            kSecUseAuthenticationContext as String: context,
        ]
        var out: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &out)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let blob = out as? Data, let format = blob.first else {
            throw CredentialStoreError.keychain(status, "reading")
        }
        let body = blob.dropFirst()
        let plain: Data
        switch format {
        case formatSecureEnclave:
            guard let priv = wrapPrivateKey(context: context) else {
                throw CredentialStoreError.crypto("the Secure Enclave key is missing")
            }
            var error: Unmanaged<CFError>?
            guard let p = SecKeyCreateDecryptedData(priv, algorithm, Data(body) as CFData, &error) as Data? else {
                throw CredentialStoreError.crypto(describe(error))
            }
            plain = p
        case formatPlain:
            plain = Data(body)
        default:
            throw CredentialStoreError.corrupt
        }
        guard let creds = try? JSONDecoder().decode(StoredCredentials.self, from: plain) else {
            throw CredentialStoreError.corrupt
        }
        return creds
    }

    /// Whether credentials exist, without reading (or prompting for) them.
    static func exists() -> Bool {
        let context = LAContext()
        context.interactionNotAllowed = true
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecUseAuthenticationContext as String: context,
        ]
        let status = SecItemCopyMatching(query as CFDictionary, nil)
        return status == errSecSuccess || status == errSecInteractionNotAllowed
    }

    /// Remove the credentials and the Secure Enclave wrapping key.
    static func deleteAll() {
        deleteItem()
        let q: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: wrapKeyTag,
        ]
        SecItemDelete(q as CFDictionary)
    }

    // MARK: Internals

    private static func deleteItem() {
        let q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(q as CFDictionary)
    }

    private static func accessControl(flags: SecAccessControlCreateFlags) -> SecAccessControl? {
        var error: Unmanaged<CFError>?
        return SecAccessControlCreateWithFlags(
            kCFAllocatorDefault,
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            flags,
            &error
        )
    }

    /// Create the Secure Enclave wrapping key and return its public half.
    /// Throws where there is no Secure Enclave (the Simulator).
    private static func createWrapKey() throws -> SecKey {
        guard let access = accessControl(flags: [.privateKeyUsage, .userPresence]) else {
            throw CredentialStoreError.crypto("access control")
        }
        let attrs: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256,
            kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
            kSecPrivateKeyAttrs as String: [
                kSecAttrIsPermanent as String: true,
                kSecAttrApplicationTag as String: wrapKeyTag,
                kSecAttrAccessControl as String: access,
            ] as [String: Any],
        ]
        var error: Unmanaged<CFError>?
        guard let key = SecKeyCreateRandomKey(attrs as CFDictionary, &error),
              let pub = SecKeyCopyPublicKey(key) else {
            throw CredentialStoreError.crypto(describe(error))
        }
        return pub
    }

    private static func wrapPrivateKey(context: LAContext) -> SecKey? {
        let q: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: wrapKeyTag,
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecReturnRef as String: true,
            kSecUseAuthenticationContext as String: context,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let ref = out else {
            return nil
        }
        // A successful query with kSecReturnRef always yields a SecKey.
        return (ref as! SecKey)
    }

    private static func describe(_ error: Unmanaged<CFError>?) -> String {
        guard let e = error?.takeRetainedValue() else { return "unknown error" }
        return (e as Error).localizedDescription
    }
}
