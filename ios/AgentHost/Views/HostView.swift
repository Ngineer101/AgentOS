import SwiftUI

/// Host status, this device's name, connection state; unpair.
struct HostView: View {
    @EnvironmentObject private var model: AppModel
    @State private var confirmUnpair = false

    var body: some View {
        List {
            Section("Connection") {
                LabeledContent("State", value: model.link.label)
                if let creds = model.credentials {
                    LabeledContent("Host", value: creds.hostName)
                    LabeledContent("Relay", value: creds.relayUrl)
                }
                if case let .offline(reason) = model.link {
                    Text(reason).font(.caption).foregroundStyle(.secondary)
                }
                if !model.link.isOnline {
                    Button("Reconnect now") { model.retryNow() }
                }
            }

            Section("This device") {
                LabeledContent("Name", value: model.deviceName ?? "—")
                if let id = model.credentials?.deviceId {
                    LabeledContent("Device id") {
                        Text(id).font(.caption.monospaced()).textSelection(.enabled)
                    }
                }
                Text("This phone can run sessions and answer approvals. It cannot change workspaces, connectors, policy or devices.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let info = model.hostInfo {
                Section("Host") {
                    LabeledContent("Version", value: info.version)
                    LabeledContent("System", value: info.os)
                    LabeledContent("Protocol", value: "\(info.protocolVersion)")
                    LabeledContent("Relay connected", value: info.relay.connected ? "Yes" : "No")
                }
                Section("Sandbox") {
                    LabeledContent("Backend", value: info.sandbox.backend)
                    LabeledContent("Healthy", value: info.sandbox.ok ? "Yes" : "No")
                    if let v = info.sandbox.runtimeVersion {
                        LabeledContent("Runtime", value: v)
                    }
                    if let p = info.sandbox.problem {
                        Text(p).font(.caption).foregroundStyle(.red)
                    }
                }
                Section("Agents") {
                    ForEach(info.agents) { a in
                        VStack(alignment: .leading, spacing: 2) {
                            HStack {
                                Text(a.label)
                                Spacer()
                                Image(systemName: a.available ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                                    .foregroundStyle(a.available ? Color.green : Color.orange)
                            }
                            if let v = a.version {
                                Text(v).font(.caption2).foregroundStyle(.secondary)
                            }
                            if let dir = a.configDir {
                                Text(dir).font(.caption2.monospaced()).foregroundStyle(.secondary)
                            }
                            if !a.available, let p = a.problem {
                                Text(p).font(.caption).foregroundStyle(.orange)
                            }
                            if !a.available, let fix = a.fix {
                                Text(fix).font(.caption2.monospaced()).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            } else {
                Section("Host") {
                    Text("Host status appears once connected.").foregroundStyle(.secondary)
                }
            }

            Section {
                Button("Unpair This iPhone", role: .destructive) {
                    confirmUnpair = true
                }
            } footer: {
                Text("Deletes this iPhone's keys and cached transcripts. Also remove the device on the desktop (Devices → Revoke) so the host forgets its key.")
            }
        }
        .navigationTitle("Host")
        .refreshable { await model.refreshAll() }
        .confirmationDialog("Unpair this iPhone?", isPresented: $confirmUnpair, titleVisibility: .visible) {
            Button("Unpair", role: .destructive) { model.unpair() }
        } message: {
            Text("You will need to scan a new QR code from the desktop to use this host again.")
        }
    }
}
