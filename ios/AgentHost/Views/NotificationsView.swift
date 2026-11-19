import SwiftUI
import UIKit
import UniformTypeIdentifiers

// Push notifications: Settings → Notifications, and the pairing flow's
// "Turn On Notifications?" and "Add Your APNs Key" steps (hosts on
// protocol 4 or newer). Designs: "iOS — Settings · Notifications",
// "iOS — Pair · Notifications", "iOS — Pair · Add your APNs key".

/// "What a notification says is encrypted …", under both screens.
private let encryptionNote = "What a notification says is encrypted for this iPhone. Apple only sees “A session needs you”."

// MARK: - Settings → Notifications

struct NotificationsSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.openURL) private var openURL
    @State private var busy = false
    @State private var pushError: String?
    @State private var importing = false
    @State private var testing = false
    @State private var testNote: String?

    var body: some View {
        List {
            Section {
                Toggle(isOn: Binding(
                    get: { model.notificationsEnabled },
                    set: { on in toggle(on) }
                )) {
                    HStack(spacing: 12) {
                        SettingsIcon(systemName: "bell.fill", color: Palette.red)
                        Text("Notifications")
                    }
                }
                .disabled(busy || (!model.notificationsEnabled && !model.link.isOnline))
                if model.notificationsEnabled && !model.pushAllowedBySystem {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Notifications are turned off for AgentHost in iOS Settings.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                        Button("Open Settings") { openNotificationSettings(openURL) }
                    }
                }
                if let pushError {
                    Text(pushError).font(.footnote).foregroundStyle(Palette.red)
                }
            } footer: {
                Text("Get a notification on this iPhone when a session on \(model.hostName) needs you.")
            }

            if model.notificationsEnabled {
                Section {
                    Toggle("Approvals", isOn: pref(\.approvals))
                    Toggle("Finished turns", isOn: pref(\.turnDone))
                    Toggle("Failed sessions", isOn: pref(\.failed))
                } header: {
                    GroupHeader("Notify me about")
                } footer: {
                    Text("Finished turns: the agent is done and waits for you.")
                }

                keySection

                Section {
                    Button {
                        sendTest()
                    } label: {
                        HStack {
                            Text("Send Test Notification")
                            if testing {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(testing || !model.link.isOnline || model.pushStatus?.configured != true)
                    if let testNote {
                        Text(testNote).font(.footnote).foregroundStyle(.secondary)
                    }
                    if let last = model.pushStatus?.thisDevice?.lastError {
                        Text("Last problem: \(AppModel.describeAPNsError(last))")
                            .font(.footnote)
                            .foregroundStyle(Palette.red)
                    }
                } footer: {
                    Text(encryptionNote)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Notifications")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await model.refreshPushStatus() }
        .task {
            await model.refreshPushAuthorization()
            await model.refreshPushStatus()
        }
        .modifier(APNsKeyImport(isPresented: $importing, busy: $busy, error: $pushError))
    }

    private var keySection: some View {
        Section {
            if let st = model.pushStatus, st.configured {
                LabeledContent("Key ID", value: st.keyId ?? "—")
                if let team = st.teamId {
                    LabeledContent("Team ID", value: team)
                }
            } else {
                LabeledContent("Key", value: model.pushStatus == nil ? "—" : "Not set up")
            }
            Button(model.pushStatus?.configured == true ? "Replace APNs Key…" : "Add APNs Key…") {
                importing = true
            }
            .disabled(busy || !model.link.isOnline)
        } header: {
            GroupHeader("APNs key on \(model.hostName)")
        } footer: {
            Text("\(model.hostName) sends notifications through Apple with this key from the Apple Developer account AgentHost is signed with. It is stored on \(model.hostName) only.")
        }
    }

    private func toggle(_ on: Bool) {
        busy = true
        pushError = nil
        testNote = nil
        Task {
            do {
                if on {
                    // Denied by iOS: on here, with the "turned off in iOS
                    // Settings" row above.
                    _ = try await model.enablePush()
                } else {
                    try await model.disablePush()
                }
            } catch {
                pushError = userMessage(for: error)
            }
            busy = false
        }
    }

    private func pref(_ key: WritableKeyPath<PushPrefsModel, Bool>) -> Binding<Bool> {
        Binding(
            get: { model.pushPrefs[keyPath: key] },
            set: { value in
                var prefs = model.pushPrefs
                prefs[keyPath: key] = value
                Task {
                    do {
                        try await model.setPushPrefs(prefs)
                        pushError = nil
                    } catch {
                        pushError = userMessage(for: error)
                    }
                }
            }
        )
    }

    private func sendTest() {
        testing = true
        testNote = nil
        Task {
            do {
                testNote = try await model.sendTestPush()
            } catch {
                testNote = userMessage(for: error)
            }
            testing = false
        }
    }
}

/// iOS Settings → AgentHost → Notifications.
@MainActor
private func openNotificationSettings(_ openURL: OpenURLAction) {
    if let url = URL(string: UIApplication.openNotificationSettingsURLString) {
        openURL(url)
    }
}

// MARK: - Pairing: Notifications step

/// Shown once after pairing, when the phone has connected to a host on
/// protocol 4 or newer. Every step can be skipped.
struct PushOnboardingView: View {
    enum Step {
        /// "Turn On Notifications?"
        case ask
        /// iOS said no.
        case denied
        /// The host has no APNs key yet.
        case key
    }

    @EnvironmentObject private var model: AppModel
    @Environment(\.openURL) private var openURL
    @State private var step: Step
    @State private var busy = false
    @State private var errorText: String?
    @State private var importing = false
    @State private var testing = false
    @State private var testNote: String?

    init() {
        var first = Step.ask
        #if DEBUG
        if DemoData.requestedScreen == .pairAPNsKey { first = .key }
        #endif
        _step = State(initialValue: first)
    }

    var body: some View {
        NavigationStack {
            switch step {
            case .ask: askStep
            case .denied: deniedStep
            case .key: keyStep
            }
        }
        .interactiveDismissDisabled()
    }

    // MARK: Ask

    private var askStep: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            VStack(spacing: 14) {
                Image(systemName: "bell.fill")
                    .font(.system(size: 60))
                    .foregroundStyle(.tint)
                    .padding(.bottom, 10)
                    .accessibilityHidden(true)
                Text("Paired with \(model.hostName)")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                Text("Turn On Notifications?")
                    .font(.title.weight(.bold))
                Text("Get a notification when a session needs your approval. You can also be told when a turn finishes or a session fails.")
                    .foregroundStyle(.secondary)
                Text(encryptionNote)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .padding(.top, 6)
                if let errorText {
                    Text(errorText)
                        .font(.footnote)
                        .foregroundStyle(Palette.red)
                }
            }
            .multilineTextAlignment(.center)
            .padding(.horizontal, 36)
            Spacer(minLength: 0)
            VStack(spacing: 6) {
                Button {
                    turnOn()
                } label: {
                    if busy {
                        ProgressView()
                    } else {
                        Text("Turn On Notifications")
                    }
                }
                .buttonStyle(CapsuleButtonStyle(fill: .accentColor, height: 50))
                .disabled(busy)
                Button("Not Now") { model.finishPushOnboarding() }
                    .frame(minHeight: 44)
                    .disabled(busy)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 12)
        }
        .toolbar(.hidden, for: .navigationBar)
    }

    private func turnOn() {
        busy = true
        errorText = nil
        Task {
            do {
                switch try await model.enablePush() {
                case .denied:
                    step = .denied
                case let .enabled(status):
                    if status.configured {
                        // Ready: one test notification, and done.
                        _ = try? await model.sendTestPush()
                        model.finishPushOnboarding()
                    } else {
                        step = .key
                    }
                }
            } catch {
                errorText = userMessage(for: error)
            }
            busy = false
        }
    }

    // MARK: Denied

    private var deniedStep: some View {
        ContentUnavailableView {
            Label("Notifications Are Off", systemImage: "bell.slash")
        } description: {
            Text("Allow notifications for AgentHost in iOS Settings. They start as soon as you do; change them later in Settings → Notifications.")
        } actions: {
            Button("Open Settings") { openNotificationSettings(openURL) }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            Button("Not Now") { model.finishPushOnboarding() }
        }
    }

    // MARK: APNs key

    private var configured: Bool { model.pushStatus?.configured == true }

    private var keyStep: some View {
        Form {
            Section {
                VStack(spacing: 10) {
                    Image(systemName: "key.fill")
                        .font(.system(size: 36))
                        .foregroundStyle(.tint)
                        .accessibilityHidden(true)
                    Text("Add Your APNs Key")
                        .font(.title2.weight(.bold))
                    Text("\(model.hostName) sends notifications through Apple with the APNs key (.p8) from the Apple Developer account AgentHost is signed with. It is needed once per host.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .listRowBackground(Color.clear)
            }

            Section {
                if configured, let id = model.pushStatus?.keyId {
                    LabeledContent("Key ID", value: id)
                } else {
                    Button {
                        importing = true
                    } label: {
                        HStack {
                            Label("Choose Key File…", systemImage: "doc")
                            if busy {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(busy)
                }
                LabeledContent("Team ID", value: AppModel.appTeamId ?? model.pushStatus?.teamId ?? "—")
                if let errorText {
                    Text(errorText).font(.footnote).foregroundStyle(Palette.red)
                }
            } footer: {
                Text("Pick AuthKey_XXXXXXXXXX.p8 in Files, for example in iCloud Drive. The key is stored on \(model.hostName) only, not on this iPhone.")
            }

            Section {
                Button {
                    sendTest()
                } label: {
                    HStack {
                        Text("Send Test Notification")
                        if testing {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .disabled(!configured || testing)
            } footer: {
                Text(testNote ?? "Create a key in your Apple Developer account under Certificates, Identifiers & Profiles → Keys, with Apple Push Notifications service enabled.")
            }
        }
        .navigationTitle("APNs Key")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Not Now") { model.finishPushOnboarding() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { model.finishPushOnboarding() }
            }
        }
        .modifier(APNsKeyImport(isPresented: $importing, busy: $busy, error: $errorText))
    }

    private func sendTest() {
        testing = true
        testNote = nil
        Task {
            do {
                testNote = try await model.sendTestPush()
            } catch {
                testNote = userMessage(for: error)
            }
            testing = false
        }
    }
}

// MARK: - Adding the APNs key

/// "Add APNs Key…": the Files picker for the .p8 file, the key id (and team
/// id) when they can't be worked out, then `push.set_key`.
struct APNsKeyImport: ViewModifier {
    @EnvironmentObject private var model: AppModel
    @Binding var isPresented: Bool
    @Binding var busy: Bool
    @Binding var error: String?
    /// The key file's text, waiting for its key id.
    @State private var pendingKey: String?
    @State private var asking = false
    @State private var keyId = ""
    @State private var teamId = ""

    func body(content: Content) -> some View {
        content
            .fileImporter(isPresented: $isPresented, allowedContentTypes: [.item]) { result in
                switch result {
                case let .success(url):
                    picked(url)
                case let .failure(e):
                    error = userMessage(for: e)
                }
            }
            .alert("Key Details", isPresented: $asking) {
                TextField("Key ID", text: $keyId)
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                if AppModel.appTeamId == nil {
                    TextField("Team ID", text: $teamId)
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                }
                Button("Cancel", role: .cancel) { pendingKey = nil }
                Button("Add") {
                    if let key = pendingKey { submit(key) }
                }
            } message: {
                Text("Enter the 10-character key ID shown for this key in your Apple Developer account (Certificates, Identifiers & Profiles → Keys).")
            }
    }

    @MainActor
    private func picked(_ url: URL) {
        error = nil
        let text: String
        do {
            text = try AppModel.readAPNsKeyFile(url)
        } catch {
            self.error = userMessage(for: error)
            return
        }
        keyId = AppModel.apnsKeyId(fromFileName: url.lastPathComponent) ?? ""
        teamId = AppModel.appTeamId ?? ""
        if keyId.isEmpty || teamId.isEmpty {
            pendingKey = text
            asking = true
        } else {
            submit(text)
        }
    }

    @MainActor
    private func submit(_ key: String) {
        pendingKey = nil
        let id = keyId.trimmingCharacters(in: .whitespaces).uppercased()
        let team = (AppModel.appTeamId ?? teamId).trimmingCharacters(in: .whitespaces).uppercased()
        guard id.count == 10, team.count == 10 else {
            error = "The key ID and the team ID each have 10 letters and digits."
            return
        }
        busy = true
        Task {
            do {
                try await model.setPushKey(p8: key, keyId: id, teamId: team)
            } catch {
                self.error = userMessage(for: error)
            }
            busy = false
        }
    }
}
