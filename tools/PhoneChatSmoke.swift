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
            let chat = PhoneChatStore(client: client, host: host)
            await chat.loadRuntimes()
            check("chat_runtimes lists the stand-in agent as usable", chat.runtimes.contains { $0.id == "fake" && $0.usable }
                  && chat.canStart, chat.runtimes.map { "\($0.id):\($0.verdict)" }.joined(separator: ","))
            check("the form preselects a usable agent", chat.runtimes.first { $0.id == chat.newRuntime }?.usable == true, chat.newRuntime)
            chat.newRuntime = "fake"; chat.newCwd = cwd + "/does-not-exist"
            await chat.start()
            check("a refused start shows the host's words and opens nothing", chat.error != nil && chat.current == nil, chat.error ?? "nil")
            chat.error = nil
            chat.newCwd = cwd; chat.newPrompt = "from the phone"; chat.showNew = true
            await chat.start()
            let mine = chat.current?.id ?? ""
            check("the new-chat form starts a session and opens it", !mine.isEmpty && !chat.showNew && chat.error == nil, chat.error ?? "")
            check("the folder is remembered for the next start", chat.recentFolders.first == cwd, chat.recentFolders.joined(separator: ","))
            check("it opens with the start ticket, read-write", await wait("connected") { chat.connected } && chat.role == "rw")
            await chat.leave()
            check("chat_sessions lists both sessions", Set(chat.sessions.map(\.id)) == [mine, foreign],
                  chat.sessions.map(\.id).joined(separator: ","))

            // Another device's session: every admin device may talk to it.
            await chat.open(chat.sessions.first { $0.id == foreign }!)
            check("a session started elsewhere opens read-write", await wait("connected") { chat.connected } && chat.canWrite,
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

            // The stand-in agent can steer (like Claude Code): a message sent while it works joins the turn.
            check("the agent can steer", chat.caps?.steer == true, "\(String(describing: chat.caps))")
            chat.draft = "SLOW job"
            await chat.send()
            check("a slow turn shows a running tool", await wait("running tool") {
                chat.log.busy && chat.log.turns.last?.blocks.contains { if case .tool(let t) = $0 { t.status != "completed" } else { false } } == true
            }, "state \(chat.log.state)")
            check("while it works, sending steers", chat.sendMode == "steer")
            let steeredTurn = chat.log.turns.last?.n
            chat.draft = "use the release profile"
            await chat.send()
            check("the steer joins the running turn as a user line", await wait("steer") {
                chat.log.turns.last?.n == steeredTurn
                    && chat.log.turns.last?.blocks.contains { if case .user(let text, _, _) = $0 { text == "use the release profile" } else { false } } == true
            }, "\(chat.log.turns.last?.blocks.count ?? 0) blocks")
            check("the agent acts on it in the same turn", await wait("steered") {
                chat.log.turns.last?.n == steeredTurn && chat.log.turns.last?.status == "completed" && chat.log.turns.last?.reply.hasPrefix("steered:") == true
            }, chat.log.turns.last?.reply ?? "")
            check("nothing was queued", chat.log.queued.isEmpty)

            // An agent that cannot steer (caps.steer false, e.g. ACP agents): a message waits in the queue.
            chat.caps?.steer = false
            chat.draft = "SLOW again"
            await chat.send()
            _ = await wait("running again") { chat.log.busy && chat.log.turns.last?.status == "running" }
            check("while it works, sending queues", chat.sendMode == "queue")
            chat.draft = "queued while busy"
            await chat.send()
            check("a message sent while busy waits in the queue", await wait("queue") { chat.log.queued.map(\.text) == ["queued while busy"] },
                  "\(chat.log.queued)")
            chat.draft = "take me back"
            await chat.send()
            _ = await wait("second queued") { chat.log.queued.count == 2 }
            if let q = chat.log.queued.last { await chat.editQueued(q.id, text: "edited in the queue") }
            check("editing a queued message re-queues the new text", await wait("edit") {
                chat.log.queued.map(\.text) == ["queued while busy", "edited in the queue"]
            }, "\(chat.log.queued.map(\.text))")
            if let q = chat.log.queued.last { await chat.dequeue(q.id) }
            check("a queued message can be taken back", await wait("dequeue") { chat.log.queued.map(\.text) == ["queued while busy"] },
                  "\(chat.log.queued.map(\.text))")
            await chat.interrupt()
            check("stop ends the turn as interrupted", await wait("interrupted") {
                chat.log.turns.contains { $0.userText == "SLOW again" && $0.status == "interrupted" }
            })
            check("the queued message runs next", await wait("queued turn") {
                chat.log.turns.last?.userText == "queued while busy" && chat.log.turns.last?.status == "completed" && !chat.log.busy
            })
            let ending = chat.log.turns.first { $0.userText == "SLOW again" }?.ending ?? ""
            check("the interrupted turn says so", ending.hasPrefix("已停止"), ending)

            // The composer: the agent's commands, files for @, images, and messages on their way.
            check("the host acknowledges messages", chat.hostAcks)
            check("the agent's commands and skills are known", Set(chat.commands.map(\.name)).isSuperset(of: ["review", "tidy"])
                  && chat.commands.contains { $0.name == "tidy" && $0.kind == "skill" }, chat.commands.map { "\($0.kind):\($0.name)" }.joined(separator: ","))
            check("the / menu finds a command by name", phoneMatchCommands(chat.commands, "rev").first?.insert == "/review")
            let found = (try? await chat.files("report")) ?? []
            check("@ lists files in the session's folder", found.contains("docs/report.html"), found.joined(separator: ","))
            let both = (try? await chat.files("docs notes")) ?? []
            check("@ matches every word", both == ["docs/notes.md"], both.joined(separator: ","))
            check("the agent takes images", chat.takesImages)
            let png = phoneFixturePNG(width: 320, height: 200)
            chat.attach(phonePrepareImage(png, name: "chart")!)
            chat.draft = "look at this"
            await chat.send()
            check("a sent message shows as on its way at once", chat.outgoing.count == 1 && chat.draft.isEmpty && chat.attachments.isEmpty)
            check("the image reaches the agent whole", await wait("image turn") {
                chat.log.turns.last?.userText == "look at this" && chat.log.turns.last?.reply.contains("[images: image/png \(png.base64EncodedString().count)]") == true
            }, chat.log.turns.last?.reply ?? "")
            check("the message leaves the on-its-way list once the conversation has it", await wait("settled") { chat.outgoing.isEmpty }, "\(chat.outgoing)")
            let row = chat.log.turns.last.flatMap { phoneTurnRows($0).first }
            if case .user(_, _, _, _, _, let extra)? = row {
                check("the conversation keeps the image's name and size; this phone keeps its picture",
                      extra.attachments.first?.mime == "image/png" && extra.attachments.first?.bytes == png.count && chat.sentImages[extra.inputID ?? ""]?.first?.data == png,
                      "\(extra)")
            } else { check("the user row", false) }
            // Five images: the host refuses the message; the bubble says so and can be sent again or dropped.
            chat.attachments = (0..<5).map { PhoneImage(name: "x\($0).png", mime: "image/png", data: png) }
            chat.draft = "too many"
            await chat.send()
            check("a refused message is shown as not delivered, with the host's words", await wait("refused") { chat.outgoing.first?.state == "failed" },
                  "\(chat.outgoing.map { "\($0.state) \($0.error ?? "")" })")
            check("a refused message is not part of the conversation", chat.log.turns.last?.userText == "look at this")
            if let o = chat.outgoing.first { chat.discard(o) }
            check("dropping it puts its text back", chat.outgoing.isEmpty && chat.draft == "too many", chat.draft)
            chat.draft = ""; chat.attachments = []

            // The folder picker browses the host.
            if let listed = try? await chat.browse(cwd) {
                check("the folder picker lists the session folder's subfolders", listed.folders.contains("docs") && listed.folders.contains("out")
                      && !listed.folders.contains(".hidden"), listed.folders.joined(separator: ","))
            } else { check("the folder picker lists the session folder", false) }
            _ = try? await chat.browse("~")
            check("listing ~ tells where the host user's home is", chat.home == NSHomeDirectory() || chat.home?.isEmpty == false, chat.home ?? "nil")
            do { _ = try await chat.browse(cwd + "/nope"); check("a missing folder is refused", false) }
            catch { check("a missing folder is refused with the host's words", !error.localizedDescription.isEmpty, error.localizedDescription) }

            // Previews: the files a turn wrote or named, read from the host into memory.
            let outside = (cwd as NSString).deletingLastPathComponent + "/outside"
            chat.draft = "see `docs/report.html`, `out/chart.png` and `\(outside)/named.md`"
            await chat.send()
            check("the reply names three previewable files", await wait("files turn") {
                chat.log.turns.last.map { phoneTurnFiles($0.blocks) } == ["docs/report.html", "out/chart.png", outside + "/named.md"]
            }, chat.log.turns.last.map { phoneTurnFiles($0.blocks).joined(separator: ",") } ?? "")
            let page = try? await chat.preview("docs/report.html")
            check("a page in the session's folder previews byte for byte", page == FileManager.default.contents(atPath: cwd + "/docs/report.html"))
            let chart = try? await chat.preview("out/chart.png")
            check("an image previews and decodes", chart.flatMap { phoneDecodeImage($0) }?.width == 64)
            let named = try? await chat.preview(outside + "/named.md")
            check("a file outside the folder that the agent named previews", named.map { String(decoding: $0, as: UTF8.self) } == "# named\n")
            do { _ = try await chat.preview(outside + "/secret.md"); check("an unnamed file outside the folder is refused", false) }
            catch { check("an unnamed file outside the folder is refused", error.localizedDescription.contains("outside"), error.localizedDescription) }
            do { _ = try await chat.preview("Cargo.toml"); check("a kind that is not previewed is refused", false) }
            catch { check("a kind that is not previewed is refused", true, error.localizedDescription) }

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
