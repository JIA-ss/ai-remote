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
        if case .user(_, _, _, _, let steer) = $0 { return steer ? "user+steer" : "user" }
        return $0.kind
    }
    check(shape == ["user", "run", "user+steer", "text"], "\(shape)")
    if case .user(_, let text, _, _, _) = rows[2] { check(text == "use the release profile", "steer text") }
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

print(failures == 0 ? "all passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
