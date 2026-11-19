#if DEBUG
import Foundation
import UIKit

/// Sample data for screenshots, matching the designs. DEBUG builds only.
///
///     -AgentHostDemo                      sessions list, no pairing, no network
///     -AgentHostDemoScreen <screen>       session | dictating | new | new-login | picker |
///                                         settings | archived | archived-session |
///                                         archive-confirm | pair | side | config | paste |
///                                         quick | plain | btw | subagents
///     -AgentHostDemoInstall               the waiting approval is a tool install
///     -AgentHostDemoModelSheet            the session opens with "Select model" shown
///     -AgentHostDemoTitleMenu             the session opens with its title menu open
///     -AgentHostDemoRename                the session opens with "Rename Session" shown
///     -AgentHostDemoSubagent done|running the `subagents` screen opens that subagent's sheet
struct DemoData {
    enum Screen: String {
        case sessions, session, dictating, new, picker, settings, pair, archived
        /// New session with a login other than the workspace's default.
        case newLogin = "new-login"
        /// An archived session's transcript (read only, Unarchive).
        case archivedSession = "archived-session"
        /// The list, asking before archiving a live session.
        case archiveConfirm = "archive-confirm"
        /// The side questions sheet (`/btw`) with two conversations.
        case side
        /// A session with model, effort and fast mode in the composer.
        case config
        /// An image pasted into the composer (an attachment chip).
        case paste
        /// An untitled quick task (titled "Quick task", no worktree).
        case quick
        /// An untitled session in a project without a worktree.
        case plain
        /// A side question (`/btw …`) being typed in the composer.
        case btw
        /// A turn with two subagents in parallel: one done, one running.
        case subagents
        /// The host's screen, streaming (plays `DemoScreen.h264`).
        case screen
        /// Choosing what to show before the stream starts.
        case screenPicker = "screen-picker"
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
    var connectors: [ConnectorInfo] = []
    var screenStatus = ScreenStatusModel(
        policy: ScreenPolicyModel(enabled: true, allowControl: true),
        installed: true,
        modes: [
            ScreenModeInfoModel(mode: ScreenModeName.desktop, available: true, detail: "Hyprland (wayland-1)"),
            ScreenModeInfoModel(mode: ScreenModeName.browser, available: true, detail: "chromium in a separate 1280x800 desktop"),
        ]
    )

    static let flakySession = "demo-flaky"
    static let quickSession = "demo-rust"
    static let archivedSession = "demo-quic"
    static let plainSession = "demo-notes-tidy"
    static let subagentSession = "demo-audit"

    /// The screen asked for on the command line (nil without `-AgentHostDemo`).
    static var requestedScreen: Screen? {
        let args = ProcessInfo.processInfo.arguments
        guard args.contains("-AgentHostDemo") else { return nil }
        if let i = args.firstIndex(of: "-AgentHostDemoScreen"), i + 1 < args.count {
            return Screen(rawValue: args[i + 1]) ?? .sessions
        }
        return .sessions
    }

    /// `-AgentHostDemoModelSheet`: the session screen opens the model sheet.
    static var opensModelSheet: Bool {
        requestedScreen != nil && ProcessInfo.processInfo.arguments.contains("-AgentHostDemoModelSheet")
    }

    /// `-AgentHostDemoTitleMenu`: the session screen opens its title menu.
    static var opensTitleMenu: Bool {
        requestedScreen != nil && ProcessInfo.processInfo.arguments.contains("-AgentHostDemoTitleMenu")
    }

    /// `-AgentHostDemoRename`: the session screen asks for a new title.
    static var opensRename: Bool {
        requestedScreen != nil && ProcessInfo.processInfo.arguments.contains("-AgentHostDemoRename")
    }

    /// `-AgentHostDemoSubagent done|running`: the call id of the subagent
    /// whose sheet the `subagents` screen opens.
    static var opensSubagent: String? {
        guard requestedScreen == .subagents else { return nil }
        let args = ProcessInfo.processInfo.arguments
        guard let i = args.firstIndex(of: "-AgentHostDemoSubagent"), i + 1 < args.count else { return nil }
        return args[i + 1] == "done" ? "agent-auth" : "agent-perf"
    }

    /// The draft on the `btw` screen.
    static let sideDraft = "/btw does the relay crate's MSRV allow async closures?"

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
                     _ title: String?, ago: Int64, lastSeq: Int64 = 0, reason: String? = nil,
                     archivedAgo: Int64? = nil) -> SessionInfo {
            SessionInfo(sessionId: id, workspaceId: ws, agent: agent, profileId: profile, state: state,
                        stateReason: reason, title: title, createdAt: now - ago * min - 10 * min,
                        updatedAt: now - ago * min, lastSeq: lastSeq,
                        archivedAt: archivedAgo.map { now - $0 * min })
        }
        let claudeConfig = [
            SessionConfigOption(
                id: "model", name: "Model", description: nil, category: "model", currentValue: "default",
                options: [
                    SessionConfigValue(value: "default", name: "Default (recommended)",
                                       description: "Opus 4.5 for complex work, Sonnet 4.5 otherwise"),
                    SessionConfigValue(value: "opus", name: "Opus 4.5", description: "Most capable for complex work"),
                    SessionConfigValue(value: "sonnet", name: "Sonnet 4.5", description: "Best for everyday tasks"),
                    SessionConfigValue(value: "haiku", name: "Haiku 4.5", description: "Fastest for quick answers"),
                ]),
            SessionConfigOption(
                id: "effort", name: "Effort", description: nil, category: "thought_level", currentValue: "high",
                options: [
                    SessionConfigValue(value: "low", name: "Low", description: nil),
                    SessionConfigValue(value: "medium", name: "Medium", description: nil),
                    SessionConfigValue(value: "high", name: "High", description: nil),
                    SessionConfigValue(value: "max", name: "Max", description: nil),
                ]),
            SessionConfigOption(
                id: "fast", name: "Fast mode", description: "Faster output from the same model, at a higher cost.",
                category: "model_config", currentValue: "on",
                options: [
                    SessionConfigValue(value: "on", name: "On", description: nil),
                    SessionConfigValue(value: "off", name: "Off", description: nil),
                ]),
            SessionConfigOption(
                id: "mode", name: "Mode", description: nil, category: "mode", currentValue: "default",
                options: [SessionConfigValue(value: "default", name: "Default", description: nil)]),
        ]
        func live(_ s: SessionInfo, branch: String? = nil) -> SessionInfo {
            var s = s
            if let branch {
                s.worktree = SessionWorktree(path: "/home/nico/.agenthost/worktrees/\(branch)", branch: branch)
            }
            if s.agent == "claude" {
                s.configOptions = claudeConfig
                s.sideQuestions = true
            }
            return s
        }
        sessions = [
            // Untitled: called "Quick task".
            live(session(Self.quickSession, "ws-quick", "claude", "claude-personal", SessionStateName.idle,
                         nil, ago: 3, lastSeq: 4)),
            live(session(Self.flakySession, "ws-relay", "claude", "claude-work", SessionStateName.waitingApproval,
                         "Fix the flaky resume test", ago: 1, lastSeq: 10), branch: "agenthost/fix-flaky-resume"),
            live(session("demo-oauth", "ws-relay", "codex", "codex", SessionStateName.running,
                         "Add OAuth refresh", ago: 5), branch: "agenthost/oauth-refresh"),
            live(session("demo-review", "ws-relay", "opencode", "opencode", SessionStateName.idle,
                         "Review the relay worker", ago: 120), branch: "agenthost/review-relay-worker"),
            live(session(Self.subagentSession, "ws-relay", "claude", "claude-work", SessionStateName.running,
                         "Audit the relay worker", ago: 0, lastSeq: 19), branch: "audit-relay"),
            // Untitled, no worktree (notes is not a git repository): called "notes".
            live(session(Self.plainSession, "ws-notes", "claude", "claude-personal", SessionStateName.idle,
                         nil, ago: 8, lastSeq: 2)),
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
        connectors = [ConnectorInfo(connectorId: "github", name: "GitHub", icon: Self.demoIcon())]
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
            Self.event(Self.flakySession, 2, EventKind.agentThought, t0 + 3000, [
                "text": .string("Run the test first, then look at how the helper waits for state."),
            ]),
            Self.event(Self.flakySession, 3, EventKind.toolCall, t0 + 5000, [
                "tool_call_id": .string("call-1"), "name": .string("shell"), "source": .string("sandbox"),
                "arguments": .object(["command": .string("cargo test -p agenthost-host")]),
            ]),
            Self.event(Self.flakySession, 4, EventKind.toolResult, t0 + 60_000, [
                "tool_call_id": .string("call-1"), "status": .string("completed"),
                "output": .string("test result: ok. 48 passed; 0 failed; 0 ignored"), "exit_code": .number(0),
            ]),
            Self.event(Self.flakySession, 5, EventKind.toolCall, t0 + 62_000, [
                "tool_call_id": .string("call-2"), "name": .string("shell"), "source": .string("sandbox"),
                "arguments": .object(["command": .string("rg -n wait_state crates/host/tests")]),
            ]),
            Self.event(Self.flakySession, 6, EventKind.toolResult, t0 + 63_000, [
                "tool_call_id": .string("call-2"), "status": .string("completed"),
                "output": .string("common/mod.rs:212: pub async fn wait_state("), "exit_code": .number(0),
            ]),
            Self.event(Self.flakySession, 7, EventKind.toolCall, t0 + 64_000, [
                "tool_call_id": .string("call-3"), "name": .string("fetch"), "source": .string("sandbox"),
                "arguments": .object(["url": .string("https://docs.rs/tokio/latest/tokio/time/fn.timeout.html")]),
            ]),
            Self.event(Self.flakySession, 8, EventKind.toolResult, t0 + 66_000, [
                "tool_call_id": .string("call-3"), "status": .string("completed"), "output": .string("<html>…"),
            ]),
            Self.event(Self.flakySession, 9, EventKind.agentMessage, t0 + 90_000, [
                "text": .string("The test passes locally. The flake comes from a race: `wait_state` polls before the resumed session writes its first event. I made the helper wait for the event instead."),
            ]),
            HostEvent(sessionId: Self.flakySession, seq: 10, type: EventKind.approvalRequested,
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
        if screen == .side {
            let side1 = "side-msrv", side2 = "side-editions"
            for e in [
                Self.event(Self.quickSession, 5, EventKind.sideQuestion, now - 2 * min, [
                    "side_id": .string(side1), "text": .string("Does the relay crate's MSRV allow async closures?"),
                ]),
                Self.event(Self.quickSession, 6, EventKind.sideMessage, now - 2 * min + 4000, [
                    "side_id": .string(side1),
                    "text": .string("Not yet. `crates/relay` declares `rust-version = \"1.85\"`, and async closures need **1.85** for the syntax but **1.92** for use in traits.\n\n- Bump `rust-version` to `1.92`, or\n- keep the boxed futures in `Handler` for now."),
                ]),
                Self.event(Self.quickSession, 7, EventKind.sideEnd, now - 2 * min + 5000, [
                    "side_id": .string(side1), "stop_reason": .string("end_turn"),
                ]),
                Self.event(Self.quickSession, 8, EventKind.sideQuestion, now - min, [
                    "side_id": .string(side2), "text": .string("Which edition are we on?"),
                ]),
                Self.event(Self.quickSession, 9, EventKind.sideMessage, now - min + 3000, [
                    "side_id": .string(side2), "text": .string("All crates use **edition 2024** (set once in the workspace `Cargo.toml`)."),
                ]),
                Self.event(Self.quickSession, 10, EventKind.sideEnd, now - min + 3500, [
                    "side_id": .string(side2), "stop_reason": .string("end_turn"),
                ]),
            ] { _ = rust.ingest(e) }
        }

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

        var tidy = Transcript(sessionId: Self.plainSession)
        for e in [
            Self.event(Self.plainSession, 1, EventKind.prompt, now - 9 * min, [
                "text": .string("Merge the duplicate meeting notes from last week."), "device": .string("Nico's iPhone"),
            ]),
            Self.event(Self.plainSession, 2, EventKind.agentMessage, now - 8 * min, [
                "text": .string("Merged `2025-11-10 standup.md` into `2025-11-10.md` and removed the duplicate. Nothing else changed."),
            ]),
        ] { _ = tidy.ingest(e) }

        var audit = Transcript(sessionId: Self.subagentSession)
        let a0 = now - 135_000
        let sid = Self.subagentSession
        func call(_ seq: Int64, _ ts: Int64, _ id: String, parent: String, _ name: String,
                  _ args: [String: JSONValue]) -> HostEvent {
            Self.event(sid, seq, EventKind.toolCall, ts, [
                "tool_call_id": .string(id), "name": .string(name), "tool_name": .string(name),
                "source": .string("sandbox"), "parent_tool_call_id": .string(parent), "arguments": .object(args),
            ])
        }
        func result(_ seq: Int64, _ ts: Int64, _ id: String, parent: String, _ out: String) -> HostEvent {
            Self.event(sid, seq, EventKind.toolResult, ts, [
                "tool_call_id": .string(id), "parent_tool_call_id": .string(parent),
                "status": .string("completed"), "output": .string(out),
            ])
        }
        let authPrompt = "Review the relay worker's auth: pairing tokens, device revocation and the WebSocket handshake. Report each issue with the file and line, and how bad it is."
        let perfPrompt = "Measure request latency in relay-worker and find the slowest handler. Report numbers and a suggested fix."
        for e in [
            Self.event(sid, 1, EventKind.prompt, a0, [
                "text": .string("Check the worker for auth bugs and slow paths."), "device": .string("Nico's iPhone"),
            ]),
            Self.event(sid, 2, EventKind.agentMessage, a0 + 4000, [
                "text": .string("I'll split this up: one agent reviews auth, another profiles the hot paths."),
            ]),
            Self.event(sid, 3, EventKind.toolCall, a0 + 5000, [
                "tool_call_id": .string("agent-auth"), "name": .string("Task"), "tool_name": .string("Agent"),
                "source": .string("agent"),
                "arguments": .object(["description": .string("Review auth handling"), "prompt": .string(authPrompt)]),
                "subagent": .object(["description": .string("Review auth handling"),
                                     "agent_type": .string("Explore"), "prompt": .string(authPrompt)]),
            ]),
            Self.event(sid, 4, EventKind.toolCall, now - 38_000, [
                "tool_call_id": .string("agent-perf"), "name": .string("Task"), "tool_name": .string("Agent"),
                "source": .string("agent"),
                "arguments": .object(["description": .string("Profile the hot paths"), "prompt": .string(perfPrompt)]),
                "subagent": .object(["description": .string("Profile the hot paths"),
                                     "agent_type": .string("general-purpose"), "prompt": .string(perfPrompt)]),
            ]),
            call(5, a0 + 9000, "auth-1", parent: "agent-auth", "Read", ["file_path": .string("crates/relay/src/pair.rs")]),
            result(6, a0 + 10_000, "auth-1", parent: "agent-auth", "…"),
            call(7, a0 + 12_000, "auth-2", parent: "agent-auth", "Grep",
                 ["command": .string("rg -n \"token ==\" crates/relay")]),
            result(8, a0 + 13_000, "auth-2", parent: "agent-auth", "src/pair.rs:88: if token == expected {"),
            call(9, a0 + 20_000, "auth-3", parent: "agent-auth", "Read", ["file_path": .string("crates/relay/src/devices.rs")]),
            result(10, a0 + 21_000, "auth-3", parent: "agent-auth", "…"),
            Self.event(sid, 11, EventKind.toolResult, a0 + 77_000, [
                "tool_call_id": .string("agent-auth"), "status": .string("completed"),
                "output": .string("Found **2 issues**: the pairing token is compared with `==`, and revoked devices keep their socket for 30 s.\n\n## Issues\n\n1. **Timing-unsafe token check** — `crates/relay/src/pair.rs:88` compares the pairing token with `==`. Use `subtle::ConstantTimeEq`. *Medium.*\n2. **Revocation lag** — `devices.rs` only re-checks revocation on the 30 s heartbeat, so a revoked device keeps its socket until then. Close its sockets when it's revoked. *High.*\n\nThe WebSocket handshake checks the origin and the token's expiry correctly."),
            ]),
            call(12, now - 34_000, "perf-1", parent: "agent-perf", "Read",
                 ["file_path": .string("src/handlers/tunnel.rs")]),
            result(13, now - 33_000, "perf-1", parent: "agent-perf", "…"),
            call(14, now - 31_000, "perf-2", parent: "agent-perf", "Read",
                 ["file_path": .string("src/handlers/pair.rs")]),
            result(15, now - 30_000, "perf-2", parent: "agent-perf", "…"),
            call(16, now - 28_000, "perf-3", parent: "agent-perf", "Grep",
                 ["command": .string("rg -n \"await\" src/handlers")]),
            result(17, now - 27_000, "perf-3", parent: "agent-perf", "src/handlers/tunnel.rs:41: kv.get(key).await"),
            Self.event(sid, 18, EventKind.subagentMessage, now - 20_000, [
                "parent_tool_call_id": .string("agent-perf"),
                "text": .string("The tunnel handler awaits a KV read on every frame. Benchmarking before and after caching it."),
            ]),
            call(19, now - 12_000, "perf-4", parent: "agent-perf", "Bash",
                 ["command": .string("cargo bench -p relay-worker")]),
        ] { _ = audit.ingest(e) }

        transcripts = [Self.subagentSession: audit, Self.flakySession: flaky, Self.quickSession: rust, Self.archivedSession: quic,
                       Self.plainSession: tidy]
    }

    /// The demo's answer to any side question asked.
    static let sideAnswer = "Short answer: yes. The session keeps going; this answer comes from a fork of the conversation, so nothing you ask here changes what the agent is doing."

    /// A screenshot-like image, as if pasted from the clipboard.
    static func pastedImage() -> UIImage {
        let size = CGSize(width: 390, height: 300)
        return UIGraphicsImageRenderer(size: size).image { ctx in
            UIColor(red: 0.12, green: 0.14, blue: 0.2, alpha: 1).setFill()
            ctx.fill(CGRect(origin: .zero, size: size))
            UIColor.systemBlue.setFill()
            ctx.fill(CGRect(x: 24, y: 40, width: 420, height: 60))
            UIColor.systemOrange.setFill()
            ctx.fill(CGRect(x: 24, y: 120, width: 200, height: 140))
        }
    }

    /// GitHub's mark stand-in, as a PNG data URL (like the host sends).
    private static func demoIcon() -> String? {
        let size = CGSize(width: 64, height: 64)
        let image = UIGraphicsImageRenderer(size: size).image { _ in
            UIColor.black.setFill()
            UIBezierPath(ovalIn: CGRect(origin: .zero, size: size)).fill()
            let config = UIImage.SymbolConfiguration(pointSize: 30, weight: .bold)
            if let glyph = UIImage(systemName: "chevron.left.forwardslash.chevron.right", withConfiguration: config)?
                .withTintColor(.white, renderingMode: .alwaysOriginal) {
                let g = glyph.size
                glyph.draw(in: CGRect(x: (size.width - g.width) / 2, y: (size.height - g.height) / 2,
                                      width: g.width, height: g.height))
            }
        }
        return image.pngData().map { "data:image/png;base64,\($0.base64EncodedString())" }
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
