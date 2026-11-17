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

/// Turn raw events into rows, laid out like the desktop transcript:
/// tool results are merged into their tool-call rows.
func buildRows(_ events: [HostEvent]) -> [TranscriptRow] {
    var rows: [TranscriptRow] = []
    var toolIndex: [String: Int] = [:]
    for e in events {
        let p = e.payload
        switch e.type {
        case EventKind.prompt:
            rows.append(.prompt(
                seq: e.seq,
                text: p["text"].string ?? "",
                attachments: AttachmentInfo.list(from: p["attachments"]),
                device: p["device"].string,
                ts: e.ts
            ))
        case EventKind.agentMessage:
            rows.append(.agentMessage(seq: e.seq, text: p["text"].string ?? ""))
        case EventKind.agentThought:
            rows.append(.thought(seq: e.seq, text: p["text"].string ?? ""))
        case EventKind.toolCall:
            let id = p["tool_call_id"].string ?? "seq\(e.seq)"
            let row = ToolRow(
                seq: e.seq,
                callId: id,
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
            toolIndex[id] = rows.count
            rows.append(.tool(row))
        case EventKind.toolResult:
            let id = p["tool_call_id"].string ?? ""
            if let i = toolIndex[id], case var .tool(row) = rows[i] {
                row.status = p["status"].string
                row.output = p["output"].string
                row.truncated = p["truncated"].bool ?? false
                row.sandboxDenied = p["sandbox_denied"].bool ?? false
                row.exitCode = p["exit_code"].int
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
                device: p["device"].string ?? "",
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
            let stop = p["stop_reason"].string ?? "end_turn"
            let detail = p["host_reason"].string ?? p["error"].string
            rows.append(.turnEnd(seq: e.seq, stopReason: stop, detail: detail))
        default:
            break
        }
    }
    return rows
}
