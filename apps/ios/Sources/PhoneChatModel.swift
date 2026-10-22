import Foundation

// Coding-agent chats on a host: the host's numbered events (docs/tools.md 5.15) folded into what the
// chat view shows. A port of the desktop app's apps/desktop/src/lib/chat.ts (reduceChat, turnRows,
// runSummary, runOutcome, diffRows, stoppedTurn, sendMode, inputCommand, editQueued); keep the two in
// step. Pure, so it runs and is tested on macOS (tools/test-phone-chat-model.sh).
//
// The layout follows Orca's native chat (stablyai/orca, MIT): a turn is a list of rows — the user's
// message, the agent's prose, a folded "run" of tool calls summarized in one sentence (edits included:
// they open to diff cards), reasoning folded behind "思考了 4 秒". Ported up to desktop 8d7900c.

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
    public var folder: String { phoneBaseName(cwd) }
}

/// An agent installed on the host, as `chat_runtimes` reports it.
public struct PhoneChatRuntime: Identifiable, Sendable, Equatable {
    public let id: String
    public let name: String
    public let version: String?
    public let verdict: String
    /// `logged_in`, `logged_out` or nil when the host could not tell.
    public let login: String?
    public init(_ v: JSONValue) {
        id = v["id"].string ?? ""
        name = v["name"].string ?? id
        version = v["version"].string
        verdict = v["verdict"].string ?? ""
        login = v["login"]["state"].string
    }
    public var usable: Bool { verdict == "ok" }
    /// Why it cannot be started, or what to know before starting it.
    public var note: String? {
        switch verdict {
        case "ok": login == "logged_out" ? "未登录，启动后可能无法工作" : nil
        case "missing": "电脑上没有安装"
        case "below_minimum": "版本过旧"
        case "not_executable": "无法运行"
        default: verdict
        }
    }
}

/// What the agent can do, from the chat channel's hello (`caps`).
public struct PhoneChatCaps: Equatable, Sendable {
    public var interrupt = true
    /// A message can be added to the running turn (`mode: "steer"`); hosts without it queue.
    public var steer = false
    public var questions = false
    public init() {}
    public init(_ v: JSONValue) {
        interrupt = v["interrupt"] != .bool(false)
        steer = v["steer"] == .bool(true)
        questions = v["questions"] == .bool(true)
    }
}

public enum PhoneToolContent: Equatable, Sendable {
    case text(String)
    case diff(path: String, old: String?, new: String)
    init?(_ v: JSONValue) {
        switch v["type"].string {
        case "text": self = .text(v["text"].string ?? "")
        case "diff": self = .diff(path: v["path"].string ?? "", old: v["old"].string, new: v["new"].string ?? "")
        default: return nil
        }
    }
}

public struct PhoneChatTool: Equatable, Sendable {
    public let callID: String
    public var title: String
    /// read, edit, delete, move, search, execute, think, fetch, other.
    public var kind: String
    /// pending, in_progress, completed, failed.
    public var status: String
    public var input: JSONValue?
    public var locations: [String] = []
    public var content: [PhoneToolContent] = []
    public var truncated = false
    public var startedMs: Double
    public var endedMs: Double?
    /// Cut short because the turn was stopped: not a failure of the tool.
    public var stopped = false
    public var running: Bool { status == "in_progress" || status == "pending" }
}

public struct PhoneThought: Equatable, Sendable {
    public let id: String
    public var text: String
    public var startedMs: Double
    public var endedMs: Double
}

public struct PhonePlanEntry: Equatable, Sendable {
    public let content: String
    public let status: String
}

public struct PhoneQuestion: Equatable, Sendable {
    public let qid: String
    public let prompt: String
    public let options: [String]
    public var answered = false
    public var answer: String?
}

public struct PhoneQueued: Equatable, Sendable, Identifiable {
    public let id: String
    public let text: String
}

/// One block of a turn, in the order it happened.
public enum PhoneChatBlock: Equatable, Sendable {
    case text(id: String, text: String)
    case thought(PhoneThought)
    case tool(PhoneChatTool)
    case plan([PhonePlanEntry])
    case question(PhoneQuestion)
    case notice(level: String, code: String, text: String)
    /// A message added to the running turn (`steer`).
    case user(text: String, by: String, ts: Double)
}

public struct PhoneChatUsage: Equatable, Sendable {
    public var input: Double?
    public var output: Double?
    public var contextWindow: Double?
    public var contextUsed: Double?
}

public struct PhoneChatTurn: Identifiable, Equatable, Sendable {
    public let n: Int
    public var id: Int { n }
    public var userText: String?
    public var userBy = ""
    public var userTs: Double = 0
    public var blocks: [PhoneChatBlock] = []
    /// `running`, `completed`, `failed` or `interrupted`.
    public var status = "running"
    public var startedMs: Double?
    public var finalText: String?
    public var reason: String?
    public var durationMs: Double?
    public var usage: PhoneChatUsage?
    public init(n: Int) { self.n = n }

    /// The turn's end in words, or nil while it runs.
    public var ending: String? {
        let took = durationMs.map { " · " + phoneDuration($0) } ?? ""
        let why = reason.map { "：\($0)" } ?? ""
        return switch status {
        case "completed": "完成" + took
        case "interrupted": "已停止" + took + why
        case "failed": "失败" + took + why
        default: nil
        }
    }
    /// The agent's last words, for copying.
    public var reply: String {
        if let finalText, !finalText.isEmpty { return finalText }
        for block in blocks.reversed() { if case .text(_, let text) = block { return text } }
        return ""
    }
}

public struct PhoneContextUsage: Equatable, Sendable {
    public let used: Double
    public let window: Double
    public var fraction: Double { max(0, min(1, used / window)) }
}

public struct PhoneChatNotice: Equatable, Sendable {
    public let level: String
    public let code: String
    public let text: String
}

public struct PhoneChatLog: Equatable, Sendable {
    /// Turns kept in memory; the host keeps the whole transcript.
    public static let maxTurns = 200
    public var seq = 0
    public var state = "idle"
    public var queued: [PhoneQueued] = []
    public var turns: [PhoneChatTurn] = []
    /// Notices outside any turn (a restart, a refused command).
    public var notices: [PhoneChatNotice] = []
    /// How full the agent's context window is, from its last report that had both numbers.
    public var context: PhoneContextUsage?
    public init() {}

    /// The agent is working on a turn (the stop button is useful).
    public var busy: Bool { state == "running" || state == "interrupting" || state == "starting" }

    private mutating func touch(_ n: Int) -> Int {
        if let i = turns.firstIndex(where: { $0.n == n }) { return i }
        var at = turns.count
        while at > 0 && turns[at - 1].n > n { at -= 1 }
        turns.insert(PhoneChatTurn(n: n), at: at)
        if turns.count > Self.maxTurns {
            let drop = turns.count - Self.maxTurns
            turns.removeFirst(drop)
            at -= drop
        }
        return max(at, 0)
    }
    private mutating func note(_ level: String, _ code: String, _ text: String) {
        notices.append(PhoneChatNotice(level: level, code: code, text: text))
        if notices.count > 20 { notices.removeFirst(notices.count - 20) }
    }

    /// Folds one event. Events at or below the last sequence number (a replay overlapping what is
    /// here) change nothing.
    public mutating func apply(_ e: JSONValue) {
        let t = e["t"].string ?? ""
        let seqNumber = Int(e["seq"].number ?? 0)
        if seqNumber < 0 {
            // Not part of the conversation (a refused command): shown, never numbered.
            if t == "notice" { note(e["level"].string ?? "info", e["code"].string ?? "", e["text"].string ?? "") }
            return
        }
        guard seqNumber > seq else { return }
        seq = seqNumber
        let n = Int(e["turn"].number ?? 0)
        let ts = e["ts"].number ?? 0
        switch t {
        case "state":
            state = e["state"].string ?? state
            queued = e["queued"].array.map { PhoneQueued(id: $0["id"].string ?? "", text: $0["text"].string ?? "") }
            return
        case "user":
            let text = e["text"].string ?? "", by = e["by"]["device"].string ?? ""
            if e["steer"] == .bool(true) {
                // Added to a running turn: shown where it landed, inside that turn. The host numbers it with
                // the running turn; an older host may number it as the next one, so the running turn wins.
                let target = turns.last(where: { $0.status == "running" })?.n ?? (turns.contains { $0.n == n } ? n : n + 1)
                let i = touch(target)
                turns[i].blocks.append(.user(text: text, by: by, ts: ts))
                return
            }
            // The user line comes right before its turn starts (whose number is one more).
            let i = touch(n + 1)
            turns[i].userText = text; turns[i].userBy = by; turns[i].userTs = ts
            return
        case "turn_start":
            let i = touch(n)
            turns[i].startedMs = ts
            return
        case "notice":
            let level = e["level"].string ?? "info"
            guard level != "debug" else { return }
            let code = e["code"].string ?? "", text = e["text"].string ?? ""
            if n > 0, let i = turns.firstIndex(where: { $0.n == n && $0.status == "running" }) {
                turns[i].blocks.append(.notice(level: level, code: code, text: text))
            } else {
                note(level, code, text)
            }
            return
        default: break
        }
        let i = touch(n)
        if turns[i].startedMs == nil { turns[i].startedMs = ts }
        switch t {
        case "message", "thought":
            let id = e["id"].string ?? "", text = e["text"].string ?? "", delta = e["delta"] == .bool(true)
            let at = turns[i].blocks.firstIndex { block in
                switch block {
                case .text(let x, _): t == "message" && x == id
                case .thought(let x): t == "thought" && x.id == id
                default: false
                }
            }
            if let at {
                switch turns[i].blocks[at] {
                case .text(_, let old): turns[i].blocks[at] = .text(id: id, text: delta ? old + text : text)
                case .thought(var x):
                    x.text = delta ? x.text + text : text
                    x.endedMs = ts
                    turns[i].blocks[at] = .thought(x)
                default: break
                }
            } else if t == "message" {
                turns[i].blocks.append(.text(id: id, text: text))
            } else {
                turns[i].blocks.append(.thought(PhoneThought(id: id, text: text, startedMs: ts, endedMs: ts)))
            }
        case "tool_call":
            let status = e["status"].string ?? "pending"
            let input: JSONValue? = e["input"] == .null ? nil : e["input"]
            turns[i].blocks.append(.tool(PhoneChatTool(
                callID: e["call_id"].string ?? "", title: e["title"].string ?? "", kind: e["kind"].string ?? "other", status: status,
                input: input, locations: e["locations"].array.compactMap(\.string), content: e["content"].array.compactMap(PhoneToolContent.init),
                startedMs: ts, endedMs: status == "completed" || status == "failed" ? ts : nil)))
        case "tool_update":
            let id = e["call_id"].string ?? ""
            if let at = turns[i].blocks.firstIndex(where: { if case .tool(let x) = $0 { x.callID == id } else { false } }),
               case .tool(var tool) = turns[i].blocks[at] {
                if let status = e["status"].string { tool.status = status }
                tool.content += e["content"].array.compactMap(PhoneToolContent.init)
                tool.truncated = tool.truncated || e["truncated"] == .bool(true)
                if tool.status == "completed" || tool.status == "failed" { tool.endedMs = ts }
                turns[i].blocks[at] = .tool(tool)
            }
        case "plan":
            // One plan per turn, updated in place: the agent rewrites its whole list each time.
            let entries = e["entries"].array.map { PhonePlanEntry(content: $0["content"].string ?? "", status: $0["status"].string ?? "") }
            if let at = turns[i].blocks.firstIndex(where: { if case .plan = $0 { true } else { false } }) {
                turns[i].blocks[at] = .plan(entries)
            } else {
                turns[i].blocks.append(.plan(entries))
            }
        case "question":
            turns[i].blocks.append(.question(PhoneQuestion(qid: e["qid"].string ?? "", prompt: e["prompt"].string ?? "",
                                                           options: e["options"].array.compactMap(\.string))))
        case "usage":
            let window = e["context_window"].number, used = e["context_used"].number
            turns[i].usage = PhoneChatUsage(input: e["input_tokens"].number, output: e["output_tokens"].number, contextWindow: window, contextUsed: used)
            if let window, let used, window > 0 { context = PhoneContextUsage(used: used, window: window) }
        case "turn_end":
            let status = e["status"].string ?? "completed"
            turns[i].status = status
            turns[i].finalText = e["final_text"].string
            turns[i].durationMs = e["duration_ms"].number
            turns[i].reason = e["reason"].string
            // Tools still shown as running did not report an end; the turn's end is theirs.
            for (k, block) in turns[i].blocks.enumerated() {
                if case .tool(var tool) = block, tool.running {
                    tool.status = status == "completed" ? "completed" : "failed"
                    tool.endedMs = ts
                    if status == "interrupted" { tool.stopped = true }
                    turns[i].blocks[k] = .tool(tool)
                }
            }
        default: break
        }
    }

    public mutating func apply(line: String) {
        if let event = try? JSONValue.parse(line) { apply(event) }
    }

    /// Marks a question answered (the answer goes to the host separately).
    public mutating func markAnswered(_ qid: String, answer: String?) {
        for t in turns.indices {
            for b in turns[t].blocks.indices {
                if case .question(var q) = turns[t].blocks[b], q.qid == qid {
                    q.answered = true; q.answer = answer
                    turns[t].blocks[b] = .question(q)
                }
            }
        }
    }

    /// The turn that was stopped last, if nothing ran since: the view offers to continue it.
    public var stoppedTurn: PhoneChatTurn? {
        guard let last = turns.last, last.status == "interrupted", state != "running", state != "ended", queued.isEmpty else { return nil }
        return last
    }

    /// What the agent is doing now (starting, stopping, thinking, asking, tool, writing; nil when idle)
    /// and since when.
    public var phase: (what: String?, since: Double?) {
        if state == "starting" { return ("starting", nil) }
        if state == "interrupting" { return ("stopping", nil) }
        guard state == "running" else { return (nil, nil) }
        let turn = turns.last
        let since = turn?.startedMs
        guard let turn, let last = turn.blocks.last else { return ("thinking", since) }
        if turn.blocks.contains(where: { if case .question(let q) = $0 { !q.answered } else { false } }) { return ("asking", since) }
        if case .tool(let x) = last, x.status == "in_progress" { return ("tool", since) }
        if case .text = last { return ("writing", since) }
        return ("thinking", since)
    }
}

// MARK: - Rows: what a turn shows, in order.

/// The title starts with `word` as a whole word (the desktop's `/^(Task|Agent)\b/`).
private func startsWithWord(_ title: String, _ word: String) -> Bool {
    guard title.hasPrefix(word) else { return false }
    guard let next = title.dropFirst(word.count).first else { return true }
    return !(next.isLetter || next.isNumber || next == "_")
}

/// A sub-agent the agent started (Claude's Task tool); shown as its own row, not folded in a run.
public func phoneIsSubagent(_ x: PhoneChatTool) -> Bool {
    x.kind == "think" && (startsWithWord(x.title, "Task") || startsWithWord(x.title, "Agent"))
}

/// An edit with a diff to show: a diff card inside its run.
public func phoneIsEdit(_ x: PhoneChatTool) -> Bool {
    x.kind == "edit" || x.content.contains { if case .diff = $0 { true } else { false } }
}

public enum PhoneRunMember: Equatable, Sendable {
    case tool(PhoneChatTool)
    case thought(PhoneThought)
}

public enum PhoneChatRow: Equatable, Sendable, Identifiable {
    case user(key: String, text: String, by: String, ts: Double, steer: Bool)
    case text(key: String, text: String, streaming: Bool)
    case thought(key: String, PhoneThought, live: Bool)
    /// Tool calls in a row (with the reasoning between them), summarized in one sentence.
    case run(key: String, members: [PhoneRunMember], tools: [PhoneChatTool], live: Bool)
    case agent(key: String, PhoneChatTool)
    case plan(key: String, [PhonePlanEntry])
    case question(key: String, PhoneQuestion)
    case notice(key: String, level: String, code: String, text: String)

    public var id: String {
        switch self {
        case .user(let k, _, _, _, _), .text(let k, _, _), .thought(let k, _, _), .run(let k, _, _, _),
             .agent(let k, _), .plan(let k, _), .question(let k, _), .notice(let k, _, _, _): k
        }
    }
    public var kind: String {
        switch self {
        case .user: "user"
        case .text: "text"
        case .thought: "thought"
        case .run: "run"
        case .agent: "agent"
        case .plan: "plan"
        case .question: "question"
        case .notice: "notice"
        }
    }
}

private func joinsRun(_ b: PhoneChatBlock) -> Bool {
    if case .tool(let x) = b { return !phoneIsSubagent(x) }
    return false
}

/// The rows of a turn. A running turn's last run and last text are live.
public func phoneTurnRows(_ turn: PhoneChatTurn) -> [PhoneChatRow] {
    var rows: [PhoneChatRow] = []
    let running = turn.status == "running"
    if let text = turn.userText { rows.append(.user(key: "u\(turn.n)", text: text, by: turn.userBy, ts: turn.userTs, steer: false)) }
    let b = turn.blocks
    // The open run, as an index into rows.
    var run: Int?
    func addToRun(_ member: PhoneRunMember, tool: PhoneChatTool?) {
        guard let r = run, case .run(let key, var members, var tools, let live) = rows[r] else { return }
        members.append(member)
        if let tool { tools.append(tool) }
        rows[r] = .run(key: key, members: members, tools: tools, live: live)
    }
    for (i, x) in b.enumerated() {
        if joinsRun(x), case .tool(let tool) = x {
            if run == nil {
                rows.append(.run(key: "r\(turn.n):\(tool.callID)", members: [], tools: [], live: false))
                run = rows.count - 1
            }
            addToRun(.tool(tool), tool: tool)
            continue
        }
        if case .thought(let thought) = x {
            // Reasoning between two tool calls stays inside their run; anywhere else it is its own row.
            var next = i + 1
            while next < b.count, case .thought = b[next] { next += 1 }
            if run != nil, next < b.count, joinsRun(b[next]) {
                addToRun(.thought(thought), tool: nil)
                continue
            }
            run = nil
            rows.append(.thought(key: "t\(turn.n):\(thought.id)", thought, live: false))
            continue
        }
        run = nil
        switch x {
        case .text(let id, let text):
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { rows.append(.text(key: "m\(turn.n):\(id)", text: text, streaming: false)) }
        case .tool(let tool):
            rows.append(.agent(key: "a\(turn.n):\(tool.callID)", tool))
        case .plan(let entries): rows.append(.plan(key: "p\(turn.n)", entries))
        case .question(let q): rows.append(.question(key: "q\(turn.n):\(q.qid)", q))
        case .notice(let level, let code, let text): rows.append(.notice(key: "n\(turn.n):\(i)", level: level, code: code, text: text))
        case .user(let text, let by, let ts): rows.append(.user(key: "s\(turn.n):\(i)", text: text, by: by, ts: ts, steer: true))
        case .thought: break
        }
    }
    if running, let last = rows.indices.last {
        switch rows[last] {
        case .run(let k, let m, let t, _): rows[last] = .run(key: k, members: m, tools: t, live: true)
        case .text(let k, let text, _): rows[last] = .text(key: k, text: text, streaming: true)
        case .thought(let k, let x, _): rows[last] = .thought(key: k, x, live: true)
        default: break
        }
    }
    for r in rows.indices {
        if case .run(let k, let m, let t, _) = rows[r], t.contains(where: \.running) { rows[r] = .run(key: k, members: m, tools: t, live: running) }
    }
    return rows
}

/// What a run did, by kind, in the order each kind first appears.
public struct PhoneRunPart: Equatable, Sendable {
    /// command, read, search, web, change, tool.
    public let cat: String
    public var n: Int
}

public func phoneRunSummary(_ tools: [PhoneChatTool]) -> [PhoneRunPart] {
    var out: [PhoneRunPart] = []
    for x in tools {
        let cat = switch x.kind {
        case "execute": "command"
        case "read": "read"
        case "search": "search"
        case "fetch": "web"
        case "delete", "move": "change"
        default: phoneIsEdit(x) ? "change" : "tool"
        }
        if let i = out.firstIndex(where: { $0.cat == cat }) { out[i].n += 1 } else { out.append(PhoneRunPart(cat: cat, n: 1)) }
    }
    return out
}

/// Lines added and removed by the edits of a run, for its header (nil when it edited nothing).
public func phoneRunDiffStat(_ tools: [PhoneChatTool]) -> (add: Int, del: Int)? {
    var add = 0, del = 0, any = false
    for x in tools {
        guard let st = phoneDiffStat(x.content) else { continue }
        any = true; add += st.add; del += st.del
    }
    return any ? (add, del) : nil
}

/// How a run ended: calls that failed, calls cut short by Stop, and whether all of them finished.
public struct PhoneRunOutcome: Equatable, Sendable {
    public let failed: Int
    public let stopped: Int
    public let done: Bool
}

public func phoneRunOutcome(_ tools: [PhoneChatTool]) -> PhoneRunOutcome {
    PhoneRunOutcome(failed: tools.filter { $0.status == "failed" && !$0.stopped }.count, stopped: tools.filter(\.stopped).count,
                    done: tools.allSatisfy { $0.status == "completed" || $0.status == "failed" })
}

/// The run's sentence, as the desktop's `hc_run_*` / `hc_live_*` strings say it: past tense when done
/// ("运行了 2 条命令，读了 1 个文件 和 编辑了 1 个文件"), present with one leading "正在" while live; the last
/// item joins with "和" (`hc_list_and`).
public func phoneRunSentence(_ tools: [PhoneChatTool], live: Bool) -> String {
    let past = ["command": ("运行了", "条命令"), "read": ("读了", "个文件"), "search": ("搜索了", "次"), "web": ("抓取了", "个网页"),
                "change": ("编辑了", "个文件"), "tool": ("用了", "个工具")]
    let present = ["command": ("运行", "条命令"), "read": ("读", "个文件"), "search": ("搜索", "次"), "web": ("抓取", "个网页"),
                   "change": ("编辑", "个文件"), "tool": ("使用", "个工具")]
    let parts = phoneRunSummary(tools).map { p -> String in
        let (verb, unit) = (live ? present : past)[p.cat] ?? ("", "")
        return "\(verb) \(p.n) \(unit)"
    }
    let list = parts.count > 1 ? parts.dropLast().joined(separator: "，") + " 和 " + parts[parts.count - 1] : parts.first ?? ""
    return (live ? "正在" : "") + list
}

/// A tool's title split into the verb the agent used and what it acted on ("Bash", "npm test").
public func phoneToolLabel(_ x: PhoneChatTool) -> (verb: String, target: String) {
    let title = x.title.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let space = title.firstIndex(where: \.isWhitespace) else { return (title, "") }
    return (String(title[..<space]), String(title[space...]).trimmingCharacters(in: .whitespacesAndNewlines))
}

/// The command of a shell tool, from its input or its title.
public func phoneToolCommand(_ x: PhoneChatTool) -> String? {
    guard x.kind == "execute" else { return nil }
    let cmd = (x.input?["command"] ?? .null) == .null ? x.input?["cmd"] : x.input?["command"]
    if let s = cmd?.string { return s }
    if let parts = cmd?.array, !parts.isEmpty { return parts.map { $0.string ?? $0.text }.joined(separator: " ") }
    let target = phoneToolLabel(x).target
    return target.isEmpty ? nil : target
}

/// The output of a tool as one text (its text contents joined).
public func phoneToolOutput(_ x: PhoneChatTool) -> String {
    x.content.compactMap { if case .text(let t) = $0 { t } else { nil } }.joined(separator: "\n")
}

/// The last `n` lines of a text and how many lines came before them.
public func phoneTailLines(_ text: String, _ n: Int) -> (text: String, hidden: Int) {
    var trimmed = text
    while trimmed.hasSuffix("\n") { trimmed.removeLast() }
    let lines = trimmed.components(separatedBy: "\n")
    if lines.count <= n { return (lines.joined(separator: "\n"), 0) }
    return (lines.suffix(n).joined(separator: "\n"), lines.count - n)
}

// MARK: - Diffs

public enum PhoneDiffRow: Equatable, Sendable {
    case context(String), add(String), del(String)
    /// Lines left out between two changes.
    case gap(Int)
}

/// Lines kept around each change.
private let diffContext = 3
/// Above this many cells the line diff falls back to "all removed, all added" for the middle.
private let diffMaxCells = 4_000_000

/// The edit script between two lists of lines (longest common subsequence).
private func editScript(_ a: [String], _ b: [String]) -> [PhoneDiffRow] {
    var head = 0
    while head < a.count, head < b.count, a[head] == b[head] { head += 1 }
    var tail = 0
    while tail < a.count - head, tail < b.count - head, a[a.count - 1 - tail] == b[b.count - 1 - tail] { tail += 1 }
    var out: [PhoneDiffRow] = a[0..<head].map { .context($0) }
    let am = Array(a[head..<(a.count - tail)]), bm = Array(b[head..<(b.count - tail)])
    let n = am.count, m = bm.count
    if n > 0, m > 0, (n + 1) * (m + 1) <= diffMaxCells {
        // lcs[i][j]: length of the common subsequence of am[i...] and bm[j...].
        let w = m + 1
        var lcs = [UInt32](repeating: 0, count: (n + 1) * w)
        for i in stride(from: n - 1, through: 0, by: -1) {
            for j in stride(from: m - 1, through: 0, by: -1) {
                lcs[i * w + j] = am[i] == bm[j] ? lcs[(i + 1) * w + j + 1] + 1 : max(lcs[(i + 1) * w + j], lcs[i * w + j + 1])
            }
        }
        var i = 0, j = 0
        while i < n, j < m {
            if am[i] == bm[j] {
                out.append(.context(am[i])); i += 1; j += 1
            } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
                out.append(.del(am[i])); i += 1
            } else {
                out.append(.add(bm[j])); j += 1
            }
        }
        while i < n { out.append(.del(am[i])); i += 1 }
        while j < m { out.append(.add(bm[j])); j += 1 }
    } else {
        out += am.map { .del($0) } + bm.map { .add($0) }
    }
    out += a[(a.count - tail)...].map { .context($0) }
    return out
}

/// The rows of a diff card: changes with a few lines around them; stretches between them fold into gaps.
public func phoneDiffRows(_ oldText: String?, _ newText: String) -> [PhoneDiffRow] {
    let a = (oldText ?? "").isEmpty ? [] : (oldText ?? "").components(separatedBy: "\n")
    let b = newText.isEmpty ? [] : newText.components(separatedBy: "\n")
    let script = editScript(a, b)
    var keep = [Bool](repeating: false, count: script.count)
    for (i, l) in script.enumerated() {
        if case .context = l { continue }
        for k in max(0, i - diffContext)...min(script.count - 1, i + diffContext) { keep[k] = true }
    }
    var out: [PhoneDiffRow] = []
    var skipped = 0
    // Only the stretch between two changes is marked; lines left out before the first or after the
    // last change are simply not shown (as in Orca's diff card).
    for (i, l) in script.enumerated() {
        if keep[i] {
            if skipped > 0, !out.isEmpty { out.append(.gap(skipped)) }
            skipped = 0
            out.append(l)
        } else {
            skipped += 1
        }
    }
    return out
}

/// Counts of added and removed lines of a tool's diffs, for its summary; nil without a diff.
public func phoneDiffStat(_ content: [PhoneToolContent]) -> (add: Int, del: Int)? {
    var add = 0, del = 0, any = false
    for c in content {
        guard case .diff(_, let old, let new) = c else { continue }
        any = true
        for l in phoneDiffRows(old, new) {
            if case .add = l { add += 1 } else if case .del = l { del += 1 }
        }
    }
    return any ? (add, del) : nil
}

/// The file name of a path, Windows or Unix.
public func phoneBaseName(_ path: String) -> String {
    path.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? path
}

// MARK: - The composer

/// What sending does while a turn runs: the agent's own way of taking a message mid-turn (`steer`,
/// Claude Code and Codex: it joins the running turn) when its caps say so, else it waits for the turn
/// to end (`queue`). There is no separate choice for the person. nil: nothing runs, a plain message.
public func phoneSendMode(running: Bool, caps: PhoneChatCaps?, interrupt: Bool = false) -> String? {
    guard running else { return nil }
    if interrupt { return "interrupt" }
    return caps?.steer == true ? "steer" : "queue"
}

/// The input command for the host (see chat/1 in docs/architecture.md). Queue is the host's default:
/// the field is left out, so older hosts read it unchanged.
public func phoneInputCommand(id: String, text: String, mode: String? = nil) -> [String: JSONValue] {
    var input: [String: JSONValue] = ["id": .string(id), "text": .string(text)]
    if let mode, mode != "queue" { input["mode"] = .string(mode) }
    return ["input": .object(input)]
}

/// Editing a queued message: the host has no "replace", so the old one is taken back and the new text
/// queued again (it goes to the end of the queue).
public func phoneEditQueued(queuedID: String, newID: String, text: String) -> [[String: JSONValue]] {
    [["dequeue": .object(["id": .string(queuedID)])], phoneInputCommand(id: newID, text: text, mode: "queue")]
}

/// What "继续" sends after a stopped turn (the desktop's `hc_continue_prompt`).
public let phoneContinuePrompt = "从刚才停下的地方继续。先确认上一步有没有做完。"

/// Text for a notice: ours for the codes the view knows, the host's otherwise.
public func phoneNoticeText(code: String, text: String) -> String {
    code == "steer_queued" ? "智能体这一轮结束后会收到这条消息。" : text
}

/// "850 毫秒", "4.2 秒", "38 秒", "2:05".
public func phoneDuration(_ ms: Double) -> String {
    if ms < 1000 { return "\(Int(max(0, ms.rounded()))) 毫秒" }
    if ms < 10_000 { return String(format: "%.1f 秒", ms / 1000) }
    if ms < 60_000 { return "\(Int((ms / 1000).rounded())) 秒" }
    return String(format: "%d:%02d", Int(ms / 60_000), Int((ms.truncatingRemainder(dividingBy: 60_000) / 1000).rounded()) % 60)
}

/// Phone wording for agent states. A session started without a first message is parked until one
/// arrives (the host starts the agent then), so a parked session with no turns is waiting, not asleep.
public func phoneChatStateName(_ state: String, turns: Int = 1) -> String {
    if state == "parked" && turns == 0 { return "等待第一条消息" }
    return switch state {
    case "starting": "正在启动"
    case "idle": "就绪"
    case "running": "工作中"
    case "interrupting": "正在停止"
    case "parked": "已暂停"
    case "ended": "已结束"
    default: state
    }
}
