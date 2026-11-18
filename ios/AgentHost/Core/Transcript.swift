import Foundation

/// The events of one session as this device has seen them, contiguous from
/// seq 1 to `lastSeq`. Events are deduplicated by `seq`; an event that
/// arrives ahead of a gap is held back until the gap is filled.
struct Transcript: Codable {
    var sessionId: String
    var events: [HostEvent] = []
    /// Highest seq such that every event 1...lastSeq is in `events`.
    var lastSeq: Int64 = 0
    /// Out-of-order events waiting for a gap to be filled. Not cached.
    var held: [Int64: HostEvent] = [:]

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case events
        case lastSeq = "last_seq"
    }

    init(sessionId: String) {
        self.sessionId = sessionId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try c.decode(String.self, forKey: .sessionId)
        events = try c.decode([HostEvent].self, forKey: .events)
        lastSeq = try c.decode(Int64.self, forKey: .lastSeq)
        held = [:]
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(sessionId, forKey: .sessionId)
        try c.encode(events, forKey: .events)
        try c.encode(lastSeq, forKey: .lastSeq)
    }

    enum Ingest {
        case appended
        case duplicate
        /// Arrived ahead of a gap: re-subscribe from `lastSeq`.
        case gap
    }

    mutating func ingest(_ e: HostEvent) -> Ingest {
        if e.seq <= lastSeq || held[e.seq] != nil { return .duplicate }
        if e.seq != lastSeq + 1 {
            held[e.seq] = e
            return .gap
        }
        events.append(e)
        lastSeq = e.seq
        while let next = held.removeValue(forKey: lastSeq + 1) {
            events.append(next)
            lastSeq = next.seq
        }
        // Anything at or below lastSeq that is still held is a duplicate.
        held = held.filter { $0.key > lastSeq }
        return .appended
    }
}

// MARK: - Transcript rows

/// One row of the rendered transcript.
enum TranscriptRow: Identifiable {
    case prompt(seq: Int64, text: String, attachments: [AttachmentInfo], device: String?, ts: Int64)
    case agentMessage(seq: Int64, text: String)
    case thought(seq: Int64, text: String)
    case tool(ToolRow)
    /// A call that started a subagent, with the steps it took.
    case subagent(SubagentRun)
    case approvalRequested(seq: Int64, approval: PendingApprovalModel?, connector: String, tool: String)
    case approvalResolved(seq: Int64, decision: String, device: String, timedOut: Bool)
    case netDenied(seq: Int64, host: String, port: Int64?, reason: String)
    case state(seq: Int64, state: String, reason: String?, fix: String?)
    case turnEnd(seq: Int64, stopReason: String, detail: String?)

    var id: String {
        switch self {
        case let .prompt(seq, _, _, _, _): return "e\(seq)"
        case let .agentMessage(seq, _): return "e\(seq)"
        case let .thought(seq, _): return "e\(seq)"
        case let .tool(t): return "t\(t.callId)-\(t.seq)"
        case let .subagent(a): return "a\(a.callId)"
        case let .approvalRequested(seq, _, _, _): return "e\(seq)"
        case let .approvalResolved(seq, _, _, _): return "e\(seq)"
        case let .netDenied(seq, _, _, _): return "e\(seq)"
        case let .state(seq, _, _, _): return "e\(seq)"
        case let .turnEnd(seq, _, _): return "e\(seq)"
        }
    }
}

/// A tool call and (once known) its result.
struct ToolRow {
    var seq: Int64
    var callId: String
    var name: String
    var source: String
    var connectorName: String?
    var arguments: JSONValue
    /// `nil` while running; otherwise `completed`, `failed` or `denied`.
    var status: String?
    var output: String?
    var truncated: Bool
    var sandboxDenied: Bool
    var exitCode: Int64?
}

/// A subagent: the `Agent`/`Task` call that started it, what it did (its
/// own tool calls and messages, which stay out of the main transcript) and,
/// once its call has a result, its report.
struct SubagentRun {
    enum Step {
        case tool(ToolRow)
        case message(seq: Int64, text: String)
    }

    var seq: Int64
    var callId: String
    var description: String
    var agentType: String
    var prompt: String
    var background: Bool
    var startedAt: Int64
    var endedAt: Int64?
    /// `nil` while running; otherwise `completed`, `failed` or `denied`.
    var status: String?
    var report: String?
    var steps: [Step] = []

    var isRunning: Bool { status == nil }
    var failed: Bool { status != nil && status != "completed" }
    var title: String { description.isEmpty ? "Subagent" : description }
    var toolSteps: [ToolRow] { steps.compactMap { if case let .tool(t) = $0 { return t }; return nil } }
    var stepCount: Int { toolSteps.count }
    var latestTool: ToolRow? { toolSteps.last }

    /// "Explore agent".
    var agentLabel: String { agentType.isEmpty ? "Agent" : "\(agentType) agent" }

    /// Milliseconds it ran (so far, given `now`).
    func elapsed(now: Int64) -> Int64 { max(0, (endedAt ?? now) - startedAt) }

    /// Is this `tool.call` payload one that starts a subagent?
    static func starts(_ p: JSONValue) -> Bool {
        if case .object = p["subagent"] { return true }
        let tool = p["tool_name"].string ?? ""
        return tool == "Agent" || tool == "Task"
    }
}

/// "38s", "1m 12s", "1h 4m".
func formatDuration(ms: Int64) -> String {
    let s = ms / 1000
    if s < 60 { return "\(s)s" }
    if s < 3600 { return "\(s / 60)m \(s % 60)s" }
    return "\(s / 3600)h \((s % 3600) / 60)m"
}

/// The first few lines of Markdown as plain text, for a preview.
func markdownPreview(_ text: String, lines: Int = 3) -> String {
    var out: [String] = []
    var inFence = false
    for raw in text.split(separator: "\n", omittingEmptySubsequences: true) {
        var line = raw.trimmingCharacters(in: .whitespaces)
        if line.hasPrefix("```") || line.hasPrefix("~~~") { inFence.toggle(); continue }
        if inFence || line.isEmpty || line.allSatisfy({ "-*_=|: ".contains($0) }) { continue }
        while let f = line.first, "#>".contains(f) { line = String(line.dropFirst()).trimmingCharacters(in: .whitespaces) }
        for m in ["- ", "* ", "+ "] where line.hasPrefix(m) { line = "• " + line.dropFirst(2) }
        for token in ["**", "__", "`"] { line = line.replacingOccurrences(of: token, with: "") }
        // [text](url) → text
        while let open = line.range(of: "]("), let lb = line[..<open.lowerBound].lastIndex(of: "["),
              let close = line[open.upperBound...].firstIndex(of: ")") {
            let label = line[line.index(after: lb)..<open.lowerBound]
            line.replaceSubrange(lb...close, with: label)
        }
        out.append(line)
        if out.count == lines { break }
    }
    return out.joined(separator: "\n")
}

/// Turn raw events into rows, laid out like the desktop transcript:
/// tool results are merged into their tool-call rows. A subagent is one
/// row; its own tool calls and messages go into that row, not the main
/// flow.
func buildRows(_ events: [HostEvent]) -> [TranscriptRow] {
    var rows: [TranscriptRow] = []
    var toolIndex: [String: Int] = [:]
    /// A subagent's call id → its row.
    var agentIndex: [String: Int] = [:]
    /// A child tool call's id → (its subagent's row, its step).
    var childIndex: [String: (Int, Int)] = [:]
    func withAgent(_ id: String, _ f: (inout SubagentRun) -> Void) -> Bool {
        guard let i = agentIndex[id], case var .subagent(a) = rows[i] else { return false }
        f(&a)
        rows[i] = .subagent(a)
        return true
    }
    func toolRow(_ e: HostEvent) -> ToolRow {
        let p = e.payload
        return ToolRow(
            seq: e.seq,
            callId: p["tool_call_id"].string ?? "seq\(e.seq)",
            name: p["name"].string ?? "tool",
            source: p["source"].string ?? "sandbox",
            connectorName: p["connector_name"].string,
            arguments: p["arguments"],
            status: nil,
            output: nil,
            truncated: false,
            sandboxDenied: false,
            exitCode: nil
        )
    }
    func applyResult(_ p: JSONValue, to row: inout ToolRow) {
        row.status = p["status"].string
        row.output = p["output"].string
        row.truncated = p["truncated"].bool ?? false
        row.sandboxDenied = p["sandbox_denied"].bool ?? false
        row.exitCode = p["exit_code"].int
    }
    for e in events {
        let p = e.payload
        // A subagent's own steps: into its row, never the main flow.
        let parent = p["parent_tool_call_id"].string ?? ""
        let isChildResult = e.type == EventKind.toolResult && childIndex[p["tool_call_id"].string ?? ""] != nil
        if isChildResult || (!parent.isEmpty &&
            [EventKind.toolCall, EventKind.toolResult, EventKind.subagentMessage].contains(e.type)) {
            switch e.type {
            case EventKind.toolCall:
                // (A subagent inside a subagent is one more step of the outer one.)
                let row = toolRow(e)
                _ = withAgent(parent) { a in
                    childIndex[row.callId] = (agentIndex[parent]!, a.steps.count)
                    a.steps.append(.tool(row))
                }
            case EventKind.toolResult:
                let id = p["tool_call_id"].string ?? ""
                if let (i, step) = childIndex[id], case var .subagent(a) = rows[i],
                   step < a.steps.count, case var .tool(t) = a.steps[step] {
                    applyResult(p, to: &t)
                    a.steps[step] = .tool(t)
                    rows[i] = .subagent(a)
                }
            default:
                let text = p["text"].string ?? ""
                if !text.isEmpty {
                    _ = withAgent(parent) { $0.steps.append(.message(seq: e.seq, text: text)) }
                }
            }
            continue
        }
        switch e.type {
        case EventKind.prompt:
            rows.append(.prompt(
                seq: e.seq,
                text: p["text"].string ?? "",
                attachments: AttachmentInfo.list(from: p["attachments"]),
                device: p["device"].string.map(deviceDisplayName),
                ts: e.ts
            ))
        case EventKind.agentMessage:
            rows.append(.agentMessage(seq: e.seq, text: p["text"].string ?? ""))
        case EventKind.agentThought:
            rows.append(.thought(seq: e.seq, text: p["text"].string ?? ""))
        case EventKind.toolCall:
            let row = toolRow(e)
            if SubagentRun.starts(p) {
                let sub = p["subagent"]
                let args = p["arguments"]
                agentIndex[row.callId] = rows.count
                rows.append(.subagent(SubagentRun(
                    seq: e.seq,
                    callId: row.callId,
                    description: sub["description"].string ?? args["description"].string ?? "",
                    agentType: sub["agent_type"].string ?? args["subagent_type"].string ?? "",
                    prompt: sub["prompt"].string ?? args["prompt"].string ?? "",
                    background: p["background"].bool ?? args["run_in_background"].bool ?? false,
                    startedAt: e.ts
                )))
            } else {
                toolIndex[row.callId] = rows.count
                rows.append(.tool(row))
            }
        case EventKind.toolResult:
            let id = p["tool_call_id"].string ?? ""
            if withAgent(id, { a in
                a.status = p["status"].string ?? "completed"
                a.report = p["output"].string
                a.endedAt = e.ts
            }) {
                break
            }
            if let i = toolIndex[id], case var .tool(row) = rows[i] {
                applyResult(p, to: &row)
                rows[i] = .tool(row)
            } else {
                // A result whose call we never saw: show it on its own.
                rows.append(.tool(ToolRow(
                    seq: e.seq,
                    callId: id.isEmpty ? "seq\(e.seq)" : id,
                    name: p["name"].string ?? "tool",
                    source: p["source"].string ?? "sandbox",
                    connectorName: nil,
                    arguments: .null,
                    status: p["status"].string,
                    output: p["output"].string,
                    truncated: p["truncated"].bool ?? false,
                    sandboxDenied: p["sandbox_denied"].bool ?? false,
                    exitCode: p["exit_code"].int
                )))
            }
        case EventKind.approvalRequested:
            rows.append(.approvalRequested(
                seq: e.seq,
                approval: p.decode(PendingApprovalModel.self),
                connector: p["connector_name"].string ?? "connector",
                tool: p["tool"].string ?? "tool"
            ))
        case EventKind.approvalResolved:
            rows.append(.approvalResolved(
                seq: e.seq,
                decision: p["decision"].string ?? "",
                device: deviceDisplayName(p["device"].string ?? ""),
                timedOut: p["timed_out"].bool ?? false
            ))
        case EventKind.netDenied:
            rows.append(.netDenied(
                seq: e.seq,
                host: p["host"].string ?? "?",
                port: p["port"].int,
                reason: p["reason"].string ?? ""
            ))
        case EventKind.sessionState:
            rows.append(.state(
                seq: e.seq,
                state: p["state"].string ?? "",
                reason: p["reason"].string,
                fix: p["fix"].string
            ))
        case EventKind.turnEnd:
            // A subagent the turn waited on can't outlive it (one in the
            // background can): if it never reported, it was stopped.
            for i in agentIndex.values {
                if case var .subagent(a) = rows[i], a.isRunning, !a.background {
                    a.status = "interrupted"
                    a.endedAt = e.ts
                    rows[i] = .subagent(a)
                }
            }
            let stop = p["stop_reason"].string ?? "end_turn"
            let detail = p["host_reason"].string ?? p["error"].string
            rows.append(.turnEnd(seq: e.seq, stopReason: stop, detail: detail))
        default:
            break
        }
    }
    return rows
}

/// The subagents still running in these events.
func runningSubagents(_ events: [HostEvent]) -> [SubagentRun] {
    buildRows(events).compactMap { if case let .subagent(a) = $0, a.isRunning { return a }; return nil }
}

/// How many subagents are still running, without building the rows (for
/// the sessions list).
func runningSubagentCount(_ events: [HostEvent]) -> Int {
    var running: [String: Bool] = [:]  // call id → background
    for e in events {
        let p = e.payload
        switch e.type {
        case EventKind.toolCall:
            if (p["parent_tool_call_id"].string ?? "").isEmpty, SubagentRun.starts(p),
               let id = p["tool_call_id"].string {
                running[id] = p["background"].bool ?? p["arguments"]["run_in_background"].bool ?? false
            }
        case EventKind.toolResult:
            if let id = p["tool_call_id"].string { running[id] = nil }
        case EventKind.turnEnd:
            running = running.filter { $0.value }
        default:
            break
        }
    }
    return running.count
}

/// A device as people read it: its name. Older events say "iPhone (dev_…)";
/// the id in brackets is left out.
func deviceDisplayName(_ label: String) -> String {
    guard label.hasSuffix(")"), let r = label.range(of: " (dev_", options: .backwards) else { return label }
    return String(label[..<r.lowerBound])
}
