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


// MARK: - The Codex view (apps/ios/Sources/PhoneCodexModel.swift), with the desktop's tools/chat-codex.test.mjs cases.

test("codex: the shell Codex wraps a command in is left out") {
    check(phoneUnwrapShell("/bin/zsh -lc 'ls -la'") == "ls -la", "zsh -lc")
    check(phoneUnwrapShell("bash -lc \"rg -n \\\"a b\\\" src\"") == "rg -n \"a b\" src", phoneUnwrapShell("bash -lc \"rg -n \\\"a b\\\" src\""))
    check(phoneUnwrapShell("/bin/bash -l -c 'npm test'") == "npm test", "bash -l -c")
    let ps = "\"C:\\\\Windows\\\\System32\\\\WindowsPowerShell\\\\v1.0\\\\powershell.exe\" -Command Get-ChildItem src"
    check(phoneUnwrapShell(ps) == "Get-ChildItem src", phoneUnwrapShell(ps))
    check(phoneUnwrapShell("cmd.exe /c dir") == "dir", "cmd /c")
    check(phoneUnwrapShell("git status") == "git status", "no shell")
    check(phoneUnwrapShell("bash run.sh x") == "bash run.sh x", "a script, not -c")
    check(phoneSplitWords("a 'b c' \"d \\\"e\\\"\" f\\ g") == ["a", "b c", "d \"e\"", "f g"], "\(phoneSplitWords("a 'b c' \"d \\\"e\\\"\" f\\ g"))")
}

test("codex: commands read as reads, searches and listings where they are only that") {
    check(phoneParseCommand("sed -n '1,120p' src/app.ts") == [.read(path: "src/app.ts", name: "app.ts")], "sed -n")
    check(phoneParseCommand("cat a.txt b/c.md") == [.read(path: "a.txt", name: "a.txt"), .read(path: "b/c.md", name: "c.md")], "cat two")
    check(phoneParseCommand("nl -ba src/x.rs | sed -n '1,40p'") == [.read(path: "src/x.rs", name: "x.rs")], "nl | sed")
    check(phoneParseCommand("head -n 50 README.md") == [.read(path: "README.md", name: "README.md")], "head -n")
    check(phoneParseCommand("rg -n \"quarter_end\" src -g \"*.py\"") == [.search(query: "quarter_end", path: "src")], "rg with a glob")
    check(phoneParseCommand("grep -rn TODO .") == [.search(query: "TODO", path: ".")], "grep")
    check(phoneParseCommand("rg --files src | head -50") == [.listFiles(path: "src")], "rg --files")
    check(phoneParseCommand("ls -la apps") == [.listFiles(path: "apps")], "ls")
    check(phoneParseCommand("find . -name '*.toml'") == [.search(query: "*.toml", path: ".")], "find -name")
    check(phoneParseCommand("rg -n -e foo src") == [.search(query: "foo", path: "src")], "rg -e")
    // Echoes between reads are not commands of their own.
    let two = phoneParseCommand("sed -n '1,20p' a.rs && echo --- && sed -n '1,20p' b.rs")
    check(two == [.read(path: "a.rs", name: "a.rs"), .read(path: "b.rs", name: "b.rs")], "\(two)")
    // Anything else makes it a command.
    check(phoneParseCommand("cargo test -p x") == [.unknown], "cargo")
    check(phoneParseCommand("cat a.txt && npm run build") == [.unknown], "a read and a build")
    check(phoneParseCommand("sed -i 's/a/b/' x") == [.unknown], "sed -i")
}

func codexTool(_ callID: String, _ title: String, kind: String = "execute", status: String = "completed", input: JSONValue? = nil,
               content: [PhoneToolContent] = [], started: Double = 1000, ended: Double? = 3400, exitCode: Int? = nil, durationMs: Double? = nil) -> PhoneChatTool {
    PhoneChatTool(callID: callID, title: title, kind: kind, status: status, input: input, content: content, startedMs: started, endedMs: ended,
                  exitCode: exitCode, durationMs: durationMs)
}

test("codex: a command's info comes from the host, or from its title and output on older hosts") {
    let fresh = phoneExecInfo(codexTool("c", "Shell rg x", input: .object(["command": s("rg x"), "cwd": s("/w"),
        "actions": .array([.object(["type": s("search"), "command": s("rg x"), "query": s("x"), "path": .null])])]),
        content: [.text("a.rs:1:x\n")], exitCode: 0, durationMs: 1234))
    check(fresh.command == "rg x" && fresh.cwd == "/w", "command and cwd")
    check(fresh.explore, "a search is exploring")
    check(fresh.actions == [.search(query: "x", path: nil)], "\(fresh.actions)")
    check(fresh.durationMs == 1234 && fresh.exitCode == 0, "time and code")
    let streamed = phoneExecInfo(codexTool("c", "Shell seq 3", content: [.text("1\n2"), .text("\n3\n")]))
    check(streamed.output == "1\n2\n3\n", "streamed pieces join as they came: \(streamed.output.debugDescription)")
    check(streamed.exitCode == 0, "a completed command without a code succeeded")
    let old = phoneExecInfo(codexTool("c", "/bin/zsh -lc 'cargo build'", status: "failed", content: [.text("error\n"), .text("(exit code 101)")]))
    check(old.command == "cargo build", old.command)
    check(!old.explore, "a build is a command")
    check(old.exitCode == 101, "the exit line's code")
    check(old.output.trimmingCharacters(in: .whitespacesAndNewlines) == "error", "without the exit line: \(old.output.debugDescription)")
    check(old.durationMs == 2400, "from the tool's start and end")
    var stopped = codexTool("c", "Shell sleep 9", status: "failed")
    stopped.stopped = true
    check(phoneExecInfo(stopped).exitCode == nil, "a stopped command has no code")
}

test("codex: a unified diff keeps its line numbers; an added file's content is all added") {
    let d = phoneParseUnifiedDiff("@@ -10,3 +10,4 @@ fn x\n a\n-b\n+B\n+C\n c\n")
    check(d.add == 2 && d.del == 1, "counts")
    let got = d.lines.map { "\($0.kind.rawValue) \($0.old.map(String.init) ?? "-") \($0.new.map(String.init) ?? "-")" }
    check(got == ["hunk - -", "context 10 10", "del 11 -", "add - 11", "add - 12", "context 12 13"], "\(got)")
    check(d.lines[0].text == "@@ -10,3 +10,4 @@ fn x" && d.lines[2].text == "b", "texts")
    let added = phoneParseUnifiedDiff("one\ntwo\n", kind: "add")
    check(added.add == 2 && added.lines[1].new == 2 && added.lines[1].kind == .add, "added content")
    let deleted = phoneParseUnifiedDiff("one\n", kind: "delete")
    check(deleted.del == 1 && deleted.lines[0].old == 1, "deleted content")
    check(phoneParseUnifiedDiff("").lines.isEmpty, "empty")
    let crlf = phoneParseUnifiedDiff("@@ -1 +1 @@\r\n-a\r\n+b\r\n\\ No newline at end of file\r\n")
    check(crlf.lines.map(\.text) == ["@@ -1 +1 @@", "a", "b"], "CRLF and the no-newline marker: \(crlf.lines.map(\.text))")
}

test("codex: an edit's files come from the host's list, or are merged per path from its hunks") {
    let fresh = phoneFileChanges(codexTool("f", "edit a", kind: "edit", input: .object(["files": .array([
        .object(["path": s("/w/a.ts"), "kind": s("update"), "diff": s("@@ -1 +1 @@\n-x\n+y\n")]),
        .object(["path": s("/w/n.ts"), "kind": s("add"), "diff": s("hi\n")]),
        .object(["path": s("/w/o.ts"), "kind": s("update"), "move_path": s("/w/p.ts"), "diff": s("")])])])))
    check(fresh.map { "\($0.path) \($0.kind) \($0.add) \($0.del)" } == ["/w/a.ts update 1 1", "/w/n.ts add 1 0", "/w/o.ts update 0 0"], "\(fresh.map(\.path))")
    check(fresh[2].movePath == "/w/p.ts", "a move")
    let old = phoneFileChanges(codexTool("f", "edit a", kind: "edit", content: [
        .diff(path: "a.ts", old: "1\n2\n", new: "1\nX\n"), .diff(path: "a.ts", old: "9\n", new: "9\n10\n"), .diff(path: "b.ts", old: nil, new: "z\n")]))
    check(old.map { "\($0.path) \($0.kind) \($0.add) \($0.del)" } == ["a.ts update 2 1", "b.ts add 1 0"], "\(old.map { "\($0.path) \($0.add) \($0.del)" })")
    check(old[0].lines.contains { $0.kind == .hunk }, "the two hunks stay apart")
    var loc = codexTool("f", "edit a", kind: "edit")
    loc.locations = ["/w/x.md"]
    check(phoneFileChanges(loc).map(\.path) == ["/w/x.md"], "only where it edited")
}

func codexEvents() -> [JSONValue] {
    seq = 0
    func shell(_ id: String, _ command: String, _ actions: [JSONValue]) -> JSONValue {
        ev(1, ["t": s("tool_call"), "call_id": s(id), "title": s("Shell " + command), "kind": s("execute"), "status": s("in_progress"),
               "input": .object(["command": s(command), "cwd": s("/w"), "actions": .array(actions)])])
    }
    return [
        ev(0, ["t": s("state"), "state": s("running"), "queued": .array([])]),
        ev(0, ["t": s("user"), "input_id": s("u1"), "by": by("mac"), "text": s("fix the export")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("thought"), "id": s("r1"), "text": s("**Looking at the export**"), "delta": .bool(true)]),
        ev(1, ["t": s("message"), "id": s("m1"), "text": s("I'll look at the export first."), "delta": .bool(true), "phase": s("commentary")]),
        shell("c1", "rg -n quarter_end", [.object(["type": s("search"), "query": s("quarter_end"), "path": .null])]),
        ev(1, ["t": s("tool_update"), "call_id": s("c1"), "status": s("completed"), "content": .array([.object(["type": s("text"), "text": s("export.py:1")])]),
               "exit_code": .number(0), "duration_ms": .number(80)]),
        shell("c2", "sed -n '1,40p' export.py", [.object(["type": s("read"), "name": s("export.py"), "path": s("/w/export.py")])]),
        ev(1, ["t": s("tool_update"), "call_id": s("c2"), "status": s("completed"), "exit_code": .number(0)]),
        ev(1, ["t": s("thought"), "id": s("r2"), "text": s("day 31 is wrong"), "delta": .bool(true)]),
        ev(1, ["t": s("tool_call"), "call_id": s("f1"), "title": s("edit /w/export.py"), "kind": s("edit"), "status": s("in_progress"),
               "input": .object(["files": .array([.object(["path": s("/w/export.py"), "kind": s("update"),
                                                            "diff": s("@@ -1,2 +1,2 @@\n def q():\n-    return 31\n+    return last_day()\n")])])]),
               "content": .array([.object(["type": s("diff"), "path": s("/w/export.py"), "old": s(" def q():\n    return 31\n"), "new": s(" def q():\n    return last_day()\n")])])]),
        ev(1, ["t": s("tool_update"), "call_id": s("f1"), "status": s("completed")]),
        shell("c3", "pytest -q", [.object(["type": s("unknown")])]),
    ]
}

func groupActs(_ u: PhoneCodexUnit) -> [PhoneCodexAct] { if case .group(_, let acts, _) = u { acts } else { [] } }
func groupLive(_ u: PhoneCodexUnit) -> Bool { if case .group(_, _, let live) = u { live } else { false } }
func actKind(_ a: PhoneCodexAct) -> String {
    switch a {
    case .exec: "exec"
    case .patch: "patch"
    case .web: "web"
    case .tool: "tool"
    case .thought: "thought"
    }
}

test("codex: a running turn has one group for its tool calls, the remark before it, nothing folded yet") {
    let v = phoneCodexTurn(fold(codexEvents()).turns[0])
    check(v.units.map(\.kind) == ["thought", "message", "group"], "\(v.units.map(\.kind))")
    let g = v.units[2]
    check(groupLive(g), "the group is live")
    check(groupActs(g).map(actKind) == ["exec", "exec", "thought", "patch", "exec"], "\(groupActs(g).map(actKind))")
    check(phoneActiveAct(groupActs(g))?.tool?.callID == "c3", "the running call is named")
    check(v.answer == nil && !v.foldable && v.status == "working", "no answer, not folded, working")
    check(!v.thinking, "the live group says it is working")
    check(v.changes.isEmpty, "changes wait for the end")
    let sum = phoneGroupSummary(groupActs(g))
    check(sum.parts == [PhoneSummaryPart(part: "edited", n: 1), PhoneSummaryPart(part: "read", n: 1), PhoneSummaryPart(part: "ran", n: 1)], "\(sum.parts)")
    check(sum.add == 1 && sum.del == 1, "the group's lines")
    check(phoneCodexSummarySentence(groupActs(g), lang: .en) == "Edited a file, read files, and ran a command", phoneCodexSummarySentence(groupActs(g), lang: .en))
    check(phoneCodexSummarySentence(groupActs(g), lang: .zh) == "编辑了文件、读取了文件和运行了命令", phoneCodexSummarySentence(groupActs(g), lang: .zh))
    // Before the last call starts, the group is still the live line (the agent is between calls).
    let between = phoneCodexTurn(fold(Array(codexEvents().dropLast())).turns[0])
    check(groupLive(between.units[2]) && phoneActiveAct(groupActs(between.units[2]))?.tool?.callID == "f1", "between calls")
}

test("codex: a plan update neither shows in the work nor splits a group") {
    var events = codexEvents()
    events.insert(ev(1, ["t": s("plan"), "entries": .array([.object(["content": s("Fix"), "status": s("in_progress")]),
                                                             .object(["content": s("Test"), "status": s("pending")])])]), at: 9)
    // Numbered in the order they now come (the inserted event was numbered last).
    events = events.enumerated().map { i, e in
        guard case .object(var o) = e else { return e }
        o["seq"] = .number(Double(i + 1))
        return .object(o)
    }
    let v = phoneCodexTurn(fold(events).turns[0])
    check(v.units.map(\.kind) == ["thought", "message", "group"], "\(v.units.map(\.kind))")
    check(groupActs(v.units[2]).map(actKind) == ["exec", "exec", "thought", "patch", "exec"], "one group: \(groupActs(v.units[2]).map(actKind))")
    check(v.plan?.map(\.content) == ["Fix", "Test"], "the plan is the turn's")
    let st = phonePlanStep(v.plan ?? [])
    check(st.step == 1 && st.total == 2 && st.done == 0 && st.current?.content == "Fix", "its step")
}

test("codex: the answer folds the work away; the turn's changes close it") {
    let events = codexEvents() + [
        ev(1, ["t": s("tool_update"), "call_id": s("c3"), "status": s("completed"), "content": .array([.object(["type": s("text"), "text": s("3 passed")])]),
               "exit_code": .number(0), "duration_ms": .number(2300)]),
        ev(1, ["t": s("message"), "id": s("m2"), "text": s("Fixed: "), "delta": .bool(true), "phase": s("final_answer")]),
    ]
    let streaming = phoneCodexTurn(fold(events).turns[0])
    check(streaming.answer?.id == "m2" && streaming.answer?.streaming == true, "the answer streams")
    check(streaming.foldable, "the work folds once the answer starts")
    check(!groupLive(streaming.units.last!), "the work is done once the answer runs")
    check(!streaming.thinking, "the streaming answer shows work")
    let done = fold(events + [ev(1, ["t": s("message"), "id": s("m2"), "text": s("the last day."), "delta": .bool(true)]),
                              ev(1, ["t": s("turn_end"), "status": s("completed"), "final_text": s("Fixed"), "duration_ms": .number(83_000)])])
    let v = phoneCodexTurn(done.turns[0])
    check(v.status == "worked", v.status)
    check(v.answer?.text == "Fixed: the last day." && v.answer?.streaming == false, "the whole answer")
    check(v.changes.map { "\($0.path) \($0.add) \($0.del)" } == ["/w/export.py 1 1"], "\(v.changes.map(\.path))")
    check(v.changes[0].lines.map(\.kind) == [.hunk, .context, .del, .add], "the change's diff, from the host's list")
    check(phoneWorkedMs(done.turns[0], now: 0) == 83_000, "worked for the turn's duration")
    check(phoneCodexDuration(83_000) == "1m 23s" && phoneCodexDuration(83_000, zh: true) == "1 分 23 秒", "durations")
    check(phoneCodexDuration(8_400) == "8s" && phoneCodexDuration(7_500_000) == "2h 5m", "short and long")
    check(phoneCodexElapsed(900) == nil && phoneCodexElapsed(2_410) == "2.4s" && phoneCodexElapsed(2_410, zh: true) == "2.4 秒", "a command's time")
    check(phoneCodexText("cdx_worked_for", ["t": phoneCodexDuration(83_000)], lang: .en) == "Worked for 1m 23s", "en words")
    check(phoneCodexText("cdx_worked_for", ["t": phoneCodexDuration(83_000, zh: true)], lang: .zh) == "工作了 1 分 23 秒", "zh words")
    check(phoneCodexPlural("cdx_files_changed", 1, lang: .en) == "1 file changed" && phoneCodexPlural("cdx_files_changed", 2, lang: .en) == "2 files changed", "plurals")
    let patch = phoneFilePatch(v.changes[0], cwd: "/w")
    check(patch == "diff --git a/export.py b/export.py\n--- a/export.py\n+++ b/export.py\n@@ -1,2 +1,2 @@\n def q():\n-    return 31\n+    return last_day()\n",
          patch ?? "nil")
}

test("codex: a stopped turn does not fold; a commentary is never the answer") {
    let stopped = fold(codexEvents() + [ev(1, ["t": s("message"), "id": s("m2"), "text": s("Partial"), "delta": .bool(true), "phase": s("final_answer")]),
                                        ev(1, ["t": s("turn_end"), "status": s("interrupted"), "duration_ms": .number(5000)])])
    let v = phoneCodexTurn(stopped.turns[0])
    check(v.status == "stopped" && !v.foldable, "stopped, not folded")
    check(v.changes.map(\.path) == ["/w/export.py"], "what it changed before the stop")
    let remark = fold(Array(codexEvents().prefix(5)) + [ev(1, ["t": s("turn_end"), "status": s("completed"), "duration_ms": .number(1000)])])
    check(phoneCodexTurn(remark.turns[0]).answer == nil, "a commentary is not an answer")
    let failed = fold(Array(codexEvents().prefix(5)) + [ev(1, ["t": s("turn_end"), "status": s("failed"), "reason": s("overloaded")])])
    check(phoneCodexTurn(failed.turns[0]).status == "failed", "failed")
}

test("codex: a host that does not send phases: the last text of a finished turn is its answer") {
    seq = 0
    let events = [
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("hi")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("message"), "id": s("a"), "text": s("Checking."), "delta": .bool(false)]),
        ev(1, ["t": s("tool_call"), "call_id": s("c"), "title": s("/bin/zsh -lc ls"), "kind": s("execute"), "status": s("completed")]),
        ev(1, ["t": s("message"), "id": s("b"), "text": s("Done."), "delta": .bool(false)]),
    ]
    check(phoneCodexTurn(fold(events).turns[0]).answer == nil, "still running: it may be a remark")
    let v = phoneCodexTurn(fold(events + [ev(1, ["t": s("turn_end"), "status": s("completed"), "duration_ms": .number(900)])]).turns[0])
    check(v.answer?.id == "b", "the last text")
    check(v.units.map(\.kind) == ["message", "group"], "\(v.units.map(\.kind))")
    if case .exec(_, let info) = groupActs(v.units[1])[0] {
        check(info.command == "ls" && info.explore, "\(info.command) \(info.explore)")
    } else { check(false, "an exec act") }
    check(v.foldable, "the work folds behind the answer")
}

test("codex: a turn that only thinks so far says so; a question waits; steering stays a unit") {
    seq = 0
    let start = [ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("hi")]), ev(1, ["t": s("turn_start")])]
    check(phoneCodexTurn(fold(start).turns[0]).thinking, "thinking")
    let q = fold(start + [ev(1, ["t": s("question"), "qid": s("q1"), "prompt": s("Which?"), "options": .array([s("a"), s("b")])])])
    let qv = phoneCodexTurn(q.turns[0])
    check(!qv.thinking && qv.asking, "a question waits for the user")
    let steer = fold(start + [ev(1, ["t": s("user"), "input_id": s("u2"), "by": by("d"), "text": s("use release"), "steer": .bool(true)])])
    let sv = phoneCodexTurn(steer.turns[0])
    check(sv.units.map(\.kind) == ["steer"], "\(sv.units.map(\.kind))")
    if case .steer(_, let text, _, _, let extra) = sv.units[0] { check(text == "use release" && extra.inputID == "u2", "its text and id") }
    let thought = fold(start + [ev(1, ["t": s("thought"), "id": s("r"), "text": s("hmm"), "delta": .bool(true)])])
    let tv = phoneCodexTurn(thought.turns[0])
    if case .thought(_, _, let live) = tv.units[0] { check(live && !tv.thinking, "the live thought says it") } else { check(false, "a thought unit") }
}

test("codex: the runtime check and paths relative to the work folder") {
    check(phoneIsCodexRuntime(id: "x", name: "Mine", protocolName: "codex-app-server"), "by protocol")
    check(phoneIsCodexRuntime(id: "codex", name: "Codex", protocolName: nil), "by id")
    check(phoneIsCodexRuntime(id: "x", name: "My Codex build", protocolName: nil), "by name")
    check(!phoneIsCodexRuntime(id: "codex", name: "Codex", protocolName: "acp"), "the protocol wins")
    check(!phoneIsCodexRuntime(id: "claude", name: "Claude Code", protocolName: "claude-stream-json"), "claude")
    check(!phoneIsCodexRuntime(id: "x", name: "Codexy", protocolName: nil), "a whole word")
    check(phoneRelPath("/w/src/a.ts", "/w") == "src/a.ts", "inside")
    check(phoneRelPath("C:\\w\\src\\a.ts", "C:\\w") == "src/a.ts", "Windows")
    check(phoneRelPath("/elsewhere/a.ts", "/w") == "/elsewhere/a.ts", "outside")
    check(phoneRelPath("/w", "/w/") == ".", "the folder itself")
}

test("codex: phase and exit codes fold into the chat state") {
    seq = 0
    let c = fold([
        ev(1, ["t": s("message"), "id": s("m"), "text": s("a"), "delta": .bool(true), "phase": s("commentary")]),
        ev(1, ["t": s("message"), "id": s("m"), "text": s("b"), "delta": .bool(true)]),
        ev(1, ["t": s("tool_call"), "call_id": s("k"), "title": s("Shell x"), "kind": s("execute"), "status": s("in_progress")]),
        ev(1, ["t": s("tool_update"), "call_id": s("k"), "status": s("failed"), "exit_code": .number(2), "duration_ms": .number(50)]),
    ])
    check(c.turns[0].blocks[0] == .text(id: "m", text: "ab", phase: "commentary"), "\(c.turns[0].blocks[0])")
    check(tools(c.turns[0])[0].exitCode == 2 && tools(c.turns[0])[0].durationMs == 50, "code and time")
    check(PhoneChatSession(.object(["id": s("x"), "runtime": s("codex"), "name": s("Codex")])).runtime == "codex", "a session names its runtime")
}

test("codex: a line a plain reading cannot follow is a command: redirections, substitutions, jobs, writes") {
    for cmd in ["cat a.txt > b.txt", "rg x src 2>&1", "cat $(ls)", "cat `ls`", "echo \"$(date)\" && cat a", "sleep 1 &", "cat a.rs | python x.py",
                "find . -name '*.tmp' -delete", "rg --pre cat x", "sed -i.bak 's/a/b/' x", "fd -x rm"] {
        check(phoneParseCommand(cmd) == [.unknown], "\(cmd) → \(phoneParseCommand(cmd))")
    }
    check(phoneParseCommand("cat a.rs | wc -l") == [.read(path: "a.rs", name: "a.rs")], "a read piped into a count")
    check(phoneParseCommand("rg -n foo src | sort | uniq") == [.search(query: "foo", path: "src")], "a search piped through filters")
    check(phoneParseCommand("echo 'a > b' && cat x") == [.read(path: "x", name: "x")], "a > inside quotes is text")
}

test("codex: PowerShell's reads, searches and listings") {
    check(phoneParseCommand("Get-Content -Path src/a.ps1") == [.read(path: "src/a.ps1", name: "a.ps1")], "Get-Content -Path")
    check(phoneParseCommand("gc README.md") == [.read(path: "README.md", name: "README.md")], "gc")
    check(phoneParseCommand("Get-Content -Raw -LiteralPath C:\\w\\x.txt") == [.read(path: "C:\\w\\x.txt", name: "x.txt")], "-Raw is a switch")
    check(phoneParseCommand("Select-String -Pattern foo -Path src") == [.search(query: "foo", path: "src")], "Select-String -Pattern -Path")
    check(phoneParseCommand("sls foo src/*.cs") == [.search(query: "foo", path: "src/*.cs")], "sls by position")
    check(phoneParseCommand("Get-ChildItem -Recurse src") == [.listFiles(path: "src")], "Get-ChildItem")
    check(phoneParseCommand("Get-ChildItem -Path apps | Select-Object Name") == [.listFiles(path: "apps")], "piped into Select-Object")
}

test("codex: a command the turn's end closed has no exit code; a stopped one neither") {
    seq = 0
    let c = fold([
        ev(1, ["t": s("tool_call"), "call_id": s("k"), "title": s("Shell make"), "kind": s("execute"), "status": s("in_progress")]),
        ev(1, ["t": s("turn_end"), "status": s("completed"), "duration_ms": .number(10)]),
    ])
    let x = tools(c.turns[0])[0]
    check(x.endedByTurn && x.status == "completed", "closed by the turn")
    check(phoneExecInfo(x).exitCode == nil, "no code was reported, so none is shown")
}

test("codex: an added file's diff may be a real diff or its content; a cut file falls back to the call's content") {
    let real = phoneParseUnifiedDiff("@@ -0,0 +1,2 @@\n+a\n+b\n", kind: "add")
    check(real.add == 2 && real.lines.first?.kind == .hunk && real.lines[2].new == 2, "a unified diff for an added file")
    let content = phoneParseUnifiedDiff("@@ this line is content\nb\n", kind: "add")
    check(content.add == 2 && content.lines.allSatisfy { $0.kind == .add }, "content that starts with @@ is still content")
    let cut = phoneFileChanges(codexTool("f", "edit", kind: "edit", input: .object(["files": .array([
        .object(["path": s("/w/big.txt"), "kind": s("update"), "truncated": .bool(true)])])]),
        content: [.diff(path: "/w/big.txt", old: "a\n", new: "b\n")]))
    check(cut.count == 1 && cut[0].truncated && cut[0].add == 1 && cut[0].del == 1, "the content's diff, marked cut")
    check(phoneFilePatch(cut[0], cwd: "/w") == nil, "a cut file is not offered as a patch")
}

test("codex: a turn's diff gives its changes, each file once, as its net change") {
    let text = """
        diff --git a/src/a.py b/src/a.py
        index 1..2 100644
        --- a/src/a.py
        +++ b/src/a.py
        @@ -1,2 +1,2 @@
         x
        -y
        +z
        diff --git a/new.md b/new.md
        new file mode 100644
        --- /dev/null
        +++ b/new.md
        @@ -0,0 +1 @@
        +hello
        diff --git a/old.txt b/old.txt
        deleted file mode 100644
        --- a/old.txt
        +++ /dev/null
        @@ -1 +0,0 @@
        -bye
        diff --git a/from.rs b/to.rs
        similarity index 90%
        rename from from.rs
        rename to to.rs

        """
    let files = phoneParseTurnDiff(text, cwd: "D:\\w")
    check(files.map { "\($0.path) \($0.kind) \($0.add) \($0.del)" } == ["D:\\w\\src\\a.py update 1 1", "D:\\w\\new.md add 1 0", "D:\\w\\old.txt delete 0 1", "D:\\w\\from.rs update 0 0"],
          "\(files.map { "\($0.path) \($0.kind) \($0.add) \($0.del)" })")
    check(files[3].movePath == "D:\\w\\to.rs", "a rename")
    check(files[0].lines.map(\.kind) == [.hunk, .context, .del, .add] && files[0].lines[3].new == 2, "numbered lines")
    check(phoneJoinPath("/w/", "a/b") == "/w/a/b" && phoneJoinPath("/w", "/abs") == "/abs" && phoneJoinPath(nil, "a") == "a", "joining")
    var turn = PhoneChatTurn(n: 1)
    turn.status = "completed"
    turn.blocks = [.tool(codexTool("f", "edit", kind: "edit", input: .object(["files": .array([
        .object(["path": s("/w/other.py"), "kind": s("update"), "diff": s("@@ -1 +1 @@\n-a\n+b\n")])])])))]
    check(phoneTurnChanges(turn, cwd: "/w").map(\.path) == ["/w/other.py"], "without a turn diff: the edits")
    turn.diff = PhoneTurnDiff(text: "diff --git a/src/a.py b/src/a.py\n--- a/src/a.py\n+++ b/src/a.py\n@@ -1 +1 @@\n-y\n+z\n", truncated: false)
    check(phoneTurnChanges(turn, cwd: "/w").map(\.path) == ["/w/src/a.py"], "the turn's diff wins")
    turn.diff = PhoneTurnDiff(text: turn.diff!.text, truncated: true)
    check(phoneTurnChanges(turn, cwd: "/w").map(\.path) == ["/w/other.py"], "a cut turn diff does not")
    seq = 0
    let folded = fold([ev(1, ["t": s("turn_diff"), "diff": s("diff --git a/x b/x"), "truncated": .bool(true)])])
    check(folded.turns[0].diff == PhoneTurnDiff(text: "diff --git a/x b/x", truncated: true), "turn_diff folds into the turn")
}

test("codex: a file created and deleted in one turn is gone; deleted and written again it changed") {
    func edit(_ id: String, _ path: String, _ kind: String, _ diff: String) -> PhoneChatBlock {
        .tool(codexTool(id, "edit", kind: "edit", input: .object(["files": .array([.object(["path": s(path), "kind": s(kind), "diff": s(diff)])])])))
    }
    var turn = PhoneChatTurn(n: 1)
    turn.status = "completed"
    turn.blocks = [edit("1", "/w/tmp.txt", "add", "x\n"), edit("2", "/w/tmp.txt", "delete", "x\n"),
                   edit("3", "/w/a.txt", "delete", "a\n"), edit("4", "/w/a.txt", "add", "b\n"),
                   edit("5", "/w/n.txt", "add", "n\n"), edit("6", "/w/n.txt", "update", "@@ -1 +1 @@\n-n\n+m\n")]
    check(phoneTurnChanges(turn).map { "\($0.path) \($0.kind)" } == ["/w/a.txt update", "/w/n.txt add"], "\(phoneTurnChanges(turn).map { "\($0.path) \($0.kind)" })")
}

test("codex: a patch git applies, for added files too; none from an older host's edits") {
    let added = PhoneFileChange(path: "D:\\w\\t.py", movePath: nil, kind: "add", add: 2, del: 0, lines: phoneParseUnifiedDiff("a\nb\n", kind: "add").lines)
    check(phoneFilePatch(added, cwd: "D:\\w") == "diff --git a/t.py b/t.py\nnew file mode 100644\n--- /dev/null\n+++ b/t.py\n@@ -0,0 +1,2 @@\n+a\n+b\n",
          phoneFilePatch(added, cwd: "D:\\w") ?? "nil")
    let moved = PhoneFileChange(path: "/w/a.rs", movePath: "/w/b.rs", kind: "update", add: 1, del: 1, lines: phoneParseUnifiedDiff("@@ -1 +1 @@\n-x\n+y\n").lines)
    check(phoneFilePatch(moved, cwd: "/w")?.hasPrefix("diff --git a/a.rs b/b.rs\n--- a/a.rs\n+++ b/b.rs\n") == true, "a move")
    let old = phoneFileChanges(codexTool("f", "edit", kind: "edit", content: [.diff(path: "a.ts", old: "1\n", new: "2\n")]))
    check(phoneFilePatch(old[0], cwd: nil) == nil, "no line numbers, no patch")
}

test("codex: a failed or stopped turn without phases has no answer; reasoning after the last call stays in the live group") {
    seq = 0
    let base = [
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("hi")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("tool_call"), "call_id": s("c"), "title": s("Shell ls"), "kind": s("execute"), "status": s("completed")]),
        ev(1, ["t": s("message"), "id": s("b"), "text": s("So far: half."), "delta": .bool(false)]),
    ]
    check(phoneCodexTurn(fold(base + [ev(1, ["t": s("turn_end"), "status": s("failed"), "reason": s("x")])]).turns[0]).answer == nil, "failed")
    check(phoneCodexTurn(fold(base + [ev(1, ["t": s("turn_end"), "status": s("interrupted")])]).turns[0]).answer == nil, "stopped")
    seq = 0
    let live = fold([
        ev(1, ["t": s("tool_call"), "call_id": s("c"), "title": s("Shell ls"), "kind": s("execute"), "status": s("completed")]),
        ev(1, ["t": s("thought"), "id": s("r"), "text": s("next"), "delta": .bool(true)]),
    ])
    let v = phoneCodexTurn(live.turns[0])
    check(v.units.map(\.kind) == ["group"] && groupActs(v.units[0]).map(actKind) == ["exec", "thought"], "\(v.units.map(\.kind))")
    check(groupLive(v.units[0]), "and the group is the live line")
}

test("codex: edits that failed are said last; a running sub-agent shows the turn busy") {
    let failedEdit = PhoneCodexAct.patch(codexTool("f", "edit", kind: "edit", status: "failed"), [PhoneFileChange(path: "/w/a", movePath: nil, kind: "update", add: 1, del: 0, lines: [])])
    let ran = PhoneCodexAct.exec(codexTool("c", "Shell make"), phoneExecInfo(codexTool("c", "Shell make")))
    check(phoneCodexSummarySentence([failedEdit, ran], lang: .en) == "Ran a command and failed to edit a file", phoneCodexSummarySentence([failedEdit, ran], lang: .en))
    check(phoneCodexSummarySentence([failedEdit], lang: .zh) == "编辑文件失败", phoneCodexSummarySentence([failedEdit], lang: .zh))
    check(phoneGroupSummary([failedEdit]).add == 0, "a failed edit's lines do not count")
    seq = 0
    let c = fold([ev(1, ["t": s("tool_call"), "call_id": s("a"), "title": s("agent review"), "kind": s("think"), "status": s("in_progress")])])
    let v = phoneCodexTurn(c.turns[0])
    check(v.units.map(\.kind) == ["agent"] && !v.thinking, "a sub-agent working is not thinking")
}

test("codex: the desktop's newest cases: writes and hand-offs never read as reads; flag values are not files") {
    for c in ["ls build | xargs rm -rf", "cat a.txt > b.txt", "ls > out.txt", "cat install.sh | bash", "find . -name '*.tmp' -delete",
              "find . -name x -exec rm {} +", "sed -n -i '1p' x", "cat $(rm -rf x)", "echo hi > f && cat f", "grep -l foo -r . | xargs sed -i 's/a/b/'",
              "rg foo 2>/dev/null", "cat a | tee b", "sleep 1 &", "fd -x rm"] {
        check(phoneParseCommand(c) == [.unknown], "\(c) → \(phoneParseCommand(c))")
    }
    check(phoneParseCommand("rg --files src | sort | head -20") == [.listFiles(path: "src")], "rg --files | sort | head")
    check(phoneParseCommand("cat a.log | grep ERROR | wc -l") == [.read(path: "a.log", name: "a.log")], "cat | grep | wc")
    check(phoneParseCommand("Get-Content -Path src\\app.ts -TotalCount 80") == [.read(path: "src\\app.ts", name: "app.ts")], "Get-Content -Path -TotalCount")
    check(phoneParseCommand("Get-Content src\\app.ts -Tail 20") == [.read(path: "src\\app.ts", name: "app.ts")], "Get-Content -Tail")
    check(phoneParseCommand("rg -e foo -e bar src") == [.search(query: "foo", path: "src")], "rg -e -e")
    check(phoneParseCommand("Select-String -Pattern TODO -Path *.ts") == [.search(query: "TODO", path: "*.ts")], "Select-String")
    let md = phoneParseUnifiedDiff("# Notes\n@@ not a hunk\n- a list item\n", kind: "add")
    check(md.add == 3 && md.del == 0 && md.lines.allSatisfy { $0.kind == .add }, "an added Markdown file is content")
    let del = phoneParseUnifiedDiff("@@ x\nold\n", kind: "delete")
    check(del.add == 0 && del.del == 2, "a deleted file's content")
}

test("codex: two edits of a file are summed without the turn's diff and netted with it") {
    func edit(_ id: String, _ diff: String) -> PhoneChatBlock {
        .tool(codexTool(id, "edit", kind: "edit", input: .object(["files": .array([.object(["path": s("/w/m.py"), "kind": s("update"), "diff": s(diff)])])])))
    }
    var turn = PhoneChatTurn(n: 1)
    turn.status = "completed"
    turn.blocks = [edit("e1", "@@ -1,2 +1,2 @@\n a\n-b\n+B\n"), edit("e2", "@@ -1,2 +1,2 @@\n a\n-B\n+b2\n")]
    let summed = phoneTurnChanges(turn)[0]
    check(summed.add == 2 && summed.del == 2, "summed")
    turn.diff = PhoneTurnDiff(text: "diff --git a/m.py b/m.py\nindex 1..2 100644\n--- a/m.py\n+++ b/m.py\n@@ -1,2 +1,2 @@\n a\n-b\n+b2\n"
        + "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n", truncated: false)
    let net = phoneTurnChanges(turn, cwd: "/w").map { "\($0.path) \($0.kind) \($0.add) \($0.del)" }
    check(net == ["/w/m.py update 1 1", "/w/new.txt add 2 0"], "\(net)")
    let cut = phoneFileChanges(codexTool("x", "edit", kind: "edit", input: .object(["files": .array([
        .object(["path": s("/w/big.txt"), "kind": s("add"), "diff": s(""), "truncated": .bool(true)])])]),
        content: [.diff(path: "/w/big.txt", old: nil, new: "a\nb\n")]))[0]
    check(cut.add == 2 && cut.truncated && phoneFilePatch(cut, cwd: "/w") == nil, "an added file past the budget")
}

test("codex: copied patches apply with git") {
    let fm = FileManager.default
    let dir = fm.temporaryDirectory.appendingPathComponent("cdx-patch-\(UUID().uuidString)")
    defer { try? fm.removeItem(at: dir) }
    func git(_ args: [String]) -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        p.arguments = ["git"] + args
        p.currentDirectoryURL = dir
        p.standardOutput = FileHandle.nullDevice; p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return -1 }
        p.waitUntilExit()
        return p.terminationStatus
    }
    try? fm.createDirectory(at: dir.appendingPathComponent("src"), withIntermediateDirectories: true)
    guard git(["init", "-q"]) == 0 else { return print("skip  codex: no git") }
    try? "a\nb\nc\n".write(to: dir.appendingPathComponent("src/m.py"), atomically: true, encoding: .utf8)
    try? "bye\n".write(to: dir.appendingPathComponent("old.txt"), atomically: true, encoding: .utf8)
    func edit(_ path: String, _ kind: String, _ diff: String) -> [PhoneFileChange] {
        phoneFileChanges(codexTool("e", "edit", kind: "edit", input: .object(["files": .array([
            .object(["path": s(dir.appendingPathComponent(path).path), "kind": s(kind), "diff": s(diff)])])])))
    }
    let files = edit("src/m.py", "update", "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n") + edit("new.md", "add", "# New\n@@ text\n") + edit("old.txt", "delete", "bye\n")
    let patch = files.compactMap { phoneFilePatch($0, cwd: dir.path) }.joined()
    try? patch.write(to: dir.appendingPathComponent("p.diff"), atomically: true, encoding: .utf8)
    check(files.count == 3 && git(["apply", "--check", "p.diff"]) == 0, "git apply --check:\n\(patch)")
    check(git(["apply", "p.diff"]) == 0, "git apply")
    check((try? String(contentsOf: dir.appendingPathComponent("src/m.py"), encoding: .utf8)) == "a\nB\nc\n", "the edit applied")
    check((try? String(contentsOf: dir.appendingPathComponent("new.md"), encoding: .utf8)) == "# New\n@@ text\n", "the new file")
    check(!fm.fileExists(atPath: dir.appendingPathComponent("old.txt").path), "the deleted file")
}

test("codex: a late phase arrives as an empty delta and only sets the phase") {
    seq = 0
    let streamed = [
        ev(0, ["t": s("user"), "input_id": s("u"), "by": by("d"), "text": s("hi")]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("tool_call"), "call_id": s("c"), "title": s("Shell make"), "kind": s("execute"), "status": s("completed")]),
        ev(1, ["t": s("message"), "id": s("m"), "text": s("All "), "delta": .bool(true)]),
        ev(1, ["t": s("message"), "id": s("m"), "text": s("done."), "delta": .bool(true)]),
    ]
    check(phoneCodexTurn(fold(streamed).turns[0]).answer == nil, "no phase yet, still running: a remark for now")
    let phased = fold(streamed + [ev(1, ["t": s("message"), "id": s("m"), "text": s(""), "delta": .bool(true), "phase": s("final_answer")])])
    check(phased.turns[0].blocks.last == .text(id: "m", text: "All done.", phase: "final_answer"), "\(String(describing: phased.turns[0].blocks.last))")
    let v = phoneCodexTurn(phased.turns[0])
    check(v.answer?.text == "All done." && v.foldable, "now it is the answer and the work folds")
}

test("a question request that is over waits no more, whoever ended it") {
    var c = fold([
        ev(0, ["t": s("state"), "state": s("running"), "queued": .array([])]),
        ev(1, ["t": s("turn_start")]),
        ev(1, ["t": s("question"), "qid": s("name"), "prompt": s("Name?"), "request": s("7-0"), "expires_in_ms": .number(120000)]),
        ev(1, ["t": s("question"), "qid": s("old"), "prompt": s("Other?"), "request": s("7-1")]),
    ])
    check(c.phase.what == "asking", "it waits")
    c.apply(ev(1, ["t": s("question_done"), "request": s("7-0"), "how": s("expired")]))
    if case .question(let q) = c.turns[0].blocks[0] { check(q.answered && q.answer == nil, "expired: over, no answer") } else { check(false, "question") }
    if case .question(let q) = c.turns[0].blocks[1] { check(!q.answered, "the other request still waits") } else { check(false, "question") }
    c.apply(ev(1, ["t": s("question_done"), "request": s("7-1"), "answers": .object(["old": .array([s("yes")])]), "how": s("answered")]))
    if case .question(let q) = c.turns[0].blocks[1] { check(q.answered && q.answer == "yes", "answered elsewhere") } else { check(false, "question") }
    check(c.phase.what != "asking", "nothing waits")
    c.markAnswered("old", answer: "no")
    if case .question(let q) = c.turns[0].blocks[1] { check(q.answer == "yes", "an answered question keeps its answer") } else { check(false, "question") }
}

print(failures == 0 ? "all passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
