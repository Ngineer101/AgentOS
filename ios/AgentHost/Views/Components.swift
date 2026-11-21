import SwiftUI

/// Connection state, shown when connecting has failed or is slow (see
/// `AppModel.showsConnectionStatus`). Plain row
/// content: inside a `List` it is a normal row; elsewhere wrap it in
/// `.statusCard()`.
struct ConnectionStatusRow: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        if model.showsConnectionStatus {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: icon)
                    .foregroundStyle(color)
                    .imageScale(.large)
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    Text(detail)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                if model.link == .connecting {
                    ProgressView()
                } else if canRetry {
                    Button("Retry") { model.retryNow() }
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.capsule)
                        .controlSize(.small)
                }
            }
            .accessibilityElement(children: .combine)
        }
    }

    private var canRetry: Bool {
        switch model.link {
        case .offline, .hostOffline, .disconnected: return true
        default: return false
        }
    }

    private var icon: String {
        switch model.link {
        case .hostOffline: return "desktopcomputer.trianglebadge.exclamationmark"
        case .updateRequired: return "arrow.down.app"
        case .connecting: return "antenna.radiowaves.left.and.right"
        default: return "wifi.slash"
        }
    }

    private var color: Color {
        switch model.link {
        case .updateRequired: return .red
        case .connecting: return .blue
        default: return .orange
        }
    }

    private var title: String {
        switch model.link {
        case .hostOffline: return "Host offline"
        case .updateRequired: return "Update required"
        case .connecting: return "Connecting…"
        default: return "Offline"
        }
    }

    private var detail: String {
        let cached: String
        if let at = model.cachedAt {
            cached = "Showing what was cached \(at.formatted(.relative(presentation: .named))). "
        } else {
            cached = ""
        }
        switch model.link {
        case .hostOffline:
            return cached + "Reconnecting automatically; prompts are disabled."
        case let .updateRequired(msg):
            return "Install the latest version of this app. (\(msg))"
        case .connecting:
            return cached + "Prompts are disabled until connected."
        case let .offline(reason):
            return cached + "Prompts are disabled. Reconnecting automatically. (\(reason))"
        default:
            return cached + "Prompts are disabled."
        }
    }
}

/// "An approval is waiting" row; tapping it opens the approval.
struct ApprovalStatusRow: View {
    @EnvironmentObject private var model: AppModel
    var sessionId: String?
    var onTap: (PendingApprovalModel?) -> Void

    private var relevant: [PendingApprovalModel] {
        guard let sid = sessionId else { return model.approvals }
        return model.approvals.filter { $0.sessionId == sid }
    }

    private var isWaiting: Bool {
        if !relevant.isEmpty { return true }
        if let sid = sessionId {
            return model.session(sid)?.state == SessionStateName.waitingApproval
        }
        return model.anyWaitingApproval
    }

    var body: some View {
        if isWaiting {
            Button {
                onTap(relevant.first)
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: "hand.raised.fill")
                        .foregroundStyle(.orange)
                        .imageScale(.large)
                        .frame(width: 28)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(relevant.count > 1 ? "\(relevant.count) approvals waiting" : "Approval waiting")
                            .foregroundStyle(.primary)
                        Group {
                            if let a = relevant.first {
                                Text("\(a.connectorName): \(a.tool)")
                            } else {
                                Text("A session is waiting for a tool-call decision.")
                            }
                        }
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
    }
}

extension View {
    /// A grouped-style card for status rows shown outside a `List`.
    func statusCard() -> some View {
        padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(uiColor: .secondarySystemGroupedBackground),
                        in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

/// A session's state as trailing detail text, coloured only when it needs
/// attention or is active (the standard list "value" style).
struct StateText: View {
    var state: String

    var body: some View {
        Text(SessionStateName.label(state))
            .font(.subheadline)
            .foregroundStyle(color)
            .lineLimit(1)
            .fixedSize()
    }

    private var color: Color {
        switch state {
        case SessionStateName.running, SessionStateName.starting: return .blue
        case SessionStateName.waitingApproval: return .orange
        case SessionStateName.failed: return .red
        default: return .secondary
        }
    }
}

/// A connector's icon (from `connector.list`), or its initial on a tile
/// when the host sent none.
struct ConnectorIcon: View {
    @EnvironmentObject private var model: AppModel
    var id: String?
    var name: String
    var size: CGFloat = 30

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.27, style: .continuous)
        Group {
            if let image = model.connectorIcon(id: id, name: name) {
                Image(uiImage: image)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
                    .padding(size * 0.12)
                    .frame(width: size, height: size)
                    .background(Color.white, in: shape)
            } else {
                Text(name.prefix(1).uppercased())
                    .font(.system(size: size * 0.5, weight: .bold, design: .rounded))
                    .frame(width: size, height: size)
                    .background(Color.white.opacity(0.1), in: shape)
            }
        }
        .clipShape(shape)
        .accessibilityHidden(true)
    }
}
