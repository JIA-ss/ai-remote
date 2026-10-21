import Foundation

// Coding-agent chats on a host: the host's numbered events (docs/tools.md 5.15) folded into what the
// chat view shows. Pure, like the desktop app's `reduceChat` (apps/desktop/src/lib/chat.ts), so the
// folding runs without a phone. Plans, diffs and tool output are left out on the phone: a tool row
// shows its status and title only.

/// A session as `chat_sessions` reports it.
public struct PhoneChatSession: Identifiable, Sendable, Equatable {
    public let id: String
    public let name: String
    public let cwd: String
    public let state: String
    public let byDevice: String
    public let principal: String?
    public let turns: Int
    public let lastActivity: Double
    public init(_ v: JSONValue) {
        id = v["id"].string ?? ""
        name = v["name"].string ?? v["runtime"].string ?? ""
        cwd = v["cwd"].string ?? ""
        state = v["state"].string ?? ""
        byDevice = v["by"]["device"].string ?? ""
        principal = v["by"]["principal"].string
        turns = Int(v["turns"].number ?? 0)
        lastActivity = v["last_activity"].number ?? 0
    }
    /// The folder's last component, for a one-line title.
    public var folder: String {
        let trimmed = cwd.hasSuffix("/") || cwd.hasSuffix("\\") ? String(cwd.dropLast()) : cwd
        return trimmed.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? cwd
    }
}

public struct PhoneChatTool: Equatable, Sendable {
    public let callID: String
    public var title: String
    public var kind: String
    public var status: String
}

public enum PhoneChatBlock: Equatable, Sendable, Identifiable {
    case text(id: String, text: String)
    case thought(id: String, text: String, startedMs: Double, endedMs: Double)
    case tool(PhoneChatTool)
    case question(qid: String, prompt: String)
    case notice(level: String, text: String)

    public var id: String {
        switch self {
        case .text(let id, _): "text-\(id)"
        case .thought(let id, _, _, _): "thought-\(id)"
        case .tool(let tool): "tool-\(tool.callID)"
        case .question(let qid, _): "question-\(qid)"
        case .notice(_, let text): "notice-\(text.hashValue)"
        }
    }
}

public struct PhoneChatTurn: Identifiable, Equatable, Sendable {
    public let n: Int
    public var id: Int { n }
    public var userText: String?
    public var userBy = ""
    public var blocks: [PhoneChatBlock] = []
    /// `running`, `completed`, `failed` or `interrupted`.
    public var status = "running"
    public var reason: String?
    public var durationMs: Double?
    public init(n: Int) { self.n = n }

    /// The turn's end in words, or nil while it runs.
    public var ending: String? {
        let took = durationMs.map { $0 >= 1000 ? String(format: " · %.1f 秒", $0 / 1000) : " · \(Int($0)) 毫秒" } ?? ""
        let why = reason.map { "：\($0)" } ?? ""
        return switch status {
        case "completed": "完成" + took
        case "interrupted": "已停止" + took + why
        case "failed": "失败" + took + why
        default: nil
        }
    }
}

public struct PhoneChatLog: Equatable, Sendable {
    /// Turns kept in memory; the host keeps the whole transcript.
    public static let maxTurns = 200
    public var seq = 0
    public var state = "idle"
    public var queued: [String] = []
    public var turns: [PhoneChatTurn] = []
    /// Notices outside any turn (a restart, a refused command).
    public var notices: [String] = []
    public init() {}

    /// The agent is working on a turn (the stop button is useful).
    public var busy: Bool { state == "running" || state == "interrupting" || state == "starting" }

    private mutating func turn(_ n: Int) -> Int {
        if let i = turns.lastIndex(where: { $0.n == n }) { return i }
        turns.append(PhoneChatTurn(n: n))
        if turns.count > Self.maxTurns { turns.removeFirst(turns.count - Self.maxTurns) }
        return turns.count - 1
    }
    private mutating func note(_ text: String) {
        notices.append(text)
        if notices.count > 20 { notices.removeFirst(notices.count - 20) }
    }

    /// Folds one event line. Events at or below the last sequence number (a replay overlapping what is
    /// here) change nothing.
    public mutating func apply(_ e: JSONValue) {
        let t = e["t"].string ?? ""
        let seqNumber = Int(e["seq"].number ?? 0)
        if seqNumber < 0 {
            // Not part of the conversation (a refused command): shown, never numbered.
            if t == "notice", let text = e["text"].string { note(text) }
            return
        }
        guard seqNumber > seq else { return }
        seq = seqNumber
        let n = Int(e["turn"].number ?? 0)
        let ts = e["ts"].number ?? 0
        switch t {
        case "state":
            state = e["state"].string ?? state
            queued = e["queued"].array.compactMap { $0["text"].string }
            return
        case "user":
            // The user line comes right before its turn starts (whose number is one more).
            let i = turn(n + 1)
            turns[i].userText = e["text"].string ?? ""
            turns[i].userBy = e["by"]["device"].string ?? ""
            return
        case "turn_start":
            _ = turn(n)
            return
        case "notice":
            guard e["level"].string != "debug", let text = e["text"].string else { return }
            if n > 0, let i = turns.lastIndex(where: { $0.n == n && $0.status == "running" }) {
                turns[i].blocks.append(.notice(level: e["level"].string ?? "info", text: text))
            } else {
                note(text)
            }
            return
        default: break
        }
        let i = turn(n)
        switch t {
        case "message", "thought":
            let id = e["id"].string ?? ""
            let text = e["text"].string ?? ""
            let delta = e["delta"] == .bool(true)
            let at = turns[i].blocks.firstIndex { block in
                switch block {
                case .text(let x, _): t == "message" && x == id
                case .thought(let x, _, _, _): t == "thought" && x == id
                default: false
                }
            }
            if let at {
                switch turns[i].blocks[at] {
                case .text(_, let old): turns[i].blocks[at] = .text(id: id, text: delta ? old + text : text)
                case .thought(_, let old, let started, _):
                    turns[i].blocks[at] = .thought(id: id, text: delta ? old + text : text, startedMs: started, endedMs: ts)
                default: break
                }
            } else if t == "message" {
                turns[i].blocks.append(.text(id: id, text: text))
            } else {
                turns[i].blocks.append(.thought(id: id, text: text, startedMs: ts, endedMs: ts))
            }
        case "tool_call":
            turns[i].blocks.append(.tool(PhoneChatTool(callID: e["call_id"].string ?? "", title: e["title"].string ?? "",
                                                      kind: e["kind"].string ?? "other", status: e["status"].string ?? "pending")))
        case "tool_update":
            let id = e["call_id"].string ?? ""
            if let at = turns[i].blocks.firstIndex(where: { if case .tool(let x) = $0 { x.callID == id } else { false } }),
               case .tool(var tool) = turns[i].blocks[at] {
                if let status = e["status"].string { tool.status = status }
                if let title = e["title"].string, !title.isEmpty { tool.title = title }
                turns[i].blocks[at] = .tool(tool)
            }
        case "question":
            turns[i].blocks.append(.question(qid: e["qid"].string ?? "", prompt: e["prompt"].string ?? ""))
        case "turn_end":
            let status = e["status"].string ?? "completed"
            turns[i].status = status
            turns[i].reason = e["reason"].string
            turns[i].durationMs = e["duration_ms"].number
            // Tools still shown as running did not report an end; the turn's end is theirs.
            for (k, block) in turns[i].blocks.enumerated() {
                if case .tool(var tool) = block, tool.status == "pending" || tool.status == "in_progress" {
                    tool.status = status == "completed" ? "completed" : "failed"
                    turns[i].blocks[k] = .tool(tool)
                }
            }
        default: break
        }
    }

    public mutating func apply(line: String) {
        if let event = try? JSONValue.parse(line) { apply(event) }
    }
}

/// Phone wording for agent states.
public func phoneChatStateName(_ state: String) -> String {
    switch state {
    case "starting": "正在启动"
    case "idle": "空闲"
    case "running": "工作中"
    case "interrupting": "正在停止"
    case "parked": "已休眠"
    case "ended": "已结束"
    default: state
    }
}
