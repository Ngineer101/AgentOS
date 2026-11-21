import SwiftUI

/// Settings: the host and its connection, Automations (protocol 5), Notifications (protocol 4), Security (Require Face ID, off by
/// default), the agents on the host (read only), this iPhone, and Unpair.
struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var confirmUnpair = false
    @State private var securityError: String?
    @State private var changingLock = false
    @State private var showScreen = false

    var body: some View {
        List {
            hostSection
            if model.hostSupportsAutomations { automationsSection }
            if model.hostSupportsPush { notificationsSection }
            if model.hostSharesScreen { screenSection }
            Section {
                NavigationLink(value: Route.archived) {
                    HStack(spacing: 12) {
                        SettingsIcon(systemName: "archivebox.fill", color: .indigo)
                        Text("Archived Sessions")
                        Spacer(minLength: 8)
                        if let n = model.archivedSessions?.count {
                            Text("\(n)").foregroundStyle(.secondary)
                        }
                    }
                }
            } footer: {
                Text("Swipe a session in the list to archive it. Archived sessions keep their transcripts.")
            }

            securitySection
            agentsSection
            deviceSection

            Section {
                Button(role: .destructive) {
                    confirmUnpair = true
                } label: {
                    Text("Unpair This iPhone")
                        .frame(maxWidth: .infinity)
                }
            } footer: {
                Text("Deletes this iPhone's keys and cached transcripts and turns off its notifications. Also remove it on the computer (Settings → Devices) so the host forgets its key.")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.large)
        .refreshable { await model.refreshAll() }
        .task {
            if model.archivedSessions == nil { await model.refreshArchived() }
            if model.hostSupportsAutomations && model.automations == nil { await model.refreshAutomations() }
        }
        .fullScreenCover(isPresented: $showScreen) {
            HostScreenView().environmentObject(model)
        }
        .confirmationDialog("Unpair this iPhone?", isPresented: $confirmUnpair, titleVisibility: .visible) {
            Button("Unpair", role: .destructive) { model.unpair() }
        } message: {
            Text("You will need to scan a new QR code on the computer to use this host again.")
        }
    }

    // MARK: Sections

    private var hostSection: some View {
        Section(caps: "Host") {
            HStack(spacing: 12) {
                SettingsIcon(systemName: "desktopcomputer", color: model.link.isOnline ? Palette.green : Color(white: 0.4))
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.hostName)
                    Text(hostDetail)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
            }
            .padding(.vertical, 2)
            .accessibilityElement(children: .combine)

            if case let .offline(reason) = model.link {
                Text(reason).font(.footnote).foregroundStyle(.secondary)
            }
            if !model.link.isOnline {
                Button("Reconnect Now") { model.retryNow() }
            }
            if let relay = model.credentials?.relayUrl ?? model.hostInfo?.relay.url {
                LabeledContent("Relay") {
                    Text(URL(string: relay)?.host() ?? relay)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            if let info = model.hostInfo {
                LabeledContent("Sandbox") {
                    Text(info.sandbox.ok ? "\(info.sandbox.backend) · Healthy" : info.sandbox.backend)
                        .foregroundStyle(info.sandbox.ok ? Color.secondary : Palette.orange)
                }
                if let p = info.sandbox.problem {
                    Text(p).font(.footnote).foregroundStyle(Palette.orange)
                }
            }
        }
    }

    private var notificationsSection: some View {
        Section {
            NavigationLink(value: Route.notifications) {
                HStack(spacing: 12) {
                    SettingsIcon(systemName: "bell.fill", color: Palette.red)
                    Text("Notifications")
                    Spacer(minLength: 8)
                    Text(model.notificationsSummary).foregroundStyle(.secondary)
                }
            }
        }
    }

    private var automationsSection: some View {
        Section {
            NavigationLink(value: Route.automations) {
                HStack(spacing: 12) {
                    SettingsIcon(systemName: "clock.arrow.circlepath", color: .purple)
                    Text("Automations")
                    Spacer(minLength: 8)
                    if let n = model.automations?.count {
                        Text("\(n)").foregroundStyle(.secondary)
                    }
                }
            }
        } footer: {
            Text("Sessions that start on a schedule, from a webhook or by hand.")
        }
    }

    private var screenSection: some View {
        Section {
            Button {
                showScreen = true
            } label: {
                HStack(spacing: 12) {
                    SettingsIcon(systemName: "display", color: .blue)
                    Text("Host Screen").foregroundStyle(Color.primary)
                    Spacer(minLength: 8)
                    Text(screenSummary).foregroundStyle(Color(uiColor: .secondaryLabel))
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Color(uiColor: .tertiaryLabel))
                }
            }
            .disabled(!model.link.isOnline)
        } footer: {
            Text("See the computer's screen when a task needs it: a sign-in page, or an approval in a browser. The computer decides whether this iPhone may watch, or also click and type.")
        }
    }

    private var screenSummary: String {
        guard let st = model.screenStatus, st.policy.enabled else { return "Off" }
        if st.running != nil { return "Sharing" }
        return st.policy.allowControl ? "Watch & control" : "Watch only"
    }

    private var hostDetail: String {
        var parts = [model.link.label]
        if let info = model.hostInfo {
            parts.append(info.os)
            parts.append(info.version)
        }
        return parts.joined(separator: " · ")
    }

    private var securitySection: some View {
        Section {
            Toggle(isOn: Binding(
                get: { model.requireUnlock },
                set: { on in
                    changingLock = true
                    Task {
                        do {
                            try await model.setRequireUnlock(on)
                            securityError = nil
                        } catch {
                            securityError = userMessage(for: error)
                        }
                        changingLock = false
                    }
                }
            )) {
                HStack(spacing: 12) {
                    SettingsIcon(systemName: unlockIcon, color: Palette.green)
                    Text("Require \(AppModel.unlockMethodName)")
                }
            }
            .disabled(changingLock)
            if let securityError {
                Text(securityError).font(.footnote).foregroundStyle(Palette.red)
            }
        } header: {
            GroupHeader("Security")
        } footer: {
            Text("When on, AgentHost asks for \(AppModel.unlockMethodName) each time you open it. Your key to the host stays on this iPhone either way.")
        }
    }

    private var unlockIcon: String {
        switch AppModel.unlockMethodName {
        case "Face ID": return "faceid"
        case "Touch ID": return "touchid"
        case "Optic ID": return "opticid"
        default: return "lock.fill"
        }
    }

    @ViewBuilder
    private var agentsSection: some View {
        Section {
            if let agents = model.hostInfo?.agents, !agents.isEmpty {
                ForEach(agents) { a in
                    HStack(spacing: 12) {
                        AgentBadge(agent: a.agent)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(a.label)
                            if !a.available, let fix = a.fix {
                                Text(fix)
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.tertiary)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                        .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
                        Spacer(minLength: 8)
                        Text(status(a))
                            .foregroundStyle(a.available ? Color.secondary : Palette.orange)
                            .lineLimit(1)
                            .fixedSize()
                    }
                    .accessibilityElement(children: .combine)
                }
            } else {
                Text("Agent status appears once connected.").foregroundStyle(.secondary)
            }
        } header: {
            GroupHeader("Agents on \(model.hostName)")
        } footer: {
            Text("Add logins and workspaces on the computer.")
        }
    }

    private func status(_ a: AgentStatusInfo) -> String {
        if a.available { return "Ready" }
        if !a.installed { return "Not installed" }
        if !a.loggedIn { return "Not logged in" }
        return a.problem ?? "Unavailable"
    }

    private var deviceSection: some View {
        Section(caps: "This iPhone") {
            LabeledContent("Name", value: model.deviceName ?? "—")
            LabeledContent("Can do", value: "Sessions, approvals")
            if let id = model.credentials?.deviceId {
                LabeledContent("Device ID") {
                    Text(id)
                        .font(.footnote.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
            }
        }
    }
}

/// The coloured rounded-square icon of a settings row.
struct SettingsIcon: View {
    let systemName: String
    let color: Color

    var body: some View {
        Image(systemName: systemName)
            .font(.system(size: 15, weight: .medium))
            .foregroundStyle(.white)
            .frame(width: 30, height: 30)
            .background(color, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            .accessibilityHidden(true)
    }
}
