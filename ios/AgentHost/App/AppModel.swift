import Foundation
import LocalAuthentication
import Network
import SwiftUI
import UserNotifications

/// Where the app is, top level.
enum AppPhase: Equatable {
    /// Nothing is shown yet: the keys are being loaded (after Face ID or the
    /// passcode, when "Require Face ID" is on).
    case locked
    /// No device keys: show the Pair screen.
    case unpaired
    /// Paired; showing sessions (online or from the cache).
    case ready
    /// The host revoked this device; keys and cache are gone.
    case removed
}

/// The tunnel to the host.
enum LinkState: Equatable {
    case disconnected
    case connecting
    case online
    /// The relay says the host is not connected (4404). Retrying.
    case hostOffline
    /// No network or the relay is unreachable. Retrying.
    case offline(String)
    /// `hello` refused this app's protocol version. Not retrying.
    case updateRequired(String)

    var isOnline: Bool { self == .online }

    var label: String {
        switch self {
        case .disconnected: return "Disconnected"
        case .connecting: return "Connecting…"
        case .online: return "Connected"
        case .hostOffline: return "Host offline"
        case .offline: return "Offline"
        case .updateRequired: return "Update required"
        }
    }
}

/// A prompt the user sent that the host has not acknowledged yet. The same
/// `promptId` is reused on every retry, so the host records it once.
struct OutgoingPrompt: Identifiable, Equatable {
    var id: String
    var sessionId: String
    var text: String
    /// Already uploaded; sent by id with the prompt.
    var attachments: [AttachmentInfo] = []
    var failure: String?
}

struct SessionGroup: Identifiable {
    var workspaceId: String
    var workspaceName: String
    var sessions: [SessionInfo]
    var id: String { workspaceId }
}

/// Streaming text for the turn in progress (`session.delta`), replaced by the
/// coalesced `agent.message` / `agent.thought` event when it arrives.
struct StreamingText: Equatable {
    var kind: String
    var text: String
}

/// Receives callbacks from the Rust layer (on its threads) and forwards them,
/// in order, to the main actor. `generation` drops callbacks from an old
/// connection after a reconnect.
final class ListenerBridge: ConnectionListener, @unchecked Sendable {
    private weak var model: AppModel?
    private let generation: Int

    init(model: AppModel, generation: Int) {
        self.model = model
        self.generation = generation
    }

    func onNotification(method: String, paramsJson: String) {
        let gen = generation
        DispatchQueue.main.async { [weak model = self.model] in
            MainActor.assumeIsolated {
                model?.handleNotification(method: method, paramsJson: paramsJson, generation: gen)
            }
        }
    }

    func onClosed(reason: String, code: UInt16) {
        let gen = generation
        DispatchQueue.main.async { [weak model = self.model] in
            MainActor.assumeIsolated {
                model?.handleClosed(reason: reason, code: code, generation: gen)
            }
        }
    }
}

@MainActor
final class AppModel: ObservableObject {
    // MARK: Published state

    @Published private(set) var phase: AppPhase = .locked
    @Published private(set) var link: LinkState = .disconnected {
        didSet { updateConnectionStatus() }
    }
    /// Whether to show the connection card. Not for a connect that is going
    /// normally (every launch and return to the app): only once it has
    /// failed or is taking a while, and then until it is back online, so the
    /// card doesn't come and go with each retry.
    @Published private(set) var showsConnectionStatus = false
    @Published var lockError: String?
    /// Ask for Face ID (or the passcode) when opening the app. Off by default.
    @Published private(set) var requireUnlock = UserDefaults.standard.bool(forKey: AppModel.requireUnlockKey)

    @Published private(set) var hostInfo: HostInfoModel?
    @Published private(set) var workspaces: [WorkspaceInfo] = []
    /// The sessions in the list (not archived).
    @Published private(set) var sessions: [SessionInfo] = []
    /// Archived sessions, loaded when Settings → Archived Sessions is shown
    /// (nil until then). Not cached.
    @Published private(set) var archivedSessions: [SessionInfo]?
    /// Sessions on their way to the archive: out of the list at once, before
    /// the host answers. (It ends a live session first, and that "ended"
    /// would otherwise move the row to the top just before it goes.) Back in
    /// the list if archiving fails.
    @Published private(set) var archiving: Set<String> = []
    @Published private(set) var approvals: [PendingApprovalModel] = []
    @Published private(set) var deviceName: String?
    @Published private(set) var transcripts: [String: Transcript] = [:]
    @Published private(set) var streaming: [String: StreamingText] = [:]
    @Published private(set) var outbox: [OutgoingPrompt] = []
    /// Unsent text in each session's composer, kept while you look at
    /// another session. Not published: only the session's own view reads it.
    var drafts: [String: String] = [:]
    /// Each session's composer attachments (pasted screenshots, photos,
    /// files) for the same reason; uploads still running carry on.
    private var composers: [String: ComposerModel] = [:]
    /// Set when the cache was written last (shown when offline).
    @Published private(set) var cachedAt: Date?
    /// The host's protocol version from `hello` on the last connection.
    @Published private(set) var hostProtocolVersion: UInt32?
    /// The host's connectors, for their icons (`connector.list`; empty on
    /// hosts that send none).
    @Published private(set) var connectors: [ConnectorInfo] = []
    /// What the host can show and whether this phone may watch (protocol 3).
    @Published var screenStatus: ScreenStatusModel?
    /// The host's automations (protocol 5), loaded when Settings →
    /// Automations is shown (nil until then) and kept current by
    /// `automation.changed`. Not cached. See AppModel+Automations.swift.
    @Published var automations: [AutomationModel]?
    /// The host's notes (protocol 9), loaded when Notes is shown (nil until
    /// then) and kept current by `note.changed`. Not cached. See
    /// AppModel+Notes.swift.
    @Published var notes: [NoteModel]?
    /// Streaming side answers (`session.delta` with a `side_id`), by side id;
    /// replaced by the coalesced `side.message` event.
    @Published private(set) var sideStreaming: [String: String] = [:]
    /// Side questions sent from this phone that the host has not echoed as
    /// `side.question` yet, by side id.
    @Published private(set) var sidePending: [String: [String]] = [:]
    /// Side conversations the user closed (this run of the app).
    @Published private(set) var closedSides: Set<String> = []

    // Push notifications (protocol 4; see AppModel+Push.swift).
    /// `push.status` from the host (nil before it answers, or on older hosts).
    @Published var pushStatus: PushStatusModel?
    /// The user wants notifications on this iPhone (stored locally; the
    /// registration is renewed on every connection while on).
    @Published var notificationsEnabled = UserDefaults.standard.bool(forKey: AppModel.notificationsEnabledKey)
    /// What this iPhone is notified about (sent with every `push.register`).
    @Published var pushPrefs = AppModel.storedPushPrefs
    /// Whether iOS lets AgentOS show notifications.
    @Published var pushAuthorization: UNAuthorizationStatus = .notDetermined
    /// The pairing flow's "Notifications" step, after the first connection.
    @Published var showPushOnboarding = false
    /// A session to open from a notification (SessionsView navigates there).
    @Published var openSessionRequest: String?
    /// The session on screen: its notifications are not shown while it is.
    var visibleSessionId: String?
    /// The APNs device token, once iOS gave it (this run of the app).
    var pushToken: Data?
    /// `remoteToken()` callers waiting for iOS.
    var pushTokenWaiters: [UUID: CheckedContinuation<Data, Error>] = [:]

    // MARK: Private state

    private(set) var credentials: StoredCredentials?
    private var client: RemoteClient?
    private var generation = 0
    private var connectTask: Task<Void, Never>?
    /// Shows the connection card if a connect is still going after a moment.
    private var connectGraceTask: Task<Void, Never>?
    private var attempt = 0
    /// Sessions subscribed on the current connection.
    private var subscribed: Set<String> = []
    private var subscriptionIds: [String: String] = [:]
    /// Sessions the user opened; re-subscribed on every connection.
    private var watched: Set<String> = []
    private var saveTask: Task<Void, Never>?
    private var dirtyTranscripts: Set<String> = []
    private var pathMonitor: NWPathMonitor?
    private var networkAvailable = true
    /// The network interface in use (Wi-Fi, cellular), to notice a switch.
    private var networkInterface: NWInterface.InterfaceType?
    private var unlocking = false
    private let constants = protocolConstants()
    /// Decoded connector icons, by connector id.
    private var iconCache: [String: UIImage] = [:]
    /// The session of each side id in `sidePending`.
    private var pendingSession: [String: String] = [:]
    /// Sample data for screenshots (DEBUG builds, `-AgentHostDemo`): no
    /// keys, no network.
    private(set) var isDemo = false
    private var demoHostName: String?

    init() {
        #if DEBUG
        if let demo = DemoData.fromLaunchArguments() {
            loadDemo(demo)
        }
        #endif
    }

    // MARK: Derived

    /// The paired host's name (the large title).
    var hostName: String {
        credentials?.hostName ?? demoHostName ?? "Sessions"
    }

    /// Sessions in a workspace that have not ended.
    func liveSessionCount(_ workspaceId: String) -> Int {
        sessions.filter { $0.workspaceId == workspaceId && !SessionStateName.isTerminal($0.state) }.count
    }

    func isQuickTasks(_ workspaceId: String) -> Bool {
        workspaces.first { $0.workspaceId == workspaceId }?.isQuickTasks ?? false
    }

    var anyWaitingApproval: Bool {
        sessions.contains { $0.state == SessionStateName.waitingApproval } || !approvals.isEmpty
    }

    /// Attachments and voice input: hosts on protocol 2 or newer. Uses the
    /// cached host info until the first connection.
    var hostSupportsMedia: Bool {
        (hostProtocolVersion ?? hostInfo?.protocolVersion ?? 0) >= RPC.mediaProtocolVersion
    }

    /// "Claude Code", or "Claude Code · Work" for a session on a profile.
    func agentLabel(_ s: SessionInfo) -> String {
        guard let pid = s.profileId,
              let a = hostInfo?.agents.first(where: { $0.profileId == pid })
        else { return agentDisplayName(s.agent) }
        return a.label
    }

    /// The agent and the login a session runs as: ("Claude Code", "Personal"),
    /// or a nil login for the agent's default one. A profile named after its
    /// agent ("Claude-Personal") loses that prefix.
    func agentAndLogin(_ s: SessionInfo) -> (agent: String, login: String?) {
        let info = s.profileId.flatMap { pid in hostInfo?.agents.first { $0.profileId == pid } }
        let agent = info?.displayName ?? agentDisplayName(s.agent)
        guard let raw = info?.profileName?.trimmingCharacters(in: .whitespaces), !raw.isEmpty else {
            return (agent, nil)
        }
        var login = raw
        for prefix in [s.agent, agent, agent.split(separator: " ").first.map(String.init) ?? ""] where !prefix.isEmpty {
            guard login.count > prefix.count, login.lowercased().hasPrefix(prefix.lowercased()) else { continue }
            let rest = login.dropFirst(prefix.count)
            if let sep = rest.first, "-_ ·:".contains(sep) {
                let trimmed = rest.dropFirst().trimmingCharacters(in: .whitespaces)
                if !trimmed.isEmpty { login = trimmed; break }
            }
        }
        return (agent, login)
    }

    /// "Claude Code (Personal)", or "Claude Code" for the default login.
    func agentLoginLabel(_ s: SessionInfo) -> String {
        let (agent, login) = agentAndLogin(s)
        return login.map { "\(agent) (\($0))" } ?? agent
    }

    /// What a session is called, everywhere it is shown: its title, the
    /// same on every device (the host names it from its first message on),
    /// or "New session" before then.
    func sessionTitle(_ s: SessionInfo) -> String {
        if let t = s.title?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty { return t }
        return "New session"
    }

    func workspaceName(_ id: String) -> String {
        workspaces.first { $0.workspaceId == id }?.name ?? "Workspace"
    }

    func session(_ id: String) -> SessionInfo? {
        sessions.first { $0.sessionId == id } ?? archivedSessions?.first { $0.sessionId == id }
    }

    func workspace(_ id: String) -> WorkspaceInfo? {
        workspaces.first { $0.workspaceId == id }
    }

    /// A connector's icon, by id (or by name when the id is unknown).
    func connectorIcon(id: String?, name: String? = nil) -> UIImage? {
        guard let c = connectors.first(where: { $0.connectorId == id })
            ?? name.flatMap({ n in connectors.first { $0.name == n } }),
            let url = c.icon
        else { return nil }
        if let cached = iconCache[c.connectorId] { return cached }
        guard let comma = url.firstIndex(of: ","), url[..<comma].hasSuffix(";base64"),
              let data = Data(base64Encoded: String(url[url.index(after: comma)...])),
              let image = UIImage(data: data)
        else { return nil }
        iconCache[c.connectorId] = image
        return image
    }

    /// A session's side conversations (`/btw`) that are not closed: from its
    /// events, plus questions on their way and answers still streaming.
    func sideConversations(_ sessionId: String) -> [SideConversation] {
        var list = buildSideConversations(transcripts[sessionId]?.events ?? [])
        let known = Set(list.map(\.id))
        for (sid, questions) in sidePending where !questions.isEmpty {
            // Pending side ids are only kept for this session's asks.
            guard pendingSession[sid] == sessionId else { continue }
            let turns = questions.map { SideConversation.Turn(question: $0, pending: true) }
            if let i = list.firstIndex(where: { $0.id == sid }) {
                list[i].turns += turns
            } else if !known.contains(sid) {
                list.append(SideConversation(id: sid, turns: turns))
            }
        }
        for i in list.indices {
            if let text = sideStreaming[list[i].id], !text.isEmpty,
               let last = list[i].turns.indices.last, !list[i].turns[last].done {
                list[i].turns[last].answer += text
            }
        }
        return list.filter { !closedSides.contains($0.id) }
    }

    func closeSide(_ sideId: String) {
        closedSides.insert(sideId)
    }

    /// Sessions grouped by workspace: Quick tasks first, then by name so groups
    /// stay put while agents work; newest first inside each.
    var groupedSessions: [SessionGroup] {
        let listed = sessions.filter { !archiving.contains($0.sessionId) }
        let groups = Dictionary(grouping: listed, by: { $0.workspaceId })
        return groups
            .map { entry in
                SessionGroup(
                    workspaceId: entry.key,
                    workspaceName: workspaceName(entry.key),
                    sessions: entry.value.sorted { $0.updatedAt > $1.updatedAt }
                )
            }
            .sorted {
                let q0 = isQuickTasks($0.workspaceId), q1 = isQuickTasks($1.workspaceId)
                if q0 != q1 { return q0 }
                let order = $0.workspaceName.localizedStandardCompare($1.workspaceName)
                if order != .orderedSame { return order == .orderedAscending }
                return $0.workspaceId < $1.workspaceId
            }
    }

    // MARK: Lock and unlock

    private static let requireUnlockKey = "requireUnlock"

    /// "Face ID", "Touch ID", "Optic ID", or "Passcode" on a phone without biometrics.
    static var unlockMethodName: String {
        let context = LAContext()
        _ = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        switch context.biometryType {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "Passcode"
        }
    }

    /// Open the app: load the keys straight away, or after Face ID or the
    /// passcode when "Require Face ID" is on (or the keys still need it).
    func unlock() async {
        guard phase == .locked, !unlocking else { return }
        unlocking = true
        defer { unlocking = false }
        if !requireUnlock {
            let quiet = LAContext()
            quiet.interactionNotAllowed = true
            do {
                finishUnlock(try CredentialStore.load(context: quiet))
                return
            } catch CredentialStoreError.keychain(errSecInteractionNotAllowed, _) {
                // Saved while Face ID was required (an earlier version): ask
                // once more, then save them without it.
            } catch {
                lockError = userMessage(for: error)
                return
            }
        }
        let context = LAContext()
        context.localizedCancelTitle = "Cancel"
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            lockError = "Set a passcode on this iPhone to use AgentOS. (\(error?.localizedDescription ?? "unavailable"))"
            return
        }
        do {
            let ok = try await context.evaluatePolicy(
                .deviceOwnerAuthentication,
                localizedReason: "Unlock AgentOS to see your sessions."
            )
            guard ok else { return }
        } catch {
            lockError = error.localizedDescription
            return
        }
        lockError = nil
        do {
            let creds = try CredentialStore.load(context: context)
            if let creds, !requireUnlock {
                try? CredentialStore.save(creds, requireUserPresence: false)
            }
            finishUnlock(creds)
        } catch {
            lockError = userMessage(for: error)
        }
    }

    private func finishUnlock(_ creds: StoredCredentials?) {
        lockError = nil
        if let creds {
            credentials = creds
            loadCache()
            phase = .ready
            startMonitoringNetwork()
            connect()
        } else {
            // Not paired: make sure nothing from an earlier pairing is left.
            TranscriptCache.deleteAll()
            SentImages.shared.clearMemory()
            AgentFiles.deleteAll()
            phase = UserDefaults.standard.bool(forKey: Self.removedFlag) ? .removed : .unpaired
        }
    }

    /// Turn "Require Face ID" on or off. Either way Face ID (or the passcode)
    /// confirms it first, so someone holding the unlocked phone can't switch
    /// the lock off; the keys are then saved again with the new protection.
    func setRequireUnlock(_ on: Bool) async throws {
        guard on != requireUnlock else { return }
        let context = LAContext()
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            throw CredentialStoreError.crypto(
                "set a passcode on this iPhone first (\(error?.localizedDescription ?? "unavailable"))")
        }
        let name = Self.unlockMethodName
        guard try await context.evaluatePolicy(
            .deviceOwnerAuthentication,
            localizedReason: on ? "Turn on \(name) for AgentOS." : "Turn off \(name) for AgentOS."
        ) else { return }
        if let creds = credentials {
            do {
                try CredentialStore.save(creds, requireUserPresence: on)
            } catch {
                // Saving replaces the old item: put it back as it was.
                try? CredentialStore.save(creds, requireUserPresence: !on)
                throw error
            }
        }
        UserDefaults.standard.set(on, forKey: Self.requireUnlockKey)
        requireUnlock = on
    }

    /// The app went to the background: lock when Face ID is required,
    /// otherwise just drop the connection (it is reopened on return).
    func enterBackground() {
        if isDemo { return }
        if requireUnlock {
            lock()
        } else if phase == .ready {
            saveNow()
            stopConnection()
        }
    }

    /// Back in the foreground without a lock: reconnect.
    func enterForeground() {
        if isDemo { return }
        guard phase == .ready else { return }
        connect()
        Task { await refreshPushAuthorization() }
        if let sid = visibleSessionId { removeDeliveredNotifications(sessionId: sid) }
    }

    /// Hide everything and drop the connection and keys from memory.
    func lock() {
        guard phase != .locked else { return }
        saveNow()
        stopConnection()
        stopMonitoringNetwork()
        credentials = nil
        phase = .locked
    }

    /// Remembers (outside the Keychain, holding no secret) that the host
    /// removed this device, so the notice survives locking the app.
    private static let removedFlag = "deviceWasRemoved"

    // MARK: Pairing

    /// Store freshly paired credentials and connect.
    func didPair(_ creds: DeviceCredentials) throws {
        let stored = StoredCredentials(creds)
        try CredentialStore.save(stored, requireUserPresence: requireUnlock)
        UserDefaults.standard.removeObject(forKey: Self.removedFlag)
        // After the first connection: offer notifications (host protocol 4+).
        UserDefaults.standard.set(true, forKey: Self.offerPushKey)
        // Locked meanwhile: the keys are saved; the next unlock loads them.
        guard phase == .unpaired else { return }
        TranscriptCache.deleteAll()
        SentImages.shared.clearMemory()
        AgentFiles.deleteAll()
        clearContent()
        credentials = stored
        phase = .ready
        startMonitoringNetwork()
        connect()
    }

    /// Forget this host on this phone. The desktop should also revoke the
    /// device (a phone cannot call `device.revoke`).
    func unpair() {
        // Best effort: tell the host to stop sending notifications here. The
        // call keeps the old connection (taken out of the model, so `wipe`
        // doesn't close it) and closes it when answered.
        if notificationsEnabled || pushStatus?.thisDevice != nil, link.isOnline, let c = client {
            client = nil
            Task.detached {
                _ = try? await c.call(method: RPC.pushUnregister, paramsJson: "{}")
                c.disconnect()
            }
        }
        wipe()
        phase = .unpaired
    }

    func pairAgain() {
        wipe()
        UserDefaults.standard.removeObject(forKey: Self.removedFlag)
        phase = .unpaired
    }

    private func wipe() {
        stopConnection()
        stopMonitoringNetwork()
        CredentialStore.deleteAll()
        TranscriptCache.deleteAll()
        SentImages.shared.clearMemory()
        AgentFiles.deleteAll()
        credentials = nil
        clearContent()
        forgetPush()
    }

    private func clearContent() {
        saveTask?.cancel()
        drafts = [:]
        for c in composers.values { c.clear() }
        composers = [:]
        hostInfo = nil
        workspaces = []
        sessions = []
        archivedSessions = nil
        automations = nil
        notes = nil
        approvals = []
        deviceName = nil
        transcripts = [:]
        streaming = [:]
        outbox = []
        connectors = []
        iconCache = [:]
        sideStreaming = [:]
        sidePending = [:]
        pendingSession = [:]
        closedSides = []
        pushStatus = nil
        openSessionRequest = nil
        hostProtocolVersion = nil
        watched = []
        subscribed = []
        subscriptionIds = [:]
        dirtyTranscripts = []
        cachedAt = nil
    }

    /// "This device was removed": delete keys and cache, keep nothing readable.
    private func deviceRemoved() {
        wipe()
        UserDefaults.standard.set(true, forKey: Self.removedFlag)
        phase = .removed
    }

    // MARK: Connection lifecycle

    func connect() {
        guard phase == .ready, credentials != nil, connectTask == nil else { return }
        if case .updateRequired = link { return }
        connectTask = Task { [weak self] in
            await self?.connectLoop(initialDelay: 0)
        }
    }

    /// Retry at once (pull to refresh, or the network came back).
    func retryNow() {
        guard phase == .ready, !link.isOnline else { return }
        connectTask?.cancel()
        connectTask = nil
        attempt = 0
        connect()
    }

    /// Drop the current connection and connect again at once (the network
    /// changed under it, so the old socket is probably dead).
    private func reconnectNow() {
        guard phase == .ready else { return }
        stopConnection()
        attempt = 0
        connect()
    }

    private func stopConnection() {
        connectTask?.cancel()
        connectTask = nil
        generation += 1
        client?.disconnect()
        client = nil
        subscribed = []
        subscriptionIds = [:]
        streaming = [:]
        link = .disconnected
    }

    private func updateConnectionStatus() {
        switch link {
        case .online:
            connectGraceTask?.cancel()
            connectGraceTask = nil
            showsConnectionStatus = false
        case .connecting:
            guard !showsConnectionStatus, connectGraceTask == nil else { return }
            connectGraceTask = Task { [weak self] in
                try? await Task.sleep(nanoseconds: 4_000_000_000)
                guard let self, !Task.isCancelled else { return }
                self.connectGraceTask = nil
                if self.link == .connecting { self.showsConnectionStatus = true }
            }
        case .disconnected:
            // Between connections (backgrounded, network changed): keep
            // whatever is shown until the next attempt says otherwise.
            break
        case .hostOffline, .offline, .updateRequired:
            connectGraceTask?.cancel()
            connectGraceTask = nil
            showsConnectionStatus = true
        }
    }

    private func scheduleReconnect() {
        guard phase == .ready, connectTask == nil else { return }
        attempt += 1
        // 1, 2, 4, 8, 16, 30, 30… seconds, with jitter. While the host is
        // offline (restarting, asleep) keep checking every few seconds so the
        // app is back within moments of it.
        let cap = link == .hostOffline ? 4.0 : 30.0
        let base = min(cap, pow(2.0, Double(max(0, attempt - 1))))
        let delay = base * Double.random(in: 0.8...1.2)
        connectTask = Task { [weak self] in
            await self?.connectLoop(initialDelay: delay)
        }
    }

    private func connectLoop(initialDelay: Double) async {
        if initialDelay > 0 {
            try? await Task.sleep(nanoseconds: UInt64(initialDelay * 1_000_000_000))
        }
        guard !Task.isCancelled else { return }
        guard phase == .ready, let creds = credentials else {
            connectTask = nil
            return
        }
        generation += 1
        let gen = generation
        link = .connecting
        do {
            let c = try await RemoteClient.connect(
                credentials: creds.ffi,
                listener: ListenerBridge(model: self, generation: gen)
            )
            guard !Task.isCancelled, gen == generation, phase == .ready else {
                // Superseded (locked, retried or wiped) while connecting.
                c.disconnect()
                return
            }
            client = c
            hostProtocolVersion = c.hello().protocolVersion
            subscribed = []
            subscriptionIds = [:]
            link = .online
            attempt = 0
            connectTask = nil
            await didConnect()
        } catch let e as AgentHostError {
            // A cancelled attempt was superseded; its replacement owns the state.
            guard !Task.isCancelled, gen == generation else { return }
            connectTask = nil
            switch e {
            case let .Closed(code, _) where code == constants.closeDeviceRevoked || code == constants.closeNotAllowed:
                // The relay refuses keys the host no longer knows: the device
                // was removed (perhaps while this phone was away).
                deviceRemoved()
                return
            case let .Closed(code, _) where code == constants.closeHostOffline:
                link = .hostOffline
            case let .Rpc(code, detail, _) where code == constants.errorUpdateRequired:
                link = .updateRequired(detail)
                return
            default:
                link = .offline(e.userMessage)
            }
            scheduleReconnect()
        } catch {
            guard !Task.isCancelled, gen == generation else { return }
            connectTask = nil
            link = .offline(error.localizedDescription)
            scheduleReconnect()
        }
    }

    /// Snapshot everything, re-subscribe, and resend unacknowledged prompts.
    private func didConnect() async {
        await refreshAll()
        let live = sessions.filter { !SessionStateName.isTerminal($0.state) }.map(\.sessionId)
        for sid in watched.union(live) {
            await subscribe(sid)
        }
        for p in outbox where p.failure == nil {
            await deliver(p)
        }
        await didConnectPush()
    }

    // MARK: Projects

    /// The project folders (`path` nil), or the subfolders of one.
    func browseProjects(_ path: String?) async throws -> ProjectBrowseInfo {
        #if DEBUG
        if isDemo { return DemoData.browse(path) }
        #endif
        var params: [String: Any] = [:]
        if let path { params["path"] = path }
        return try await call(RPC.projectBrowse, params)
    }

    /// Make a folder inside a project folder a workspace (`path`), or create
    /// the new folder `name` in `parent` first.
    func createProject(path: String? = nil, parent: String? = nil, name: String? = nil) async throws -> WorkspaceInfo {
        var params: [String: Any] = [:]
        if let path { params["path"] = path }
        if let parent { params["parent"] = parent }
        if let name { params["name"] = name }
        let w: WorkspaceInfo = try await call(RPC.projectCreate, params)
        if let list: [WorkspaceInfo] = try? await call(RPC.workspaceList, [:]) {
            workspaces = list
        }
        return w
    }

    func refreshAll() async {
        guard link.isOnline else { return }
        if let info: HostInfoModel = try? await call(RPC.hostInfo, [:]) {
            hostInfo = info
        }
        if let ws: [WorkspaceInfo] = try? await call(RPC.workspaceList, [:]) {
            workspaces = ws
        }
        if let list: [SessionInfo] = try? await call(RPC.sessionList, ["watch": true]) {
            sessions = list.filter { !$0.isArchived }
        }
        if archivedSessions != nil {
            await refreshArchived()
        }
        if let devices: [DeviceModel] = try? await call(RPC.deviceList, [:]),
           let me = devices.first(where: { $0.deviceId == credentials?.deviceId }) {
            deviceName = me.name
        }
        await refreshApprovals()
        await refreshScreen()
        await refreshPushStatus()
        if automations != nil {
            await refreshAutomations()
        }
        if notes != nil {
            await refreshNotes()
        }
        if let list: [ConnectorInfo] = try? await call(RPC.connectorList, [:]) {
            connectors = list
            iconCache = [:]
        }
        scheduleSave()
    }

    /// Load the archived sessions (Settings → Archived Sessions).
    func refreshArchived() async {
        #if DEBUG
        if isDemo { return }
        #endif
        if let list: [SessionInfo] = try? await call(RPC.sessionList, ["archived": true]) {
            archivedSessions = list.filter(\.isArchived).sorted { ($0.archivedAt ?? 0) > ($1.archivedAt ?? 0) }
        }
    }

    func refreshWorkspaces() async {
        if let list: [WorkspaceInfo] = try? await call(RPC.workspaceList, [:]) {
            workspaces = list
        }
    }

    func refreshApprovals() async {
        if let list: [PendingApprovalModel] = try? await call(RPC.approvalList, [:]) {
            approvals = list
        }
    }

    // MARK: Attachments

    /// Send a local file to the host for the next prompt. The file is read
    /// during the call, so a temporary copy can be deleted afterwards.
    func uploadFile(at url: URL, name: String, mime: String?) async throws -> AttachmentInfo {
        #if DEBUG
        if isDemo {
            let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
            try await Task.sleep(nanoseconds: 300_000_000)
            return AttachmentInfo(attachmentId: UUID().uuidString, name: name,
                                  mime: mime ?? "application/octet-stream", size: Int64(size))
        }
        #endif
        guard let c = client else { throw AgentHostError.NotConnected }
        let text = try await c.uploadFile(path: url.path(percentEncoded: false), name: name, mime: mime)
        return try JSONDecoder().decode(AttachmentInfo.self, from: Data(text.utf8))
    }

    // MARK: Calls

    /// One JSON-RPC call through the tunnel; the result decoded as `T`.
    func call<T: Decodable>(_ method: String, _ params: [String: Any]) async throws -> T {
        guard let c = client else { throw AgentHostError.NotConnected }
        let text = try await c.call(method: method, paramsJson: jsonText(params))
        return try JSONDecoder().decode(T.self, from: Data(text.utf8))
    }

    /// A call whose result is not needed.
    func send(_ method: String, _ params: [String: Any]) async throws {
        let _: JSONValue = try await call(method, params)
    }

    // MARK: Sessions

    /// The attachments being prepared in a session's composer, the same
    /// object each time the session is shown (not published: reading it
    /// from a view's body must not change anything the view watches).
    func composer(for sessionId: String) -> ComposerModel {
        if let c = composers[sessionId] { return c }
        let c = ComposerModel()
        composers[sessionId] = c
        return c
    }

    /// The user opened a session: subscribe (once per connection) from the
    /// last seq this device has.
    func open(_ sessionId: String) {
        // Archived sessions are read once, not followed after reconnects.
        if session(sessionId)?.isArchived != true {
            watched.insert(sessionId)
        }
        if transcripts[sessionId] == nil {
            transcripts[sessionId] = Transcript(sessionId: sessionId)
        }
        Task { await subscribe(sessionId) }
    }

    private func subscribe(_ sessionId: String, force: Bool = false) async {
        guard link.isOnline, force || !subscribed.contains(sessionId) else { return }
        subscribed.insert(sessionId)
        if transcripts[sessionId] == nil {
            transcripts[sessionId] = Transcript(sessionId: sessionId)
        }
        if force, let old = subscriptionIds.removeValue(forKey: sessionId) {
            try? await send(RPC.eventsUnsubscribe, ["subscription_id": old])
        }
        let after = transcripts[sessionId]?.lastSeq ?? 0
        do {
            let r: SubscribeResultModel = try await call(
                RPC.eventsSubscribe,
                ["session_id": sessionId, "after_seq": after]
            )
            subscriptionIds[sessionId] = r.subscriptionId
        } catch {
            subscribed.remove(sessionId)
        }
    }

    /// Start a session as one login of the agent (`profileId` from host info;
    /// nil on hosts before agent profiles). From a note (`note`, in the same
    /// workspace): the note lists the session, and its text waits in the
    /// session's composer to edit and send.
    func createSession(workspaceId: String, agent: String, profileId: String?, note: NoteModel? = nil) async throws -> SessionInfo {
        let note = note.flatMap { $0.workspaceId == workspaceId ? $0 : nil }
        #if DEBUG
        if isDemo {
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            let s = SessionInfo(sessionId: UUID().uuidString.lowercased(), workspaceId: workspaceId, agent: agent,
                                profileId: profileId, state: SessionStateName.idle, stateReason: nil, title: nil,
                                createdAt: now, updatedAt: now, lastSeq: 0)
            upsert(s)
            transcripts[s.sessionId] = Transcript(sessionId: s.sessionId)
            if let note {
                drafts[s.sessionId] = note.text
                noteStartedSession(note.noteId, sessionId: s.sessionId)
            }
            if let i = workspaces.firstIndex(where: { $0.workspaceId == workspaceId }),
               workspaces[i].defaultAgent == nil {
                workspaces[i].defaultAgent = agent
                workspaces[i].defaultProfileId = profileId == agent ? nil : profileId
            }
            return s
        }
        #endif
        var params: [String: Any] = ["workspace_id": workspaceId, "agent": agent]
        if let profileId { params["profile_id"] = profileId }
        if let note { params["note_id"] = note.noteId }
        let s: SessionInfo = try await call(RPC.sessionCreate, params)
        upsert(s)
        if let note {
            drafts[s.sessionId] = note.text
            noteStartedSession(note.noteId, sessionId: s.sessionId)
        }
        open(s.sessionId)
        // The host may have set the workspace's default login.
        Task { await refreshWorkspaces() }
        return s
    }

    func sendPrompt(sessionId: String, text: String, attachments: [AttachmentInfo] = []) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !attachments.isEmpty else { return }
        let p = OutgoingPrompt(
            id: UUID().uuidString.lowercased(),
            sessionId: sessionId,
            text: trimmed,
            attachments: attachments
        )
        outbox.append(p)
        Task { await deliver(p) }
    }

    func retryPrompt(_ id: String) {
        guard let i = outbox.firstIndex(where: { $0.id == id }) else { return }
        outbox[i].failure = nil
        let p = outbox[i]
        Task { await deliver(p) }
    }

    func discardPrompt(_ id: String) {
        outbox.removeAll { $0.id == id }
    }

    /// Send (or resend) one prompt with its fixed prompt id. Transport errors
    /// leave it queued for the next connection; host errors are shown.
    private func deliver(_ p: OutgoingPrompt) async {
        #if DEBUG
        if isDemo {
            let seq = (transcripts[p.sessionId]?.lastSeq ?? 0) + 1
            ingest(HostEvent(
                sessionId: p.sessionId, seq: seq, type: EventKind.prompt,
                payload: .object(["text": .string(p.text), "prompt_id": .string(p.id), "device": .string(deviceName ?? "iPhone")]),
                ts: Int64(Date().timeIntervalSince1970 * 1000)))
            return
        }
        #endif
        guard link.isOnline else { return }
        var params: [String: Any] = ["session_id": p.sessionId, "prompt_id": p.id, "text": p.text]
        if !p.attachments.isEmpty {
            params["attachments"] = p.attachments.map(\.attachmentId)
        }
        do {
            let _: PromptResultModel = try await call(RPC.sessionPrompt, params)
            // Accepted now or earlier (accepted == false): either way, recorded once.
            outbox.removeAll { $0.id == p.id }
        } catch let e as AgentHostError {
            if case .Rpc = e, let i = outbox.firstIndex(where: { $0.id == p.id }) {
                outbox[i].failure = e.userMessage
            }
        } catch {}
    }

    func cancel(_ sessionId: String) async throws {
        try await send(RPC.sessionCancel, ["session_id": sessionId])
    }

    /// Change one of the agent's settings (model, effort). Shown at once;
    /// put back if the host refuses.
    func setConfig(sessionId: String, configId: String, value: String) async throws {
        guard let before = session(sessionId) else { return }
        var s = before
        if let i = s.configOptions?.firstIndex(where: { $0.id == configId }) {
            s.configOptions?[i].currentValue = value
        }
        upsert(s)
        #if DEBUG
        if isDemo { return }
        #endif
        do {
            let updated: SessionInfo = try await call(
                RPC.sessionSetConfig,
                ["session_id": sessionId, "config_id": configId, "value": value]
            )
            upsert(updated)
        } catch {
            if session(sessionId)?.configOptions == s.configOptions { upsert(before) }
            throw error
        }
    }

    /// Ask a side question (`/btw`), new (`sideId` nil) or a follow-up. The
    /// session itself is not interrupted. Returns the side id.
    @discardableResult
    func sideAsk(sessionId: String, sideId: String?, text: String) async throws -> String {
        let question = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !question.isEmpty else { throw ComposerError(message: "Type a question after /btw.") }
        #if DEBUG
        if isDemo {
            let sid = sideId ?? "side-\(UUID().uuidString.prefix(8).lowercased())"
            demoSideAnswer(sessionId: sessionId, sideId: sid, question: question)
            return sid
        }
        #endif
        var params: [String: Any] = ["session_id": sessionId, "text": question]
        if let sideId { params["side_id"] = sideId }
        let r: SideAskResultModel = try await call(RPC.sessionSideAsk, params)
        closedSides.remove(r.sideId)
        // The `side.question` event may already be here.
        let asked = buildSideConversations(transcripts[sessionId]?.events ?? [])
            .first { $0.id == r.sideId }?.turns.last
        if asked?.question != question || asked?.done == true {
            sidePending[r.sideId, default: []].append(question)
            pendingSession[r.sideId] = sessionId
        }
        return r.sideId
    }

    /// Archive a session (the host ends it first if it is live). It leaves
    /// the list and stops being followed.
    func archive(_ sessionId: String) async throws {
        archiving.insert(sessionId)
        // After the answer has taken it out of `sessions`, or failed.
        defer { archiving.remove(sessionId) }
        #if DEBUG
        if isDemo, var s = session(sessionId) {
            s.archivedAt = Int64(Date().timeIntervalSince1970 * 1000)
            s.state = SessionStateName.ended
            upsert(s)
            notesDoneForArchived(sessionId)
            return
        }
        #endif
        let s: SessionInfo = try await call(RPC.sessionArchive, ["session_id": sessionId])
        upsert(s)
    }

    /// Give a session a title of its own; blank clears it (the session is
    /// then called after its workspace). At most `RPC.maxSessionTitle`
    /// characters.
    func rename(_ sessionId: String, title: String) async throws {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.count > RPC.maxSessionTitle {
            throw ComposerError(message: "A session title has at most \(RPC.maxSessionTitle) characters.")
        }
        let newTitle: String? = trimmed.isEmpty ? nil : trimmed
        #if DEBUG
        if isDemo, var s = session(sessionId) {
            s.title = newTitle
            upsert(s)
            return
        }
        #endif
        let s: SessionInfo = try await call(
            RPC.sessionRename,
            ["session_id": sessionId, "title": newTitle.map { $0 as Any } ?? NSNull()]
        )
        upsert(s)
    }

    func unarchive(_ sessionId: String) async throws {
        #if DEBUG
        if isDemo, var s = session(sessionId) {
            s.archivedAt = nil
            upsert(s)
            return
        }
        #endif
        let s: SessionInfo = try await call(RPC.sessionUnarchive, ["session_id": sessionId])
        upsert(s)
    }

    /// Make a login the default for new sessions in a workspace (`agent` nil
    /// clears it; `profileId` nil or the agent's name: its default login).
    func setDefaultLogin(workspaceId: String, agent: String?, profileId: String?) async throws {
        #if DEBUG
        if isDemo, let i = workspaces.firstIndex(where: { $0.workspaceId == workspaceId }) {
            workspaces[i].defaultAgent = agent
            workspaces[i].defaultProfileId = profileId == agent ? nil : profileId
            return
        }
        #endif
        var params: [String: Any] = ["workspace_id": workspaceId, "agent": agent.map { $0 as Any } ?? NSNull()]
        if let profileId { params["profile_id"] = profileId }
        let w: WorkspaceInfo = try await call(RPC.workspaceSetDefaultAgent, params)
        if let i = workspaces.firstIndex(where: { $0.workspaceId == w.workspaceId }) {
            workspaces[i] = w
        } else {
            workspaces.append(w)
        }
        scheduleSave()
    }

    func respond(_ approvalId: String, approve: Bool) async throws {
        #if DEBUG
        if isDemo {
            approvals.removeAll { $0.approvalId == approvalId }
            return
        }
        #endif
        try await send(
            RPC.approvalRespond,
            ["approval_id": approvalId, "decision": approve ? "approve" : "deny"]
        )
        approvals.removeAll { $0.approvalId == approvalId }
    }

    // MARK: Notifications

    func handleNotification(method: String, paramsJson: String, generation gen: Int) {
        guard gen == generation else { return }
        let data = Data(paramsJson.utf8)
        switch method {
        case RPC.nEvent:
            guard let e = try? JSONDecoder().decode(HostEvent.self, from: data) else { return }
            ingest(e)
        case RPC.nSessionDelta:
            guard let d = try? JSONDecoder().decode(SessionDeltaModel.self, from: data) else { return }
            if d.kind == EventKind.sideMessage {
                if let sid = d.sideId { sideStreaming[sid, default: ""] += d.text }
                return
            }
            if var s = streaming[d.sessionId], s.kind == d.kind {
                s.text += d.text
                streaming[d.sessionId] = s
            } else {
                streaming[d.sessionId] = StreamingText(kind: d.kind, text: d.text)
            }
        case RPC.nSessionChanged:
            guard let s = try? JSONDecoder().decode(SessionInfo.self, from: data) else { return }
            let before = session(s.sessionId)?.state
            upsert(s)
            if before != s.state,
               s.state == SessionStateName.waitingApproval || before == SessionStateName.waitingApproval {
                Task { await refreshApprovals() }
            }
            if !SessionStateName.isTerminal(s.state) && !s.isArchived {
                Task { await subscribe(s.sessionId) }
            }
            catchUpIfBehind(s.sessionId, hostLastSeq: s.lastSeq)
        case RPC.nScreenChanged:
            screenStatus = try? JSONDecoder().decode(ScreenStatusModel.self, from: data)
        case RPC.nNoteChanged:
            applyNoteChanged(data)
        case RPC.nAutomationChanged:
            applyAutomationChanged(data)
        case RPC.nDeviceRevoked:
            deviceRemoved()
        default:
            break
        }
    }

    /// Safety net for live updates: the host says this session has newer
    /// events than we hold. They are usually just behind this message; if
    /// they haven't arrived after a moment, subscribe again from where we are
    /// so a lost stream heals without reopening the app.
    private func catchUpIfBehind(_ sessionId: String, hostLastSeq: Int64) {
        guard subscribed.contains(sessionId),
              hostLastSeq > (transcripts[sessionId]?.lastSeq ?? 0) else { return }
        let gen = generation
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard let self, gen == self.generation, self.link.isOnline else { return }
            if (self.transcripts[sessionId]?.lastSeq ?? 0) < hostLastSeq {
                await self.subscribe(sessionId, force: true)
            }
        }
    }

    func handleClosed(reason: String, code: UInt16, generation gen: Int) {
        guard gen == generation else { return }
        client = nil
        subscribed = []
        subscriptionIds = [:]
        streaming = [:]
        if code == constants.closeDeviceRevoked {
            deviceRemoved()
            return
        }
        link = code == constants.closeHostOffline ? .hostOffline : .offline(reason)
        scheduleReconnect()
    }

    private func ingest(_ e: HostEvent) {
        var t = transcripts[e.sessionId] ?? Transcript(sessionId: e.sessionId)
        let r = t.ingest(e)
        transcripts[e.sessionId] = t
        switch r {
        case .duplicate:
            return
        case .gap:
            // Ask for everything after the last contiguous event again.
            Task { await subscribe(e.sessionId, force: true) }
            return
        case .appended:
            break
        }
        dirtyTranscripts.insert(e.sessionId)
        switch e.type {
        case EventKind.agentMessage, EventKind.agentThought, EventKind.toolCall, EventKind.turnEnd:
            streaming[e.sessionId] = nil
        case EventKind.sideMessage, EventKind.sideEnd:
            if let sid = e.payload["side_id"].string { sideStreaming[sid] = nil }
        case EventKind.sideQuestion:
            if let sid = e.payload["side_id"].string, var q = sidePending[sid],
               let i = q.firstIndex(of: e.payload["text"].string ?? "") ?? q.indices.first {
                q.remove(at: i)
                sidePending[sid] = q.isEmpty ? nil : q
            }
        case EventKind.prompt:
            if let pid = e.payload["prompt_id"].string {
                outbox.removeAll { $0.id == pid }
            }
        case EventKind.approvalRequested:
            if let a = e.payload.decode(PendingApprovalModel.self),
               !approvals.contains(where: { $0.approvalId == a.approvalId }) {
                approvals.append(a)
            }
        case EventKind.approvalResolved:
            if let id = e.payload["approval_id"].string {
                approvals.removeAll { $0.approvalId == id }
            }
        case EventKind.sessionState:
            if let state = e.payload["state"].string,
               let i = sessions.firstIndex(where: { $0.sessionId == e.sessionId }) {
                sessions[i].state = state
                sessions[i].stateReason = e.payload["reason"].string
                sessions[i].updatedAt = max(sessions[i].updatedAt, e.ts)
            }
        default:
            break
        }
        if let i = sessions.firstIndex(where: { $0.sessionId == e.sessionId }) {
            sessions[i].lastSeq = max(sessions[i].lastSeq, e.seq)
            sessions[i].updatedAt = max(sessions[i].updatedAt, e.ts)
        }
        scheduleSave()
    }

    /// Archived sessions go to `archivedSessions` (when loaded) and stop
    /// being re-subscribed; unarchived ones come back to the list.
    private func upsert(_ s: SessionInfo) {
        if s.isArchived {
            sessions.removeAll { $0.sessionId == s.sessionId }
            watched.remove(s.sessionId)
            if var list = archivedSessions {
                list.removeAll { $0.sessionId == s.sessionId }
                list.insert(s, at: 0)
                archivedSessions = list
            }
        } else {
            archivedSessions?.removeAll { $0.sessionId == s.sessionId }
            if let i = sessions.firstIndex(where: { $0.sessionId == s.sessionId }) {
                sessions[i] = s
            } else {
                sessions.append(s)
            }
        }
        scheduleSave()
    }

    // MARK: Cache

    private func loadCache() {
        if let s = TranscriptCache.loadState() {
            hostInfo = s.hostInfo
            workspaces = s.workspaces
            sessions = s.sessions.filter { !$0.isArchived }
            deviceName = s.deviceName
            watched = Set(s.watched)
            cachedAt = s.savedAt
        }
        transcripts = TranscriptCache.loadTranscripts()
    }

    private func scheduleSave() {
        guard phase == .ready, saveTask == nil else { return }
        saveTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            guard let self, !Task.isCancelled else { return }
            self.saveTask = nil
            self.saveNow()
        }
    }

    func saveNow() {
        guard phase == .ready, credentials != nil else { return }
        let now = Date()
        TranscriptCache.saveState(CachedState(
            hostInfo: hostInfo,
            workspaces: workspaces,
            sessions: sessions,
            deviceName: deviceName,
            watched: Array(watched),
            savedAt: now
        ))
        for sid in dirtyTranscripts {
            if let t = transcripts[sid] { TranscriptCache.saveTranscript(t) }
        }
        dirtyTranscripts = []
        cachedAt = now
    }

    // MARK: Network reachability

    private func startMonitoringNetwork() {
        guard pathMonitor == nil else { return }
        let m = NWPathMonitor()
        m.pathUpdateHandler = { [weak self] path in
            let up = path.status == .satisfied
            let via = path.availableInterfaces.first?.type
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self else { return }
                    let wasUp = self.networkAvailable
                    let wasVia = self.networkInterface
                    self.networkAvailable = up
                    self.networkInterface = via
                    if up && !wasUp {
                        self.retryNow()
                    } else if up, wasVia != nil, via != wasVia, self.link.isOnline {
                        // Wi-Fi to cellular or back: the old socket won't survive.
                        self.reconnectNow()
                    }
                }
            }
        }
        m.start(queue: DispatchQueue(label: "agenthost.path"))
        pathMonitor = m
    }

    private func stopMonitoringNetwork() {
        pathMonitor?.cancel()
        pathMonitor = nil
    }

    // MARK: Demo (DEBUG only)

    #if DEBUG
    private func loadDemo(_ d: DemoData) {
        isDemo = true
        demoHostName = d.hostName
        hostInfo = d.hostInfo
        workspaces = d.workspaces
        sessions = d.sessions
        archivedSessions = d.archived
        approvals = d.approvals
        deviceName = d.deviceName
        transcripts = d.transcripts
        connectors = d.connectors
        screenStatus = d.screenStatus
        automations = d.automations
        notes = d.notes
        hostProtocolVersion = RPC.noteProtocolVersion
        notificationsEnabled = d.screen == .notifications || d.screen == .notificationsOwnKey
        pushPrefs = PushPrefsModel()
        pushStatus = d.pushStatus(for: d.screen)
        pushAuthorization = .authorized
        showPushOnboarding = d.screen == .pairNotifications || d.screen == .pairAPNsKey
        link = .online
        phase = d.screen == .pair ? .unpaired : .ready
    }

    /// Demo: the side question appears, and its answer streams in.
    private func demoSideAnswer(sessionId: String, sideId: String, question: String) {
        func next(_ type: String, _ payload: [String: JSONValue]) {
            let seq = (transcripts[sessionId]?.lastSeq ?? 0) + 1
            var p = payload
            p["side_id"] = .string(sideId)
            ingest(HostEvent(sessionId: sessionId, seq: seq, type: type, payload: .object(p),
                             ts: Int64(Date().timeIntervalSince1970 * 1000)))
        }
        closedSides.remove(sideId)
        next(EventKind.sideQuestion, ["text": .string(question)])
        let answer = DemoData.sideAnswer
        Task {
            for word in answer.split(separator: " ", omittingEmptySubsequences: false) {
                try? await Task.sleep(nanoseconds: 60_000_000)
                sideStreaming[sideId, default: ""] += word + " "
            }
            next(EventKind.sideMessage, ["text": .string(answer)])
            next(EventKind.sideEnd, ["stop_reason": .string("end_turn")])
        }
    }
    #endif
}
