// The iPhone chat model (apps/ios/Sources/PhoneChatModel.swift), checked with the same cases as the
// desktop's tools/chat-state.test.mjs so the two folds stay in step. Run by tools/test-phone-chat-model.sh.
import Foundation

nonisolated(unsafe) var failures = 0
nonisolated(unsafe) var current = ""
func check(_ ok: Bool, _ what: String, file: String = #file, line: Int = #line) {
    if !ok { failures += 1; print("FAIL  \(current): \(what) (line \(line))") }
}
func test(_ name: String, _ body: () -> Void) {
    current = name
    let before = failures
    body()
    if failures == before { print("ok    \(name)") }
}

nonisolated(unsafe) var seq = 0
func ev(_ turn: Int, _ fields: [String: JSONValue]) -> JSONValue {
    seq += 1
    var e = fields
    e["seq"] = .number(Double(seq)); e["ts"] = .number(Double(1000 + seq * 100)); e["turn"] = .number(Double(turn))
    return .object(e)
}
func s(_ v: String) -> JSONValue { .string(v) }
func by(_ device: String) -> JSONValue { .object(["device": s(device)]) }
func fold(_ events: [JSONValue]) -> PhoneChatLog {
    var log = PhoneChatLog()
    for e in events { log.apply(e) }
    return log
}
func tools(_ turn: PhoneChatTurn) -> [PhoneChatTool] { turn.blocks.compactMap { if case .tool(let x) = $0 { x } else { nil } } }

func conversation() -> [JSONValue] {
    seq = 0
    return [
        ev(0, ["t": s("state"), "state": s("running"), "queued": .array([])]),
        ev(0, ["t": s("user"), "input_id": s("u1"), "by": by("laptop"), "text": s("fix the build")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("message"), "id": s("m:0"), "text": s("Let me "), "delta": .bool(true)]),
        ev(1, ["t": s("message"), "id": s("m:0"), "text": s("look."), "delta": .bool(true)]),
        ev(1, ["t": s("tool_call"), "call_id": s("k1"), "title": s("Bash cargo build"), "kind": s("execute"), "status": s("in_progress")]),
        ev(1, ["t": s("thought"), "id": s("r"), "text": s("the import is missing"), "delta": .bool(true)]),
        ev(1, ["t": s("tool_update"), "call_id": s("k1"), "status": s("failed"), "content": .array([.object(["type": s("text"), "text": s("error[E0425]")])])]),
        ev(1, ["t": s("tool_call"), "call_id": s("k2"), "title": s("Edit src/a.rs"), "kind": s("edit"), "status": s("completed"),
               "content": .array([.object(["type": s("diff"), "path": s("src/a.rs"), "old": s("a\nb\nc"), "new": s("a\nB\nc")])])]),
        ev(1, ["t": s("message"), "id": s("m2:0"), "text": s("Fixed: `b` was wrong."), "delta": .bool(false)]),
        ev(1, ["t": s("usage"), "input_tokens": .number(1200), "output_tokens": .number(80)]),
        ev(1, ["t": s("turn_end"), "status": s("completed"), "final_text": s("Fixed"), "duration_ms": .number(4200)]),
        ev(1, ["t": s("state"), "state": s("idle"), "queued": .array([])]),
    ]
}

test("a turn folds into its user message, streamed text, tools and its end") {
    let c = fold(conversation())
    check(c.state == "idle", "state")
    check(c.turns.count == 1, "one turn")
    let turn = c.turns[0]
    check(turn.userText == "fix the build", "user text")
    check(turn.status == "completed" && turn.durationMs == 4200, "end")
    check(turn.blocks.first == .text(id: "m:0", text: "Let me look."), "streamed text")
    check(tools(turn).map { [$0.callID, $0.status] } == [["k1", "failed"], ["k2", "completed"]], "tools")
    check(tools(turn)[0].content == [.text("error[E0425]")], "tool output")
}

test("a turn projects into rows: tools and edits in one run, the reply last") {
    let rows = phoneTurnRows(fold(conversation()).turns[0])
    // As in Orca, an edit is part of the run ("ran 1 command and edited 1 file") and opens to its diff
    // card; the thought between the build and the edit stays inside the run.
    check(rows.map(\.kind) == ["user", "text", "run", "text"], "kinds \(rows.map(\.kind))")
    guard case .run(_, let members, let t, let live) = rows[2] else { return check(false, "run row") }
    check(members.map { if case .tool = $0 { "tool" } else { "thought" } } == ["tool", "thought", "tool"], "members")
    check(phoneRunSummary(t) == [PhoneRunPart(cat: "command", n: 1), PhoneRunPart(cat: "change", n: 1)], "summary")
    let stat = phoneRunDiffStat(t)
    check(stat?.add == 1 && stat?.del == 1, "run diff stat")
    check(phoneRunDiffStat([t[0]]) == nil, "no edit, no stat")
    check(!live, "not live")
    check(phoneRunSentence(t, live: false) == "运行了 1 条命令 和 编辑了 1 个文件", phoneRunSentence(t, live: false))
    if case .text(_, let text, _) = rows[3] { check(text == "Fixed: `b` was wrong.", "reply") } else { check(false, "reply row") }
}

test("replayed events after a reconnect are not applied twice") {
    let events = conversation()
    let once = fold(events)
    var again = once
    for e in events.dropFirst(3) { again.apply(e) }
    check(again == once, "an overlapping replay changes nothing")
}

test("a refused command shows as a notice without moving the cursor") {
    var c = fold(conversation())
    let seqBefore = c.seq
    c.apply(.object(["seq": .number(-1), "ts": .number(0), "turn": .number(0), "t": s("notice"), "level": s("error"), "code": s("readonly"), "text": s("only watching")]))
    check(c.seq == seqBefore, "cursor")
    check(c.notices.last?.text == "only watching", "notice")
}

test("tools still running when the turn ends take the turn's outcome; questions can be answered") {
    seq = 0
    var c = fold([
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("tool_call"), "call_id": s("k"), "title": s("Bash sleep 600"), "kind": s("execute"), "status": s("in_progress")]),
        ev(1, ["t": s("question"), "qid": s("q1"), "prompt": s("Which config?"), "options": .array([s("debug"), s("release")])]),
        ev(1, ["t": s("turn_end"), "status": s("interrupted"), "duration_ms": .number(900)]),
    ])
    let tool = tools(c.turns[0])[0]
    check(tool.status == "failed" && tool.stopped, "stopped, not failed")
    check(phoneRunOutcome([tool]) == PhoneRunOutcome(failed: 0, stopped: 1, done: true), "outcome")
    c.markAnswered("q1", answer: nil)
    let answered = c.turns[0].blocks.contains { if case .question(let q) = $0 { q.answered } else { false } }
    check(answered, "answered")
}

test("the phase follows the last thing the agent does") {
    seq = 0
    var c = fold([
        ev(0, ["t": s("state"), "state": s("running"), "queued": .array([])]),
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("tool_call"), "call_id": s("k"), "title": s("Bash npm test"), "kind": s("execute"), "status": s("in_progress")]),
    ])
    check(c.phase.what == "tool", "tool")
    c.apply(ev(1, ["t": s("message"), "id": s("m"), "text": s("Done"), "delta": .bool(true)]))
    check(c.phase.what == "writing", "writing")
    c.apply(ev(1, ["t": s("state"), "state": s("interrupting"), "queued": .array([])]))
    check(c.phase.what == "stopping", "stopping")
}

test("tool calls in a row form one run; reasoning between them stays inside; a running turn's run is live") {
    seq = 0
    let c = fold([
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("tool_call"), "call_id": s("a"), "title": s("Read src/a.rs"), "kind": s("read"), "status": s("completed")]),
        ev(1, ["t": s("thought"), "id": s("r"), "text": s("now the tests"), "delta": .bool(false)]),
        ev(1, ["t": s("tool_call"), "call_id": s("b"), "title": s("Bash cargo test"), "kind": s("execute"), "status": s("completed")]),
        ev(1, ["t": s("tool_call"), "call_id": s("c"), "title": s("Bash cargo build"), "kind": s("execute"), "status": s("in_progress")]),
    ])
    let rows = phoneTurnRows(c.turns[0])
    check(rows.map(\.kind) == ["user", "run"], "kinds \(rows.map(\.kind))")
    guard case .run(_, let members, let t, let live) = rows[1] else { return check(false, "run row") }
    check(members.map { if case .tool = $0 { "tool" } else { "thought" } } == ["tool", "thought", "tool", "tool"], "members")
    check(live, "live")
    check(phoneRunSummary(t) == [PhoneRunPart(cat: "read", n: 1), PhoneRunPart(cat: "command", n: 2)], "summary")
    check(phoneRunOutcome(t) == PhoneRunOutcome(failed: 0, stopped: 0, done: false), "outcome")
    check(phoneRunSentence(t, live: true) == "正在读 1 个文件 和 运行 2 条命令", phoneRunSentence(t, live: true))
    check(phoneRunSentence(t, live: false) == "读了 1 个文件 和 运行了 2 条命令", phoneRunSentence(t, live: false))
}

test("a sub-agent (the Task tool) gets a row of its own") {
    seq = 0
    let c = fold([
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("tool_call"), "call_id": s("s"), "title": s("Task Find the date helpers"), "kind": s("think"), "status": s("in_progress")]),
    ])
    check(phoneTurnRows(c.turns[0]).map(\.kind) == ["user", "agent"], "kinds")
    let notTask = PhoneChatTool(callID: "x", title: "Tasks list", kind: "think", status: "completed", startedMs: 0)
    check(!phoneIsSubagent(notTask), "a word boundary after Task")
}

test("a message added to the running turn (steer) shows inside it, flagged") {
    seq = 0
    var c = fold([
        ev(0, ["t": s("state"), "state": s("running"), "queued": .array([])]),
        ev(0, ["t": s("user"), "input_id": s("u1"), "by": by("laptop"), "text": s("fix the build")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("tool_call"), "call_id": s("k"), "title": s("Bash cargo build"), "kind": s("execute"), "status": s("in_progress")]),
        // The host numbers a steer with the running turn.
        ev(1, ["t": s("user"), "input_id": s("u2"), "by": by("laptop"), "text": s("use the release profile"), "steer": .bool(true)]),
        ev(1, ["t": s("message"), "id": s("m"), "text": s("Switching to release."), "delta": .bool(false)]),
    ])
    check(c.turns.count == 1, "one turn")
    let rows = phoneTurnRows(c.turns[0])
    let shape: [String] = rows.map {
        if case .user(_, _, _, _, let steer, _) = $0 { return steer ? "user+steer" : "user" }
        return $0.kind
    }
    check(shape == ["user", "run", "user+steer", "text"], "\(shape)")
    if case .user(_, let text, _, _, _, _) = rows[2] { check(text == "use the release profile", "steer text") }
    // An older host might number it as the next turn: it still lands in the running one.
    c.apply(ev(2, ["t": s("user"), "input_id": s("u3"), "by": by("laptop"), "text": s("and skip docs"), "steer": .bool(true)]))
    check(c.turns.count == 1, "still one turn")
    check(c.turns[0].blocks.last == .user(text: "and skip docs", by: "laptop", ts: Double(1000 + seq * 100)), "lands in the running turn")
    // A host without steering queues it and says so.
    c.apply(ev(1, ["t": s("notice"), "level": s("info"), "code": s("steer_queued"), "text": s("queued")]))
    if case .notice(_, let code, let text) = c.turns[0].blocks.last {
        check(code == "steer_queued" && phoneNoticeText(code: code, text: text) == "智能体这一轮结束后会收到这条消息。", "steer_queued")
    } else { check(false, "notice block") }
}

test("the composer sends steer, queue or interrupt as the agent allows") {
    var steer = PhoneChatCaps(); steer.steer = true
    check(phoneSendMode(running: false, caps: steer) == nil, "idle")
    check(phoneSendMode(running: true, caps: steer) == "steer", "steer")
    check(phoneSendMode(running: true, caps: PhoneChatCaps()) == "queue", "queue")
    check(phoneSendMode(running: true, caps: nil) == "queue", "no caps")
    check(phoneSendMode(running: true, caps: steer, interrupt: true) == "interrupt", "interrupt")
    check(JSONValue.object(phoneInputCommand(id: "i1", text: "hi", mode: "steer")) == .object(["input": .object(["id": s("i1"), "text": s("hi"), "mode": s("steer")])]), "steer command")
    // Queue is the host's default: the field is left out, so older hosts read it unchanged.
    check(JSONValue.object(phoneInputCommand(id: "i2", text: "hi", mode: "queue")) == .object(["input": .object(["id": s("i2"), "text": s("hi")])]), "queue command")
    check(PhoneChatCaps(.object(["steer": .bool(true)])).steer && !PhoneChatCaps(.null).steer, "caps.steer")
}

test("editing a queued message takes the old one back and queues the new text") {
    let commands = phoneEditQueued(queuedID: "q1", newID: "q2", text: "new text").map(JSONValue.object)
    check(commands == [.object(["dequeue": .object(["id": s("q1")])]), .object(["input": .object(["id": s("q2"), "text": s("new text")])])], "\(commands)")
}

test("usage with a context window feeds the ring; usage without it leaves it alone") {
    seq = 0
    var c = fold([
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("usage"), "input_tokens": .number(10), "output_tokens": .number(2), "context_window": .number(200000), "context_used": .number(45000)]),
    ])
    check(c.context == PhoneContextUsage(used: 45000, window: 200000), "ring")
    c.apply(ev(1, ["t": s("usage"), "input_tokens": .number(12)]))
    check(c.context == PhoneContextUsage(used: 45000, window: 200000), "kept")
    check(c.turns[0].usage?.input == 12, "usage")
}

test("a stopped turn offers to continue until something else happens") {
    seq = 0
    var c = fold([
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("turn_end"), "status": s("interrupted"), "duration_ms": .number(900)]),
        ev(1, ["t": s("state"), "state": s("idle"), "queued": .array([])]),
    ])
    check(c.stoppedTurn?.n == 1, "offered")
    c.apply(ev(1, ["t": s("state"), "state": s("running"), "queued": .array([])]))
    check(c.stoppedTurn == nil, "gone")
}

test("answers are kept with their question") {
    seq = 0
    var c = fold([
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("go")]),
        ev(1, ["t": s("question"), "qid": s("q"), "prompt": s("Which?"), "options": .array([s("a"), s("b")])]),
    ])
    c.markAnswered("q", answer: "b")
    if case .question(let q) = c.turns[0].blocks[0] { check(q.answered && q.answer == "b", "answer") } else { check(false, "question") }
}

test("diffs show changes with context, fold unchanged stretches, and count lines") {
    let old = (0..<20).map { "line \($0)" }.joined(separator: "\n")
    let neu = old.replacingOccurrences(of: "line 2\n", with: "line two\n").replacingOccurrences(of: "line 17", with: "line seventeen")
    let rows = phoneDiffRows(old, neu)
    let kinds: [String] = rows.compactMap {
        switch $0 {
        case .context: nil
        case .add: "add"
        case .del: "del"
        case .gap: "gap"
        }
    }
    // Two separate changes, each with three lines around it, and the stretch between folded.
    check(kinds == ["del", "add", "gap", "del", "add"], "\(kinds)")
    check(rows.contains(.gap(8)), "gap of 8")
    // Lines before the first and after the last change are left out without a marker.
    let mid = phoneDiffRows(old, old.replacingOccurrences(of: "line 10", with: "line ten"))
    check(mid.first.map { if case .gap = $0 { false } else { true } } == true && mid.last.map { if case .gap = $0 { false } else { true } } == true, "no edge gaps")
    check(mid.count == 8, "\(mid.count) rows")
    let stat = phoneDiffStat([.diff(path: "x", old: old, new: neu)])
    check(stat?.add == 2 && stat?.del == 2, "stat")
    // A new file is all added; a change in the middle is a real line diff, not "everything replaced".
    let created = phoneDiffStat([.diff(path: "x", old: nil, new: "1\n2")])
    check(created?.add == 2 && created?.del == 0, "created")
    let middle = phoneDiffStat([.diff(path: "x", old: "a\nb\nc\nd\ne", new: "a\nB\nc\nD\ne")])
    check(middle?.add == 2 && middle?.del == 2, "middle")
    check(phoneDiffStat([.text("x")]) == nil, "no diff")
}

test("long output shows its tail; tool labels split verb from target") {
    let out = (0..<30).map { "l\($0)" }.joined(separator: "\n")
    let tail = phoneTailLines(out, 14)
    check(tail.hidden == 16 && tail.text.hasPrefix("l16\n"), "tail")
    let x = PhoneChatTool(callID: "k", title: "Bash npm test -- --watch", kind: "execute", status: "completed",
                          input: .object(["command": s("npm test -- --watch")]), startedMs: 0, endedMs: 1)
    check(phoneToolLabel(x) == ("Bash", "npm test -- --watch"), "label")
    check(phoneToolCommand(x) == "npm test -- --watch", "command")
    check(phoneBaseName("D:\\work\\export.py") == "export.py", "base name")
}

test("phone wording: durations and a session waiting for its first message") {
    check(phoneDuration(850) == "850 毫秒" && phoneDuration(4200) == "4.2 秒" && phoneDuration(38_000) == "38 秒" && phoneDuration(125_000) == "2:05", "durations")
    check(phoneChatStateName("parked", turns: 0) == "等待第一条消息" && phoneChatStateName("parked", turns: 2) == "已暂停", "parked")
}

// MARK: - The composer, folders and previews (tools/chat-input.test.mjs, chat-folder.test.mjs, chat-preview.test.mjs)

func out(_ id: String, _ state: String = "sending") -> PhoneOutgoing {
    var o = PhoneOutgoing(id: id, text: id, mode: nil, at: 0); o.state = state; return o
}

test("a message leaves the list once the conversation shows it, as a user event or queued") {
    seq = 0
    let c = fold([
        ev(0, ["t": s("user"), "input_id": s("a"), "by": by("d"), "text": s("a")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("state"), "state": s("running"), "queued": .array([.object(["id": s("b"), "text": s("b")])])]),
    ])
    let left = phoneSettleOutgoing([out("a", "acked"), out("b", "acked"), out("c", "acked")], log: c, now: 10, hostAcks: true)
    check(left.map(\.id) == ["c"], "\(left.map(\.id))")
}

test("acks, refusals, timeouts and a dropped connection") {
    var list = [out("a"), out("b"), out("c")]
    list = phoneOutgoingReply(list, .object(["t": s("ack"), "input_id": s("a")]))
    list = phoneOutgoingReply(list, .object(["t": s("error"), "code": s("refused"), "input_id": s("b"), "text": s("empty message")]))
    check(list.map { "\($0.id):\($0.state):\($0.refused)" } == ["a:acked:false", "b:failed:true", "c:sending:false"], "replies")
    check(phoneSettleOutgoing(list, log: PhoneChatLog(), now: phoneSendTimeoutMs + 1, hostAcks: false) == list, "no timeout without acks")
    let late = phoneSettleOutgoing(list, log: PhoneChatLog(), now: phoneSendTimeoutMs + 1, hostAcks: true)
    check(late.map { "\($0.id):\($0.state):\($0.error ?? "-")" } == ["a:acked:-", "b:failed:empty message", "c:failed:timeout"], "\(late)")
    let gone = phoneOutgoingDisconnected([out("x"), out("y", "acked")])
    check(gone.map(\.state) == ["failed", "acked"], "disconnected")
}

test("a message the full queue dropped fails; a retry keeps its id unless the host refused it") {
    seq = 0
    let c = fold([ev(1, ["t": s("notice"), "level": s("warn"), "code": s("queue_full"), "text": s("8 messages are already waiting"), "input_id": s("q")])])
    let q = phoneSettleOutgoing([out("q", "acked")], log: c, now: 1, hostAcks: true)[0]
    check(q.state == "failed" && q.refused, "dropped")
    check(phoneRetryOutgoing(q, newID: "new", now: 5).id == "new", "new id after a refusal")
    var lost = out("l"); lost.state = "failed"; lost.error = "disconnected"
    let again = phoneRetryOutgoing(lost, newID: "new", now: 5)
    check(again.id == "l" && again.state == "sending" && again.at == 5, "same id after a lost connection")
}

test("lines are told apart") {
    check(phoneChatLineKind(.object(["t": s("commands"), "commands": .array([])])) == "commands", "commands")
    check(phoneChatLineKind(.object(["t": s("ack"), "input_id": s("a")])) == "reply", "ack")
    check(phoneChatLineKind(.object(["t": s("files"), "rid": s("r1"), "files": .array([])])) == "reply", "files")
    check(phoneChatLineKind(.object(["t": s("error"), "input_id": s("a")])) == "reply", "refusal")
    check(phoneChatLineKind(.object(["t": s("error"), "text": s("x")])) == "notice", "error")
    check(phoneChatLineKind(.object(["t": s("message"), "seq": .number(3)])) == "event", "event")
    check(phoneChatLineKind(.object(["t": s("bye")])) == "closed", "closed")
}

test("the caret's word opens the / menu at the start of the message and @ anywhere") {
    check(phoneAutocompleteAt("/rev", caret: 4) == PhoneAutocomplete(mode: "slash", query: "rev", start: 0, end: 4), "slash")
    check(phoneAutocompleteAt("  /", caret: 3) == PhoneAutocomplete(mode: "slash", query: "", start: 2, end: 3), "slash after spaces")
    check(phoneAutocompleteAt("see /rev", caret: 8) == nil, "not mid-message")
    check(phoneAutocompleteAt("/usr/bin", caret: 8) == nil, "not a path")
    check(phoneAutocompleteAt("look at @src/ma and", caret: 15) == PhoneAutocomplete(mode: "mention", query: "src/ma", start: 8, end: 15), "mention")
    check(phoneAutocompleteAt("mail a@b", caret: 8) == nil, "not an address")
    check(phoneAutocompleteAt("plain text", caret: 5) == nil, "plain")
    // UTF-16 offsets, as UITextView counts them.
    check(phoneAutocompleteAt("看 @文", caret: 4)?.query == "文", "CJK offsets")
}

test("commands match by name first, then by description; choosing replaces the word") {
    let cmds = [PhoneSlashCommand(name: "compact", description: "Clear history but keep a summary"),
                PhoneSlashCommand(name: "review", description: "Review the changes"),
                PhoneSlashCommand(name: "pr-comments", description: "Get the review comments"),
                PhoneSlashCommand(name: "tidy", description: "a skill", kind: "skill", insert: "$tidy")]
    check(phoneMatchCommands(cmds, "re").map(\.name) == ["review", "pr-comments"], "\(phoneMatchCommands(cmds, "re").map(\.name))")
    check(phoneMatchCommands(cmds, "").count == 4 && phoneMatchCommands(cmds, "zzz").isEmpty, "empty and none")
    let ac = phoneAutocompleteAt("/re more", caret: 3)!
    check(phoneApplyChoice("/re more", ac, insert: "/review") == ("/review more", 8), "choose a command")
    let at = phoneAutocompleteAt("fix @ma", caret: 7)!
    check(phoneApplyChoice("fix @ma", at, insert: phoneMentionText("src/main.rs")) == ("fix @src/main.rs ", 17), "choose a file")
    check(phoneMentionText("docs/a b.md") == "@\"docs/a b.md\"", "quoted")
    check(phoneRankFiles(["src/lib/main.rs", "docs/domain.md", "main.rs", "tests/x_main.rs"], "main")
          == ["main.rs", "src/lib/main.rs", "docs/domain.md", "tests/x_main.rs"], "file ranking")
}

test("the agent's command list folds into the chat") {
    seq = 0
    var c = fold([ev(0, ["t": s("commands"), "commands": .array([.object(["name": s("review"), "kind": s("command"), "insert": s("/review")])])])])
    check(c.commands?.first?.insert == "/review" && c.turns.isEmpty, "numbered")
    check(PhoneChatLog().commands == nil, "none yet")
    var opened = PhoneChatLog()
    opened.knownCommands([.object(["name": s("a")])])
    check(opened.commands?.first?.insert == "/a", "unnumbered fills in, insert defaults to /name")
    c.knownCommands([])
    check(c.commands?.first?.insert == "/review", "an event wins over the known list")
}

test("images are checked against the host's limits; an input carries them; the conversation keeps their names") {
    check(phoneImageProblem(mime: "image/png", bytes: 1000, have: 0) == nil, "fine")
    check(phoneImageProblem(mime: "image/svg+xml", bytes: 1000, have: 0) == "type", "type")
    check(phoneImageProblem(mime: "image/jpeg", bytes: phoneMaxImageBytes + 1, have: 0) == "size", "size")
    check(phoneImageProblem(mime: "image/png", bytes: 10, have: phoneMaxImages) == "count", "count")
    check(phoneFitWithin(width: 4000, height: 1000) == (2048, 512, true) && phoneFitWithin(width: 800, height: 600) == (800, 600, false), "fit")
    let img = PhoneImage(name: "a.png", mime: "image/png", data: Data([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))
    check(phoneInputCommand(id: "u1", text: "", mode: nil, images: [img])["input"]?["attachments"].array.first?["data"].string == "iVBORw0KGgo=", "payload")
    check(phoneInputCommand(id: "u1", text: "hi", mode: "queue", images: [])["input"]?["attachments"] == .null, "no images, no field")
    seq = 0
    let c = fold([
        ev(0, ["t": s("user"), "input_id": s("u1"), "by": by("d"), "text": s(""), "attachments": .array([.object(["name": s("a.png"), "mime": s("image/png"), "bytes": .number(8)])])]),
        ev(1, ["t": s("turn_start")]),
        ev(0, ["t": s("agent"), "images": .bool(false)]),
    ])
    if case .user(_, _, _, _, _, let extra)? = phoneTurnRows(c.turns[0]).first {
        check(extra.inputID == "u1" && extra.attachments.first?.name == "a.png" && extra.attachments.first?.bytes == 8, "row extra")
    } else { check(false, "user row") }
    check(c.images == false && c.inputs == ["u1"], "agent images, inputs")
}

test("what is a path and what is a search; trailing separators go, roots stay") {
    for p in ["/", "/home/a", "~", "~/code", "~\\code", "C:", "c:\\", "D:\\work\\x", "D:/work", "\\\\srv\\share"] { check(phoneLooksLikePath(p), p) }
    for p in ["app", "code/app", "~code", ""] { check(!phoneLooksLikePath(p), p) }
    check(phoneTrimFolder("/home/a/") == "/home/a" && phoneTrimFolder("/") == "/" && phoneTrimFolder("D:\\work\\") == "D:\\work", "trim 1")
    check(phoneTrimFolder("D:\\") == "D:\\" && phoneTrimFolder("D:") == "D:\\" && phoneTrimFolder("D:/") == "D:/" && phoneTrimFolder(" ~/code/ ") == "~/code", "trim 2")
}

test("names, parents, children and crumbs on both kinds of path") {
    check(phoneFolderName("D:\\work\\app") == "app" && phoneFolderName("/Users/a/code/app/") == "app" && phoneFolderName("C:\\") == "C:", "names 1")
    check(phoneFolderName("/") == "/" && phoneFolderName("~") == "~" && phoneFolderName("\\\\srv\\share") == "share", "names 2")
    check(phoneParentFolder("D:\\work\\app") == "D:\\work" && phoneParentFolder("D:\\work") == "D:\\" && phoneParentFolder("D:\\") == nil, "parents 1")
    check(phoneParentFolder("/a") == "/" && phoneParentFolder("~/code") == "~" && phoneParentFolder("\\\\srv\\share\\x") == "\\\\srv\\share\\", "parents 2")
    check(phoneChildFolder("D:\\", "work") == "D:\\work" && phoneChildFolder("D:\\work", "app") == "D:\\work\\app" && phoneChildFolder("/", "tmp") == "/tmp", "children 1")
    check(phoneChildFolder("~", "code") == "~/code" && phoneChildFolder("C:/Users", "a") == "C:/Users/a", "children 2")
    check(phoneFolderCrumbs("D:\\work\\app").map { "\($0.name)=\($0.path)" } == ["D:=D:\\", "work=D:\\work", "app=D:\\work\\app"], "crumbs win")
    check(phoneFolderCrumbs("/home/a").map { "\($0.name)=\($0.path)" } == ["/=/", "home=/home", "a=/home/a"], "crumbs unix")
}

test("the same folder, the home shortened to ~, choices, matching and listings") {
    check(phoneSameFolder("D:\\Work\\", "d:/work") && !phoneSameFolder("/home/A", "/home/a"), "same")
    check(phoneTildeFolder("/Users/a/code/app", home: "/Users/a") == "~/code/app" && phoneTildeFolder("/Users/a", home: "/Users/a/") == "~", "tilde 1")
    check(phoneTildeFolder("/Users/ab/x", home: "/Users/a") == "/Users/ab/x" && phoneTildeFolder("C:\\Users\\A\\code", home: "c:\\users\\a") == "~\\code", "tilde 2")
    check(phoneTildeFolder("/tmp", home: nil) == "/tmp", "no home")
    check(phoneFolderChoices(recent: ["/a/x/", "/b"], sessions: ["/a/x", "D:\\W", "d:\\w\\"]) == ["/a/x", "/b", "D:\\W"], "choices")
    check(phoneMatchFolders(["/code/myapp", "/code/app", "/srv/other"], "app") == ["/code/app", "/code/myapp"], "match")
    check(phoneMatchFolders(["/a", "/b"], " ") == ["/a", "/b"], "blank")
    let entries: [JSONValue] = [.object(["name": s(".git"), "kind": s("dir")]), .object(["name": s("src"), "kind": s("dir")]),
                                .object(["name": s("tmp"), "kind": s("link")]), .object(["name": s("a.txt"), "kind": s("file")])]
    check(phoneListedFolders(entries) == ["src", "tmp"] && phoneListedFolders(entries, query: ".g") == [".git"] && phoneListedFolders(entries, query: "SR") == ["src"], "listed")
}

test("sessions group by folder, latest first; how long ago, in one unit") {
    func sess(_ id: String, _ cwd: String, _ at: Double) -> PhoneChatSession {
        PhoneChatSession(.object(["id": s(id), "cwd": s(cwd), "last_activity": .number(at)]))
    }
    let g = phoneSessionsByFolder([sess("a", "/w/x", 1), sess("b", "/w/y", 5), sess("c", "/w/x/", 9), sess("d", "D:\\P", 2), sess("e", "d:\\p", 3)])
    check(g.map { "\($0.folder):\($0.sessions.map(\.id).joined())" } == ["/w/x:ca", "/w/y:b", "d:\\p:ed"], "\(g.map(\.folder))")
    let now = 10.0 * 86_400_000
    check(phoneAgo(now - 30_000, now: now) == "刚刚" && phoneAgo(now - 5 * 60_000, now: now) == "5 分钟前", "ago 1")
    check(phoneAgo(now - 3 * 3_600_000, now: now) == "3 小时前" && phoneAgo(now - 2 * 86_400_000, now: now) == "2 天前" && phoneAgo(now + 5000, now: now) == "刚刚", "ago 2")
}

test("previews: kinds by extension, files named in a reply, the HTML document's policy") {
    check(phonePreviewKind("/tmp/out/Report.HTML") == "html" && phonePreviewKind("C:\\work\\notes.md") == "markdown" && phonePreviewKind("chart.svg") == "image", "kinds")
    check(phonePreviewKind("a.txt") == nil && phonePreviewKind(".md") == nil && phonePreviewKind("Makefile") == nil, "not previewable")
    check(phoneImageMime("x.SVG") == "image/svg+xml" && phoneImageMime("x.jpg") == "image/jpeg", "mime")
    let yes: [(String, String)] = [("out/report.html", "out/report.html"), ("./notes.md", "./notes.md"), ("C:\\work\\chart.png", "C:\\work\\chart.png"),
        ("C:/work/chart.png", "C:/work/chart.png"), ("file:///tmp/a%20b.svg", "/tmp/a b.svg"), ("file:///C:/x/r.html", "C:/x/r.html"),
        ("report.html#top", "report.html"), ("docs/guide.md:12", "docs/guide.md")]
    for (input, want) in yes { check(phoneFileTarget(input) == want, "\(input) → \(phoneFileTarget(input) ?? "nil")") }
    for input in ["https://example.com/a.html", "mailto:a@b.md", "javascript:alert(1)//.html", "npm run build", "src/main.rs", "out/"] {
        check(phoneFileTarget(input) == nil, input)
    }
    let withHead = phonePreviewDocument("<!doctype html><html><head><title>x</title><script>1</script></head><body>hi</body></html>")
    check(withHead.hasPrefix("<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none';"), "after head")
    check(withHead.range(of: "Content-Security-Policy")!.lowerBound < withHead.range(of: "<script>")!.lowerBound, "before the script")
    check(phonePreviewDocument("<!DOCTYPE html>\n<p>hi</p>").hasPrefix("<!DOCTYPE html><meta http-equiv"), "after the doctype")
    check(phonePreviewDocument("<p>hi</p>").hasPrefix("<meta http-equiv"), "at the start")
    check(phoneSizeLabel(512) == "512 B" && phoneSizeLabel(12 * 1024) == "12 KB" && phoneSizeLabel(Int(3.4 * 1_048_576)) == "3.4 MB", "sizes")
}

test("a turn's files: what its tools wrote, then what its reply names, one per name") {
    func tool(_ kind: String = "edit", status: String = "completed", locations: [String] = [], content: [PhoneToolContent] = []) -> PhoneChatBlock {
        .tool(PhoneChatTool(callID: "c", title: "", kind: kind, status: status, locations: locations, content: content, startedMs: 0, endedMs: 0))
    }
    let blocks: [PhoneChatBlock] = [
        tool(content: [.diff(path: "/w/out/report.html", old: nil, new: "<p>")]),
        tool("read", locations: ["/w/in/source.md"]),
        tool(status: "failed", content: [.diff(path: "/w/broken.md", old: nil, new: "x")]),
        tool(content: [.diff(path: "/w/gone.png", old: "x", new: "")]),
        tool("execute", locations: ["/w/out/chart.svg", "/w/main.rs"]),
        .text(id: "m", text: "Wrote [the page](out/report.html), `out/notes.md` and `README.txt`; see https://x.dev/a.md"),
    ]
    check(phoneTurnFiles(blocks) == ["/w/out/report.html", "/w/out/chart.svg", "out/notes.md"], "\(phoneTurnFiles(blocks))")
    let many = (0..<6).map { tool(content: [.diff(path: "/w/p\($0).md", old: nil, new: "x")]) }
    check(phoneTurnFiles(many).count == phoneMaxInlinePreviews, "at most four")
}

print(failures == 0 ? "all passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
