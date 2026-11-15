import SwiftUI

/// Connection state strip, shown whenever the app is not online.
struct ConnectionBanner: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        if !model.link.isOnline {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: icon)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.subheadline.bold())
                    Text(detail).font(.caption)
                }
                Spacer()
                if model.link == .connecting {
                    ProgressView()
                } else if canRetry {
                    Button("Retry") { model.retryNow() }
                        .font(.caption.bold())
                        .buttonStyle(.bordered)
                }
            }
            .padding(10)
            .foregroundStyle(.primary)
            .background(color.opacity(0.18), in: RoundedRectangle(cornerRadius: 10))
            .padding(.horizontal)
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
            return cached + "The computer running AgentHost is not reachable. Reconnecting automatically; prompts are disabled."
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

/// "An approval is waiting" banner.
struct ApprovalBanner: View {
    @EnvironmentObject private var model: AppModel
    var sessionId: String?
    var onTap: (PendingApprovalModel?) -> Void

    private var relevant: [PendingApprovalModel] {
        guard let sid = sessionId else { return model.approvals }
        return model.approvals.filter { $0.sessionId == sid }
    }

    private var waiting: Bool {
        if !relevant.isEmpty { return true }
        if let sid = sessionId {
            return model.session(sid)?.state == SessionStateName.waitingApproval
        }
        return model.anyWaitingApproval
    }

    var body: some View {
        if waiting {
            Button {
                onTap(relevant.first)
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "hand.raised.fill")
                    VStack(alignment: .leading, spacing: 2) {
                        Text(relevant.count > 1 ? "\(relevant.count) approvals waiting" : "Approval waiting")
                            .font(.subheadline.bold())
                        if let a = relevant.first {
                            Text("\(a.connectorName): \(a.tool)").font(.caption)
                        } else {
                            Text("A session is waiting for a tool-call decision.").font(.caption)
                        }
                    }
                    Spacer()
                    Image(systemName: "chevron.right")
                }
                .padding(10)
                .foregroundStyle(.primary)
                .background(Color.yellow.opacity(0.3), in: RoundedRectangle(cornerRadius: 10))
            }
            .buttonStyle(.plain)
            .padding(.horizontal)
        }
    }
}

struct StateBadge: View {
    var state: String

    var body: some View {
        Text(SessionStateName.label(state))
            .font(.caption2.bold())
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.2), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch state {
        case SessionStateName.running, SessionStateName.starting: return .blue
        case SessionStateName.waitingApproval: return .orange
        case SessionStateName.idle: return .green
        case SessionStateName.failed: return .red
        default: return .secondary
        }
    }
}
