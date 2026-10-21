// Drives the iPhone app's chat code (PhoneClient, PhoneChatStore, PhoneChatLog) against a real host, on
// macOS, through the same embedded controller the phone uses. Run by tools/phone-chat-smoke.py, which
// starts the host and passes: <profile dir> <pairing ticket> <work folder> <foreign session id>.
import AIRemotePhone
import Foundation

@MainActor func wait(_ what: String, seconds: Double = 20, _ condition: @MainActor () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(100))
    }
    print("     (timed out waiting for \(what))")
    return false
}

var failures = 0
func check(_ name: String, _ ok: Bool, _ detail: String = "") {
    print("\(ok ? "ok  " : "FAIL")  \(name)" + (detail.isEmpty ? "" : "  (\(detail))"))
    if !ok { failures += 1 }
}

@main struct PhoneChatSmoke {
    @MainActor static func main() async {
        setvbuf(stdout, nil, _IONBF, 0)
        let args = CommandLine.arguments
        guard args.count == 5 else { print("usage: phone-chat-smoke <profile> <ticket> <cwd> <foreign session>"); exit(2) }
        let (profile, ticket, cwd, foreign) = (args[1], args[2], args[3], args[4])
        let client = PhoneClient(directory: profile)
        do {
            try await client.start()
            let paired = try await client.request(["op": .string("pair"), "ticket": .string(ticket)])
            check("the phone pairs", paired["sas"].string != nil, paired.text)
            let host = try await client.request(["op": .string("hosts")]).array.first?["name"].string ?? ""
            // The phone has no form to start agents; a session it started is the one it may write to.
            let started = try await client.call(host: host, tool: "chat_start", args: [
                "runtime": .string("fake"), "cwd": .string(cwd), "prompt": .string("from the phone")])
            let mine = started["structuredContent"]["session"].string ?? ""
            check("chat_start from the phone", !mine.isEmpty)

            let chat = PhoneChatStore(client: client, host: host)
            await chat.loadSessions()
            check("chat_sessions lists both sessions", Set(chat.sessions.map(\.id)) == [mine, foreign],
                  chat.sessions.map(\.id).joined(separator: ","))

            // Another device's session: everything is shown, nothing can be sent.
            await chat.open(chat.sessions.first { $0.id == foreign }!)
            check("a foreign session opens read-only", await wait("connected") { chat.connected } && chat.role == "ro" && !chat.canWrite,
                  "role \(chat.role ?? "nil")")
            check("its replayed turn has the user line and the answer", await wait("replay") {
                chat.log.turns.first?.userText == "from the desktop" && chat.log.turns.first?.status == "completed"
            }, "\(chat.log.turns)")

            // The phone's own session: send, queue, stop.
            await chat.leave()
            await chat.open(chat.sessions.first { $0.id == mine }!)
            check("its own session opens read-write", await wait("connected") { chat.connected } && chat.role == "rw")
            check("the first turn (the start prompt) is answered", await wait("first turn") {
                chat.log.turns.first?.status == "completed"
            })
            let answer: String? = chat.log.turns.first?.blocks.compactMap { if case .text(_, let t) = $0 { t } else { nil } }.first
            check("streamed deltas fold into one message", answer == "you said: from the phone", answer ?? "nil")

            chat.draft = "hello again"
            await chat.send()
            check("send clears the draft", chat.draft.isEmpty)
            check("the message is answered as a new turn", await wait("second turn") {
                chat.log.turns.count == 2 && chat.log.turns[1].userText == "hello again" && chat.log.turns[1].status == "completed"
            }, "\(chat.log.turns.count) turns")

            chat.draft = "SLOW job"
            await chat.send()
            check("a slow turn shows a running tool", await wait("running tool") {
                chat.log.busy && chat.log.turns.last?.blocks.contains { if case .tool(let t) = $0 { t.status != "completed" } else { false } } == true
            }, "state \(chat.log.state)")
            chat.draft = "queued while busy"
            await chat.send()
            check("a message sent while busy waits in the queue", await wait("queue") { chat.log.queued == ["queued while busy"] },
                  "\(chat.log.queued)")
            await chat.interrupt()
            check("stop ends the turn as interrupted", await wait("interrupted") {
                chat.log.turns.contains { $0.userText == "SLOW job" && $0.status == "interrupted" }
            })
            check("the queued message runs next", await wait("queued turn") {
                chat.log.turns.last?.userText == "queued while busy" && chat.log.turns.last?.status == "completed" && !chat.log.busy
            })
            let ending = chat.log.turns.first { $0.userText == "SLOW job" }?.ending ?? ""
            check("the interrupted turn says so", ending.hasPrefix("已停止"), ending)

            // A dropped connection resumes from the last event, without repeating any.
            let before = chat.log
            await chat.close()
            check("closing disconnects", !chat.connected)
            await chat.resume()
            check("resume reconnects", await wait("reconnect") { chat.connected })
            try? await Task.sleep(for: .milliseconds(500))
            check("resume replays nothing twice", chat.log.turns == before.turns, "\(chat.log.turns.count) vs \(before.turns.count)")
            await chat.close()
        } catch {
            check("no errors", false, error.localizedDescription)
        }
        await client.close()
        print(failures == 0 ? "all passed" : "\(failures) failed")
        exit(failures == 0 ? 0 : 1)
    }
}
