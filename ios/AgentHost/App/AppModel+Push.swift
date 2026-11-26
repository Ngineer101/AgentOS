import Foundation
import UIKit
import UserNotifications

/// Push notifications (protocol 4). Usually a relay sends them to Apple with
/// the APNs key of the team that signs this build: the phone registers its
/// APNs token with that relay and gives the host only the handle it gets
/// back (protocol 6). Otherwise the host holds an APNs key (`push.set_key`)
/// and sends to Apple itself. Either way each notification's content is
/// encrypted to this iPhone's key (`PushKeyStore`), which the notification
/// extension opens; Apple only sees "A session needs you". The phone
/// registers on every connection while notifications are on.
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

    /// The host can send through a relay (no APNs key on the host).
    var hostSupportsPushRelay: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.pushRelayProtocolVersion
    }

    /// iOS lets AgentOS show notifications (or hasn't been asked yet).
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

    /// The relay that sends this build's notifications to Apple:
    /// `AgentHostPushRelay` in Info.plist (set by whoever publishes the app,
    /// with their team's APNs key on that relay), else the relay this
    /// iPhone is paired through.
    var pushRelayURL: URL? {
        if let s = Bundle.main.object(forInfoDictionaryKey: "AgentHostPushRelay") as? String,
           !s.isEmpty, !s.hasPrefix("$("), let url = Self.httpURL(fromRelay: s) {
            return url
        }
        return credentials.flatMap { Self.httpURL(fromRelay: $0.relayUrl) }
    }

    /// `wss://relay.example` → `https://relay.example` (and `ws` → `http`).
    static func httpURL(fromRelay s: String) -> URL? {
        var t = s.trimmingCharacters(in: .whitespaces)
        while t.hasSuffix("/") { t.removeLast() }
        if t.hasPrefix("wss://") { t = "https://" + t.dropFirst(6) }
        else if t.hasPrefix("ws://") { t = "http://" + t.dropFirst(5) }
        guard let url = URL(string: t), url.scheme == "https" || url.scheme == "http", url.host != nil else { return nil }
        return url
    }

    enum RelayRegistration {
        case handle(url: String, handle: String)
        /// Why the relay didn't take it, in words.
        case refused(String)
        /// No relay to ask (a host older than protocol 6, or not paired).
        case notAsked
    }

    /// `POST /v1/push/register` on the relay: this iPhone's token, sealed by
    /// the relay for this host only. When the relay doesn't send for this
    /// build (or can't be reached) the host needs its own key instead.
    private func relayHandle(token: String) async -> RelayRegistration {
        guard hostSupportsPushRelay, let base = pushRelayURL, let relayId = credentials?.relayId else { return .notAsked }
        let name = base.host ?? base.absoluteString
        var req = URLRequest(url: base.appendingPathComponent("v1/push/register"))
        req.httpMethod = "POST"
        req.timeoutInterval = 15
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try? JSONSerialization.data(withJSONObject: [
            "token": token,
            "topic": Self.pushTopic,
            "environment": Self.apnsEnvironment,
            "relay_id": relayId,
        ])
        let reply: (Data, URLResponse)
        do {
            reply = try await URLSession.shared.data(for: req)
        } catch {
            return .refused("couldn’t reach \(name) (\(error.localizedDescription))")
        }
        let status = (reply.1 as? HTTPURLResponse)?.statusCode ?? 0
        let obj = (try? JSONSerialization.jsonObject(with: reply.0)) as? [String: Any]
        if status == 200, let handle = obj?["handle"] as? String, !handle.isEmpty {
            return .handle(url: base.absoluteString, handle: handle)
        }
        if status == 404 {
            return .refused("\(name) doesn’t send notifications (it has no APNs key)")
        }
        if let message = obj?["error"] as? String, !message.isEmpty {
            // "this relay sends …" → "relay.example sends …".
            let said = message.hasPrefix("this relay") ? String(message.dropFirst("this relay".count)) : " said: " + message
            return .refused(name + said)
        }
        return .refused("\(name) answered HTTP \(status)")
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
        /// iOS (or the user, earlier) said no: on in AgentOS, off in iOS
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

    /// `push.register` with the current token (or the relay's handle for
    /// it), key and prefs. Idempotent.
    @discardableResult
    func registerPush() async throws -> PushStatusModel {
        let token = try await remoteToken().map { String(format: "%02x", $0) }.joined()
        let key = try PushKeyStore.loadOrCreate()
        var params: [String: Any] = [
            "topic": Self.pushTopic,
            "environment": Self.apnsEnvironment,
            "key": PushCrypto.base64URLEncode(key),
            "prefs": pushPrefs.params,
        ]
        switch await relayHandle(token: token) {
        case let .handle(url, handle):
            params["gateway"] = ["url": url, "handle": handle]
            pushRelayProblem = nil
        case let .refused(why):
            params["token"] = token
            pushRelayProblem = why
        case .notAsked:
            params["token"] = token
            pushRelayProblem = nil
        }
        let st: PushStatusModel = try await call(RPC.pushRegister, params)
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
        // "<device name>: <why>": this iPhone tested only itself, so drop the name.
        let reasons = r.errors.map { e -> String in
            guard let i = e.range(of: ": ") else { return Self.describeAPNsError(e) }
            return Self.describeAPNsError(String(e[i.upperBound...]))
        }
        return reasons.isEmpty ? "Not sent." : reasons.joined(separator: " ")
    }

    /// An APNs reason (`BadDeviceToken`, …) in words.
    static func describeAPNsError(_ reason: String) -> String {
        switch reason {
        case "BadDeviceToken":
            return "Apple refused this iPhone's token (BadDeviceToken): the app's build and the APNs server don't match. Reinstall from Xcode or TestFlight and try again."
        case "InvalidProviderToken":
            return "Apple doesn’t accept the APNs key (InvalidProviderToken). Check that the key is still listed under Certificates, IDs & Profiles → Keys on developer.apple.com (a revoked key disappears from it), then the key ID, the team ID, and that the key is for Sandbox & Production."
        case "BadEnvironmentKeyInToken":
            return "The APNs key only works in the other environment (BadEnvironmentKeyInToken). Builds from Xcode use the sandbox, TestFlight and App Store builds production. Use a key for Sandbox & Production."
        case "RelayKeyInvalid":
            return "The relay’s APNS_KEY secret isn’t a .p8 key (RelayKeyInvalid). Paste the whole AuthKey_….p8 file into it, including the BEGIN and END lines."
        case "TopicDisallowed", "DeviceTokenNotForTopic":
            return "The APNs key belongs to another Apple Developer team than this app (\(reason))."
        case "Unregistered":
            return "This iPhone's token is no longer valid (Unregistered). Turn notifications off and on again."
        case "ExpiredProviderToken", "TooManyProviderTokenUpdates":
            return "Apple refused the host's sign-in (\(reason)). Try again in a minute."
        case "NoAPNsKey":
            return "The host has no APNs key, so nothing was sent."
        case "BadHandle", "PushUnavailable":
            return "The relay that sends notifications no longer takes this iPhone's registration (\(reason)). Turn notifications off and on again."
        case "BadSignature":
            return "The relay that sends notifications didn't accept the host's signature (BadSignature). Check that the host's clock is right."
        case "TooManyRequests":
            return "The host sent too many notifications in a minute; the relay held some back."
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
        pushRelayProblem = nil
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
        case .noToken: return "iOS did not give AgentOS a notification token. Check that the app is signed with the Push Notifications capability, then try again."
        }
    }
}
