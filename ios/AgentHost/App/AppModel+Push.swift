import Foundation
import UIKit
import UserNotifications

/// Push notifications (protocol 4). The host holds the APNs key
/// (`push.set_key`) and sends each notification with its content encrypted
/// to this iPhone's key (`PushKeyStore`), which the notification extension
/// opens; Apple only sees "A session needs you". The phone registers its
/// APNs token on every connection while notifications are on.
extension AppModel {
    static let notificationsEnabledKey = "notificationsEnabled"
    static let pushPrefsKey = "pushPrefs"
    /// Set by `didPair`: offer notifications after the first connection.
    static let offerPushKey = "offerPushAfterPairing"

    static var storedPushPrefs: PushPrefsModel {
        guard let data = UserDefaults.standard.data(forKey: pushPrefsKey),
              let prefs = try? JSONDecoder().decode(PushPrefsModel.self, from: data)
        else { return PushPrefsModel() }
        return prefs
    }

    var hostSupportsPush: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.pushProtocolVersion
    }

    /// iOS lets AgentHost show notifications (or hasn't been asked yet).
    var pushAllowedBySystem: Bool {
        switch pushAuthorization {
        case .denied: return false
        default: return true
        }
    }

    /// Settings → Notifications row: "On" or "Off".
    var notificationsSummary: String {
        notificationsEnabled && pushAllowedBySystem ? "On" : "Off"
    }

    // MARK: App facts

    /// The APNs topic: this app's bundle id.
    static var pushTopic: String {
        Bundle.main.bundleIdentifier ?? "dev.agenthost.ios"
    }

    /// The Apple team id this build is signed with (`AppIdentifierPrefix` in
    /// Info.plist without its trailing dot), or nil for an unsigned build.
    static var appTeamId: String? {
        guard var prefix = Bundle.main.object(forInfoDictionaryKey: "AppIdentifierPrefix") as? String,
              !prefix.hasPrefix("$(")
        else { return nil }
        if prefix.hasSuffix(".") { prefix.removeLast() }
        return prefix.count == 10 ? prefix : nil
    }

    /// The APNs server this build's tokens belong to: Debug builds are
    /// development-signed (sandbox); otherwise the embedded provisioning
    /// profile's `aps-environment` decides (App Store builds have none:
    /// production).
    static var apnsEnvironment: String {
        #if DEBUG
        return "sandbox"
        #else
        return profileApsEnvironment == "development" ? "sandbox" : "production"
        #endif
    }

    /// `Entitlements.aps-environment` from embedded.mobileprovision (a
    /// CMS-signed plist; the plist itself is plain text inside).
    private static var profileApsEnvironment: String? {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url),
              let text = String(data: data, encoding: .isoLatin1),
              let start = text.range(of: "<?xml"),
              let end = text.range(of: "</plist>", range: start.lowerBound..<text.endIndex)
        else { return nil }
        let plist = Data(text[start.lowerBound..<end.upperBound].utf8)
        guard let dict = try? PropertyListSerialization.propertyList(from: plist, format: nil) as? [String: Any],
              let ents = dict["Entitlements"] as? [String: Any]
        else { return nil }
        return ents["aps-environment"] as? String
    }

    // MARK: Status

    func refreshPushStatus() async {
        guard hostSupportsPush else {
            pushStatus = nil
            return
        }
        #if DEBUG
        if isDemo { return }
        #endif
        guard link.isOnline else { return }
        if let st: PushStatusModel = try? await call(RPC.pushStatus, [:]) {
            applyPushStatus(st)
        }
    }

    func refreshPushAuthorization() async {
        #if DEBUG
        if isDemo { return }
        #endif
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        pushAuthorization = settings.authorizationStatus
    }

    private func applyPushStatus(_ st: PushStatusModel) {
        pushStatus = st
        if let prefs = st.thisDevice?.prefs, prefs != pushPrefs {
            storePushPrefs(prefs)
        }
    }

    private func storePushPrefs(_ prefs: PushPrefsModel) {
        pushPrefs = prefs
        if isDemo { return }
        if let data = try? JSONEncoder().encode(prefs) {
            UserDefaults.standard.set(data, forKey: Self.pushPrefsKey)
        }
    }

    private func setNotificationsEnabled(_ on: Bool) {
        notificationsEnabled = on
        if isDemo { return }
        UserDefaults.standard.set(on, forKey: Self.notificationsEnabledKey)
    }

    /// End of `didConnect` (after `refreshAll` read `push.status`): renew the
    /// registration while notifications are on, drop one left over from when
    /// they were turned off offline, and offer them once after pairing.
    func didConnectPush() async {
        guard hostSupportsPush else {
            UserDefaults.standard.removeObject(forKey: Self.offerPushKey)
            return
        }
        #if DEBUG
        if isDemo { return }
        #endif
        await refreshPushAuthorization()
        if notificationsEnabled {
            // Tokens can change; the host keeps one registration per device.
            if pushAuthorization != .denied {
                _ = try? await registerPush()
            }
        } else if pushStatus?.thisDevice != nil {
            if let st: PushStatusModel = try? await call(RPC.pushUnregister, [:]) {
                applyPushStatus(st)
            }
        }
        if UserDefaults.standard.bool(forKey: Self.offerPushKey) {
            UserDefaults.standard.removeObject(forKey: Self.offerPushKey)
            if !notificationsEnabled { showPushOnboarding = true }
        }
    }

    // MARK: Turning notifications on and off

    enum PushEnableResult {
        case enabled(PushStatusModel)
        /// iOS (or the user, earlier) said no: on in AgentHost, off in iOS
        /// Settings until they allow it there.
        case denied
    }

    /// Ask iOS (the first time), get the APNs token and register it.
    func enablePush() async throws -> PushEnableResult {
        #if DEBUG
        if isDemo {
            setNotificationsEnabled(true)
            let st = pushStatus ?? PushStatusModel(configured: false)
            return .enabled(st)
        }
        #endif
        let granted = try await UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound, .badge])
        await refreshPushAuthorization()
        setNotificationsEnabled(true)
        guard granted else { return .denied }
        do {
            return .enabled(try await registerPush())
        } catch {
            setNotificationsEnabled(false)
            throw error
        }
    }

    /// Off: the host forgets this iPhone's token. Off locally even when the
    /// host can't be reached; the next connection unregisters then.
    func disablePush() async throws {
        setNotificationsEnabled(false)
        #if DEBUG
        if isDemo { return }
        #endif
        guard link.isOnline else { return }
        let st: PushStatusModel = try await call(RPC.pushUnregister, [:])
        applyPushStatus(st)
    }

    /// `push.register` with the current token, key and prefs. Idempotent.
    @discardableResult
    func registerPush() async throws -> PushStatusModel {
        let token = try await remoteToken()
        let key = try PushKeyStore.loadOrCreate()
        let st: PushStatusModel = try await call(RPC.pushRegister, [
            "token": token.map { String(format: "%02x", $0) }.joined(),
            "topic": Self.pushTopic,
            "environment": Self.apnsEnvironment,
            "key": PushCrypto.base64URLEncode(key),
            "prefs": pushPrefs.params,
        ])
        applyPushStatus(st)
        return st
    }

    /// Change what this iPhone is notified about. Shown at once; put back if
    /// the host refuses.
    func setPushPrefs(_ prefs: PushPrefsModel) async throws {
        let before = pushPrefs
        storePushPrefs(prefs)
        #if DEBUG
        if isDemo { return }
        #endif
        do {
            let st: PushStatusModel = try await call(RPC.pushSetPrefs, ["prefs": prefs.params])
            applyPushStatus(st)
        } catch {
            if pushPrefs == prefs { storePushPrefs(before) }
            throw error
        }
    }

    /// Give the host its APNs key (the .p8 file's text).
    func setPushKey(p8: String, keyId: String, teamId: String) async throws {
        #if DEBUG
        if isDemo {
            try await Task.sleep(nanoseconds: 400_000_000)
            pushStatus = PushStatusModel(configured: true, keyId: keyId, teamId: teamId,
                                         thisDevice: pushStatus?.thisDevice)
            return
        }
        #endif
        let st: PushStatusModel = try await call(RPC.pushSetKey, [
            "key_p8": p8,
            "key_id": keyId,
            "team_id": teamId,
        ])
        applyPushStatus(st)
    }

    /// Send a test notification to this iPhone; a sentence saying how it went.
    func sendTestPush() async throws -> String {
        #if DEBUG
        if isDemo { return "Sent. It should arrive in a few seconds." }
        #endif
        let r: PushTestResultModel = try await call(RPC.pushTest, [:])
        await refreshPushStatus()
        if r.sent > 0 && r.errors.isEmpty {
            return "Sent. It should arrive in a few seconds."
        }
        let reasons = r.errors.map(Self.describeAPNsError)
        return reasons.isEmpty ? "Not sent." : reasons.joined(separator: " ")
    }

    /// An APNs reason (`BadDeviceToken`, …) in words.
    static func describeAPNsError(_ reason: String) -> String {
        switch reason {
        case "BadDeviceToken":
            return "Apple refused this iPhone's token (BadDeviceToken): the app's build and the APNs server don't match. Reinstall from Xcode or TestFlight and try again."
        case "InvalidProviderToken":
            return "Apple refused the key (InvalidProviderToken): check the key ID and the team ID."
        case "TopicDisallowed", "DeviceTokenNotForTopic":
            return "The APNs key belongs to another Apple Developer team than this app (\(reason))."
        case "Unregistered":
            return "This iPhone's token is no longer valid (Unregistered). Turn notifications off and on again."
        case "ExpiredProviderToken", "TooManyProviderTokenUpdates":
            return "Apple refused the host's sign-in (\(reason)). Try again in a minute."
        default:
            return reason
        }
    }

    // MARK: The APNs key file

    /// The key id in a key file's name (`AuthKey_<KEYID>.p8`).
    static func apnsKeyId(fromFileName name: String) -> String? {
        guard let r = name.range(of: "AuthKey_[A-Z0-9]{10}\\.p8", options: .regularExpression) else { return nil }
        return String(name[r].dropFirst("AuthKey_".count).prefix(10))
    }

    /// Read a picked .p8 file (downloading it first if it is only in iCloud).
    static func readAPNsKeyFile(_ url: URL) throws -> String {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        var coordinationError: NSError?
        var result: Result<String, Error> = .failure(PushError.unreadable)
        NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordinationError) { u in
            do {
                let data = try Data(contentsOf: u)
                guard data.count < 16 * 1024, let text = String(data: data, encoding: .utf8) else {
                    throw PushError.notAKey
                }
                result = .success(text)
            } catch {
                result = .failure(error)
            }
        }
        if let coordinationError { throw coordinationError }
        let text = try result.get()
        guard text.contains("-----BEGIN PRIVATE KEY-----") else { throw PushError.notAKey }
        return text
    }

    // MARK: APNs token

    /// The APNs device token. Asks iOS each time (cheap; tokens can change)
    /// and waits for `AppDelegate` to pass it on, at most 20 s.
    func remoteToken() async throws -> Data {
        let id = UUID()
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Data, Error>) in
            let first = pushTokenWaiters.isEmpty
            pushTokenWaiters[id] = cont
            if first {
                UIApplication.shared.registerForRemoteNotifications()
            }
            Task { [weak self] in
                try? await Task.sleep(nanoseconds: 20_000_000_000)
                self?.pushTokenWaiters.removeValue(forKey: id)?.resume(throwing: PushError.noToken)
            }
        }
    }

    /// From `AppDelegate`.
    func didRegisterForRemoteNotifications(token: Data) {
        let changed = pushToken != nil && pushToken != token
        pushToken = token
        let waiting = !pushTokenWaiters.isEmpty
        finishTokenWaiters(.success(token))
        // A new token outside a registration: tell the host.
        if changed, !waiting, notificationsEnabled, link.isOnline, hostSupportsPush {
            Task { _ = try? await registerPush() }
        }
    }

    /// From `AppDelegate`.
    func didFailToRegisterForRemoteNotifications(_ error: Error) {
        finishTokenWaiters(.failure(error))
    }

    private func finishTokenWaiters(_ result: Result<Data, Error>) {
        let waiters = pushTokenWaiters.values
        pushTokenWaiters = [:]
        for w in waiters { w.resume(with: result) }
    }

    // MARK: Delivered notifications

    /// The pairing step finished (turned on or skipped).
    func finishPushOnboarding() {
        showPushOnboarding = false
    }

    /// A notification was tapped: open its session.
    func openFromNotification(_ userInfo: [AnyHashable: Any]) {
        guard let sid = userInfo[PushUserInfo.sessionId] as? String, !sid.isEmpty else { return }
        openSessionRequest = sid
    }

    /// Whether a notification for this session should stay quiet (the user
    /// is looking at it).
    func isShowing(sessionId: String?) -> Bool {
        guard let sessionId else { return false }
        return visibleSessionId == sessionId && UIApplication.shared.applicationState == .active
    }

    /// A session is on screen: clear its notifications.
    func sessionShown(_ sessionId: String) {
        visibleSessionId = sessionId
        removeDeliveredNotifications(sessionId: sessionId)
    }

    func sessionHidden(_ sessionId: String) {
        if visibleSessionId == sessionId { visibleSessionId = nil }
    }

    func removeDeliveredNotifications(sessionId: String) {
        #if DEBUG
        if isDemo { return }
        #endif
        UNUserNotificationCenter.current().getDeliveredNotifications { list in
            let ids = list
                .filter { ($0.request.content.userInfo[PushUserInfo.sessionId] as? String) == sessionId }
                .map(\.request.identifier)
            if !ids.isEmpty {
                UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
            }
        }
    }

    /// Unpaired or removed: forget the notification key and the token, and
    /// clear what was shown (it came from that host).
    func forgetPush() {
        setNotificationsEnabled(false)
        UserDefaults.standard.removeObject(forKey: Self.pushPrefsKey)
        UserDefaults.standard.removeObject(forKey: Self.offerPushKey)
        pushPrefs = PushPrefsModel()
        pushStatus = nil
        showPushOnboarding = false
        visibleSessionId = nil
        PushKeyStore.delete()
        pushToken = nil
        finishTokenWaiters(.failure(PushError.noToken))
        UIApplication.shared.unregisterForRemoteNotifications()
        let center = UNUserNotificationCenter.current()
        center.removeAllDeliveredNotifications()
        center.removeAllPendingNotificationRequests()
    }
}

enum PushError: LocalizedError {
    case unreadable
    case notAKey
    case noToken

    var errorDescription: String? {
        switch self {
        case .unreadable: return "Could not read that file."
        case .notAKey: return "That is not an APNs key. Choose the AuthKey_….p8 file you downloaded from your Apple Developer account."
        case .noToken: return "iOS did not give AgentHost a notification token. Check that the app is signed with the Push Notifications capability, then try again."
        }
    }
}
