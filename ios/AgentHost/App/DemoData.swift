#if DEBUG
import Foundation

/// Sample data for screenshots, matching the designs. DEBUG builds only.
///
///     -AgentHostDemo                      sessions list, no pairing, no network
///     -AgentHostDemoScreen <screen>       session | dictating | new | new-login | picker |
///                                         settings | archived | archived-session |
///                                         archive-confirm | pair
///     -AgentHostDemoInstall               the waiting approval is a tool install
struct DemoData {
    enum Screen: String {
        case sessions, session, dictating, new, picker, settings, pair, archived
        /// New session with a login other than the workspace's default.
        case newLogin = "new-login"
        /// An archived session's transcript (read only, Unarchive).
        case archivedSession = "archived-session"
        /// The list, asking before archiving a live session.
        case archiveConfirm = "archive-confirm"
    }

    var screen: Screen
    var hostName = "workstation"
    var deviceName = "Nico's iPhone"
    var hostInfo: HostInfoModel
    var workspaces: [WorkspaceInfo]
    var sessions: [SessionInfo]
    var archived: [SessionInfo]
    var approvals: [PendingApprovalModel]
    var transcripts: [String: Transcript]

    static let flakySession = "demo-flaky"
    static let quickSession = "demo-rust"
    static let archivedSession = "demo-quic"

    /// The screen asked for on the command line (nil without `-AgentHostDemo`).
    static var requestedScreen: Screen? {
        let args = ProcessInfo.processInfo.arguments
        guard args.contains("-AgentHostDemo") else { return nil }
        if let i = args.firstIndex(of: "-AgentHostDemoScreen"), i + 1 < args.count {
            return Screen(rawValue: args[i + 1]) ?? .sessions
        }
        return .sessions
    }

    static func fromLaunchArguments() -> DemoData? {
        guard let screen = requestedScreen else { return nil }
        return DemoData(screen: screen)
    }

    init(screen: Screen) {
        self.screen = screen
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let min: Int64 = 60_000

        hostInfo = HostInfoModel(
            hostId: "host-demo",
            version: "0.1.0",
            os: "Linux",
            protocolVersion: RPC.mediaProtocolVersion,
            agents: [
                Self.agent("claude", "Claude Code", profile: ("claude-work", "Work"), dir: "~/.claude-work"),
                Self.agent("claude", "Claude Code", profile: ("claude-personal", "Personal"), dir: "~/.claude-p"),
                Self.agent("codex", "Codex"),
                Self.agent("opencode", "OpenCode"),
                Self.agent("gemini", "Gemini CLI", loggedIn: false),
            ],
            sandbox: SandboxHealthInfo(backend: "podman", ok: true, runtimeVersion: "5.2.1", problem: nil),
            relay: RelayStatusInfo(configured: true, connected: true, url: "wss://agenthost-relay.example.net", error: nil)
        )

        workspaces = [
            WorkspaceInfo(workspaceId: "ws-quick", name: "Quick tasks", path: "", kind: "scratch", createdAt: now,
                          defaultAgent: "claude", defaultProfileId: "claude-personal"),
            WorkspaceInfo(workspaceId: "ws-relay", name: "relay", path: "/home/nico/Code/relay", kind: "project", createdAt: now,
                          defaultAgent: "claude", defaultProfileId: "claude-work"),
            WorkspaceInfo(workspaceId: "ws-notes", name: "notes", path: "/home/nico/notes", kind: "project", createdAt: now,
                          defaultAgent: "claude"),
        ]

        func session(_ id: String, _ ws: String, _ agent: String, _ profile: String?, _ state: String,
                     _ title: String, ago: Int64, lastSeq: Int64 = 0, reason: String? = nil,
                     archivedAgo: Int64? = nil) -> SessionInfo {
            SessionInfo(sessionId: id, workspaceId: ws, agent: agent, profileId: profile, state: state,
                        stateReason: reason, title: title, createdAt: now - ago * min - 10 * min,
                        updatedAt: now - ago * min, lastSeq: lastSeq,
                        archivedAt: archivedAgo.map { now - $0 * min })
        }
        sessions = [
            session(Self.quickSession, "ws-quick", "claude", "claude-personal", SessionStateName.idle,
                    "What changed in Rust 1.92?", ago: 3, lastSeq: 4),
            session(Self.flakySession, "ws-relay", "claude", "claude-work", SessionStateName.waitingApproval,
                    "Fix the flaky resume test", ago: 1, lastSeq: 5),
            session("demo-oauth", "ws-relay", "codex", "codex", SessionStateName.running,
                    "Add OAuth refresh", ago: 5),
            session("demo-review", "ws-relay", "opencode", "opencode", SessionStateName.idle,
                    "Review the relay worker", ago: 120),
            session("demo-import", "ws-notes", "claude", nil, SessionStateName.failed,
                    "Import old notes", ago: 50, reason: "The agent exited (code 1)"),
            session("demo-weekly", "ws-notes", "claude", nil, SessionStateName.ended,
                    "Weekly summary", ago: 26 * 60),
        ]
        archived = [
            session(Self.archivedSession, "ws-relay", "codex", "codex", SessionStateName.ended,
                    "Spike: QUIC transport", ago: 4 * 24 * 60, lastSeq: 3, archivedAgo: 3 * 24 * 60),
            session("demo-notes-old", "ws-notes", "claude", "claude-personal", SessionStateName.ended,
                    "Draft the release notes", ago: 9 * 24 * 60, archivedAgo: 8 * 24 * 60),
        ]

        let approval = PendingApprovalModel(
            approvalId: "demo-approval",
            sessionId: Self.flakySession,
            connectorId: "github",
            connectorName: "GitHub",
            tool: "create_issue",
            arguments: .object([
                "repo": .string("nico/relay"),
                "title": .string("Resume test: wait for the first event, not the state"),
                "body": .string("`wait_state` polls before the resumed session writes its first event."),
            ]),
            requestedAt: now - min,
            expiresAt: now + 9 * min + 30_000
        )
        approvals = [approval]
        // -AgentHostDemoInstall: the agent asks to install a tool instead.
        if ProcessInfo.processInfo.arguments.contains("-AgentHostDemoInstall") {
            approvals = [PendingApprovalModel(
                approvalId: "demo-approval",
                sessionId: Self.flakySession,
                connectorId: "agenthost",
                connectorName: "AgentHost",
                tool: "install_tool",
                arguments: .object([
                    "command": .string("npm install --global agent-desktop@1.3.1"),
                    "manager": .string("npm"),
                    "package": .string("agent-desktop"),
                    "version": .string("1.3.1"),
                    "reason": .string("You asked me to set up agent-desktop so you can drive desktop apps from the terminal."),
                ]),
                requestedAt: now - min,
                expiresAt: now + 9 * min + 30_000,
                summary: "Install agent-desktop 1.3.1 with npm, on this computer"
            )]
        }

        // 14:02 today, for the prompt's time stamp.
        let start = Calendar.current.date(bySettingHour: 14, minute: 2, second: 0, of: Date()) ?? Date()
        let t0 = Int64(start.timeIntervalSince1970 * 1000)

        var flaky = Transcript(sessionId: Self.flakySession)
        let approvalPayload: JSONValue = .object([
            "approval_id": .string(approval.approvalId),
            "session_id": .string(approval.sessionId),
            "connector_id": .string(approval.connectorId),
            "connector_name": .string(approval.connectorName),
            "tool": .string(approval.tool),
            "arguments": approval.arguments,
            "requested_at": .number(Double(approval.requestedAt)),
            "expires_at": .number(Double(approval.expiresAt)),
        ])
        for e in [
            Self.event(Self.flakySession, 1, EventKind.prompt, t0, [
                "text": .string("The resume test fails about one run in ten on CI. Find out why and fix it."),
                "device": .string("Nico's iPhone"),
            ]),
            Self.event(Self.flakySession, 2, EventKind.toolCall, t0 + 5000, [
                "tool_call_id": .string("call-1"), "name": .string("shell"), "source": .string("sandbox"),
                "arguments": .object(["command": .string("cargo test -p agenthost-host")]),
            ]),
            Self.event(Self.flakySession, 3, EventKind.toolResult, t0 + 60_000, [
                "tool_call_id": .string("call-1"), "status": .string("completed"),
                "output": .string("test result: ok. 48 passed; 0 failed; 0 ignored"), "exit_code": .number(0),
            ]),
            Self.event(Self.flakySession, 4, EventKind.agentMessage, t0 + 90_000, [
                "text": .string("The test passes locally. The flake comes from a race: `wait_state` polls before the resumed session writes its first event. I made the helper wait for the event instead."),
            ]),
            HostEvent(sessionId: Self.flakySession, seq: 5, type: EventKind.approvalRequested,
                      payload: approvalPayload, ts: t0 + 100_000),
        ] { _ = flaky.ingest(e) }

        var rust = Transcript(sessionId: Self.quickSession)
        for e in [
            Self.event(Self.quickSession, 1, EventKind.prompt, now - 4 * min, [
                "text": .string("What changed in Rust 1.92?"), "device": .string("Nico's iPhone"),
            ]),
            Self.event(Self.quickSession, 2, EventKind.toolCall, now - 4 * min + 2000, [
                "tool_call_id": .string("call-1"), "name": .string("fetch"), "source": .string("sandbox"),
                "arguments": .object(["url": .string("https://blog.rust-lang.org/releases/1.92.0")]),
            ]),
            Self.event(Self.quickSession, 3, EventKind.toolResult, now - 4 * min + 4000, [
                "tool_call_id": .string("call-1"), "status": .string("completed"), "output": .string("<html>…"),
            ]),
            Self.event(Self.quickSession, 4, EventKind.agentMessage, now - 3 * min, [
                "text": .string("Rust 1.92 stabilises `async` closures in traits, adds lints for never-type fallback, and speeds up incremental builds by about 8%."),
            ]),
        ] { _ = rust.ingest(e) }

        var quic = Transcript(sessionId: Self.archivedSession)
        let q0 = now - 4 * 24 * 60 * min
        for e in [
            Self.event(Self.archivedSession, 1, EventKind.prompt, q0, [
                "text": .string("Could the relay use QUIC instead of WebSockets? Sketch it, don't merge anything."),
                "device": .string("Nico's iPhone"),
            ]),
            Self.event(Self.archivedSession, 2, EventKind.agentMessage, q0 + 120_000, [
                "text": .string("It could, but Cloudflare Workers can't accept raw QUIC yet, so the relay would need a separate host. I left the sketch in `spike/quic` and changed nothing else."),
            ]),
            Self.event(Self.archivedSession, 3, EventKind.sessionState, q0 + 200_000, [
                "state": .string(SessionStateName.ended),
            ]),
        ] { _ = quic.ingest(e) }

        transcripts = [Self.flakySession: flaky, Self.quickSession: rust, Self.archivedSession: quic]
    }

    /// The text shown while dictating on the `dictating` screen.
    static let dictationText = "Which of those matter for the relay crate and do we need to bump the"

    /// `project.browse` answers for the folder picker.
    static func browse(_ path: String?) -> ProjectBrowseInfo {
        let root = "/home/nico/Code"
        guard let path else {
            return ProjectBrowseInfo(roots: [root], path: nil, parent: nil,
                                     folders: [ProjectFolderInfo(name: "Code", path: root, workspaceId: nil)])
        }
        guard path == root else {
            return ProjectBrowseInfo(roots: [root], path: path, parent: root, folders: [])
        }
        let names = ["agenthost-site", "blog", "dotfiles", "relay", "scratch-ideas"]
        return ProjectBrowseInfo(roots: [root], path: root, parent: nil, folders: names.map {
            ProjectFolderInfo(name: $0, path: "\(root)/\($0)", workspaceId: $0 == "relay" ? "ws-relay" : nil)
        })
    }

    private static func agent(_ agent: String, _ name: String, profile: (String, String)? = nil,
                              dir: String? = nil, loggedIn: Bool = true) -> AgentStatusInfo {
        AgentStatusInfo(
            agent: agent, displayName: name, installed: true, version: "1.0.0", loggedIn: loggedIn,
            available: loggedIn, problem: loggedIn ? nil : "Not logged in on the computer",
            fix: loggedIn ? nil : "\(agent) login", profileId: profile?.0 ?? agent,
            profileName: profile?.1, configDir: dir)
    }

    private static func event(_ sid: String, _ seq: Int64, _ type: String, _ ts: Int64,
                              _ payload: [String: JSONValue]) -> HostEvent {
        HostEvent(sessionId: sid, seq: seq, type: type, payload: .object(payload), ts: ts)
    }
}
#endif
