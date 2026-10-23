import SwiftUI

// A Codex turn on the phone, laid out the way Codex's own app shows one (PhoneCodexModel.swift has the
// structure): the user's message on the right; the agent's work as quiet one-line units that open in
// place — a command opens to its shell output, an edit to its diff with line numbers — with what the
// agent said between them; "Worked for …" folding all of that once the answer starts; the answer; and
// the files the turn changed, each opening to its diff. The phone version of the desktop's
// apps/desktop/src/ui/codex/CodexTurn.tsx and styles/codex.css: the same measures (ink at 60 % for
// activity and 45 % for details, a faint ground for shells and diffs, green and red diff rows), with a
// tap where the desktop has a hover, so chevrons always show (dimmer until opened).

/// The view's words in the phone's language (PhoneCodexLang).
private func w(_ key: String, _ vars: [String: String] = [:]) -> String { phoneCodexText(key, vars) }
private var zh: Bool { PhoneCodexLang.current == .zh }

/// The desktop's codex.css colors: ink, its 60 / 45 / 35 % shades, hairlines, faint grounds, diff colors.
enum PhoneCodexInk {
    static let full = Color.primary
    static let s60 = Color.primary.opacity(0.6)
    static let s45 = Color.primary.opacity(0.45)
    static let s35 = Color.primary.opacity(0.35)
    static let prose = Color.primary.opacity(0.92)
    static let line = Color.primary.opacity(0.1)
    static let ground = Color.primary.opacity(0.035)
    static let hover = Color.primary.opacity(0.05)
    static let bubble = Color.primary.opacity(0.07)
    static let add = Color(red: 0.16, green: 0.62, blue: 0.33)
    static let del = Color(red: 0.86, green: 0.25, blue: 0.24)
    static let addBG = Color.green.opacity(0.14)
    static let delBG = Color.red.opacity(0.13)
}
private typealias Ink = PhoneCodexInk

// MARK: - Pieces

/// Text that shimmers while the agent works on it: a brighter band sweeps across it.
private struct CodexShimmer: ViewModifier {
    let on: Bool
    @Environment(\.accessibilityReduceMotion) private var reduce
    func body(content: Content) -> some View {
        if on && !reduce {
            content.overlay {
                TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
                    let phase = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 2.2) / 2.2
                    GeometryReader { g in
                        let band = max(48, g.size.width * 0.35)
                        LinearGradient(colors: [.clear, Color.primary.opacity(0.9), .clear], startPoint: .leading, endPoint: .trailing)
                            .frame(width: band)
                            .offset(x: -band + (g.size.width + band) * phase)
                    }
                }
                .mask(content)
                .allowsHitTesting(false)
                .accessibilityHidden(true)
            }
        } else {
            content
        }
    }
}

extension View {
    fileprivate func codexShimmer(_ on: Bool) -> some View { modifier(CodexShimmer(on: on)) }
}

private struct Chevron: View {
    let open: Bool
    var body: some View {
        Image(systemName: "chevron.right").font(.caption2.weight(.semibold))
            .foregroundStyle(open ? Ink.s45 : Ink.s35)
            .rotationEffect(.degrees(open ? 90 : 0))
            .accessibilityHidden(true)
    }
}

/// A small copy button; it shows a check for a moment after copying.
private struct CopyButton: View {
    let text: String
    let label: String
    @PhoneViewState private var copied = false
    var body: some View {
        Button {
            phoneCopy(text)
            copied = true
            Task { try? await Task.sleep(for: .seconds(1.2)); copied = false }
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc").font(.caption).frame(width: 26, height: 24).contentShape(Rectangle())
        }
        .buttonStyle(.plain).foregroundStyle(Ink.s45)
        .accessibilityLabel(label)
    }
}

/// A line's words: a verb, details in a lighter shade, commands in monospace, a diff's counts.
private struct Words {
    var text = AttributedString()
    mutating func verb(_ s: String, failed: Bool = false) {
        var x = AttributedString(s)
        if failed { x.foregroundColor = Ink.del }
        text += x
    }
    mutating func plain(_ s: String) { text += AttributedString(s) }
    mutating func detail(_ s: String) {
        var x = AttributedString(s)
        x.foregroundColor = Ink.s45
        text += x
    }
    mutating func code(_ s: String) {
        var x = AttributedString(s)
        x.font = Font.footnote.monospaced()
        x.foregroundColor = Color.primary.opacity(0.72)
        text += x
    }
    mutating func bad(_ s: String) {
        var x = AttributedString(s)
        x.foregroundColor = Ink.del
        text += x
    }
    mutating func stat(add: Int, del: Int) {
        if add > 0 {
            var x = AttributedString("  +\(add)")
            x.font = Font.footnote.monospacedDigit(); x.foregroundColor = Ink.add
            text += x
        }
        if del > 0 {
            var x = AttributedString((add > 0 ? " " : "  ") + "−\(del)")
            x.font = Font.footnote.monospacedDigit(); x.foregroundColor = Ink.del
            text += x
        }
    }
}

/// A one-line activity: its words, and (when it has any) details that open below it on a tap.
private struct ActLine<Details: View, Trailing: View>: View {
    let label: AttributedString
    var live = false
    /// Reasoning reads quieter than work.
    var quiet = false
    let opens: Bool
    var id: String
    var indent: CGFloat = 0
    @ViewBuilder let details: () -> Details
    @ViewBuilder let trailing: () -> Trailing
    @PhoneViewState private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 4) {
                Button {
                    guard opens else { return }
                    withAnimation(.easeOut(duration: 0.2)) { open.toggle() }
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 5) {
                        Text(label).font(.subheadline).lineLimit(1).truncationMode(.tail)
                            .foregroundStyle(open ? (quiet ? Ink.s60 : Ink.full) : (quiet ? Ink.s45 : Ink.s60))
                            .codexShimmer(live)
                        if opens { Chevron(open: open) }
                    }
                    .padding(.vertical, 3)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier(id)
                .accessibilityValue(opens ? (open ? w("cdx_collapse") : w("cdx_expand")) : "")
                Spacer(minLength: 0)
                trailing()
            }
            if open && opens {
                details()
                    .padding(.leading, indent).padding(.top, 6).padding(.bottom, 4)
                    .transition(.opacity.combined(with: .offset(y: -6)))
            }
        }
    }
}

extension ActLine where Trailing == EmptyView {
    init(label: AttributedString, live: Bool = false, quiet: Bool = false, opens: Bool, id: String, indent: CGFloat = 0,
         @ViewBuilder details: @escaping () -> Details) {
        self.init(label: label, live: live, quiet: quiet, opens: opens, id: id, indent: indent, details: details, trailing: { EmptyView() })
    }
}

// MARK: - Words for one act

private func running(_ x: PhoneChatTool) -> Bool { x.running }
private func failed(_ x: PhoneChatTool) -> Bool { x.status == "failed" && !x.stopped }

private func oneLine(_ s: String) -> String {
    let first = s.components(separatedBy: "\n").first ?? s
    return first.count < s.count ? first + " …" : first
}

private func actionWords(_ a: PhoneCmdAction, done: Bool, cwd: String?, into words: inout Words) {
    switch a {
    case .read(let path, let name):
        words.verb(w(done ? "cdx_v_read" : "cdx_v_reading")); words.plain(" ")
        let rel = phoneRelPath(path, cwd)
        words.detail(rel.isEmpty ? name : rel)
    case .search(let query, let path):
        words.verb(w(done ? "cdx_v_searched" : "cdx_v_searching")); words.plain(" ")
        words.detail((query.map { w("cdx_d_for", ["q": $0]) } ?? w("cdx_d_files")) + (path.map { " " + w("cdx_d_in", ["p": phoneRelPath($0, cwd)]) } ?? ""))
    case .listFiles(let path):
        words.verb(w(done ? "cdx_v_listed" : "cdx_v_listing")); words.plain(" ")
        words.detail(path.map { w("cdx_d_files_in", ["p": phoneRelPath($0, cwd)]) } ?? w("cdx_d_files"))
    case .unknown: break
    }
}

private func execWords(_ info: PhoneExecInfo, _ x: PhoneChatTool, cwd: String?) -> Words {
    var words = Words()
    let done = !running(x)
    if info.explore {
        for (i, a) in info.actions.enumerated() {
            if i > 0 { words.detail(", ") }
            actionWords(a, done: done, cwd: info.cwd ?? cwd, into: &words)
        }
        return words
    }
    words.verb(w(!done ? "cdx_v_running" : x.stopped ? "cdx_v_stopped" : "cdx_v_ran")); words.plain(" ")
    words.code(oneLine(info.command))
    if done, let took = phoneCodexElapsed(info.durationMs, zh: zh) { words.detail(" " + w("cdx_in_time", ["t": took])) }
    if done, !x.stopped, let code = info.exitCode, code != 0 { words.bad(" · " + w("cdx_exit_short", ["n": "\(code)"])) }
    return words
}

private func fileVerb(_ f: PhoneFileChange, done: Bool) -> String {
    if f.kind == "add" { return done ? "cdx_v_created" : "cdx_v_creating" }
    if f.kind == "delete" { return done ? "cdx_v_deleted" : "cdx_v_deleting" }
    if f.movePath != nil && done { return "cdx_v_moved" }
    return done ? "cdx_v_edited" : "cdx_v_editing"
}

private func fileWords(_ f: PhoneFileChange, _ x: PhoneChatTool) -> Words {
    var words = Words()
    words.verb(w(failed(x) ? "cdx_v_failed_edit" : fileVerb(f, done: !running(x))), failed: failed(x)); words.plain(" ")
    words.detail(phoneBaseName(f.path) + (f.movePath.map { " " + w("cdx_d_to", ["p": phoneBaseName($0)]) } ?? ""))
    // An edit that did not apply changed nothing: no counts.
    if !failed(x) { words.stat(add: f.add, del: f.del) }
    return words
}

/// A thought's first line, without its Markdown marks.
private func thoughtHead(_ text: String) -> String {
    let first = text.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: "\n").first ?? ""
    let bare = first.replacingOccurrences(of: "^[#>\\s*_]+|[*_]+$", with: "", options: .regularExpression)
    return (try? AttributedString(markdown: bare)).map { String($0.characters) } ?? bare
}

/// The words of an act as its line says them (present tense while it runs).
private func actWords(_ a: PhoneCodexAct, cwd: String?) -> Words {
    switch a {
    case .exec(let x, let info): return execWords(info, x, cwd: cwd)
    case .patch(let x, let files):
        if files.count == 1 { return fileWords(files[0], x) }
        var words = Words()
        words.verb(w(running(x) ? "cdx_v_editing" : "cdx_v_edited")); words.plain(" ")
        words.detail(files.map { phoneBaseName($0.path) }.joined(separator: ", "))
        words.stat(add: files.reduce(0) { $0 + $1.add }, del: files.reduce(0) { $0 + $1.del })
        return words
    case .web(let x, let query):
        var words = Words()
        words.verb(w(running(x) ? "cdx_v_web_ing" : "cdx_v_web"))
        if !query.isEmpty { words.detail(" " + w("cdx_d_for", ["q": query])) }
        return words
    case .tool(let x):
        var words = Words()
        words.verb(w(running(x) ? "cdx_v_calling" : x.status == "failed" ? "cdx_v_failed" : "cdx_v_called"), failed: failed(x)); words.plain(" ")
        let title = x.title.trimmingCharacters(in: .whitespacesAndNewlines)
        words.code(title.isEmpty ? "tool" : title)
        return words
    case .thought(let t):
        var words = Words()
        words.verb(w("cdx_thought")); words.plain(" ")
        words.detail(thoughtHead(t.text))
        return words
    }
}

// MARK: - Details: a command's shell, an edit's diff, a tool's input and output

/// Lines of output shown before "N more lines".
private let outputLines = 40

private struct ShellCard: View {
    let info: PhoneExecInfo
    let tool: PhoneChatTool
    @PhoneViewState private var all = false

    var body: some View {
        var out = info.output
        while out.hasSuffix("\n") { out.removeLast() }
        let lines = out.isEmpty ? [] : out.components(separatedBy: "\n")
        let shown = all || lines.count <= outputLines ? out : lines.suffix(outputLines).joined(separator: "\n")
        var prompt = AttributedString("$ ")
        prompt.foregroundColor = Ink.s35
        var command = AttributedString(info.command)
        command.foregroundColor = Ink.full
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 2) {
                Text(w("cdx_shell")).font(.caption).foregroundStyle(Ink.s45)
                Spacer(minLength: 0)
                CopyButton(text: info.command, label: w("cdx_copy_cmd"))
                if !out.isEmpty { CopyButton(text: out, label: w("cdx_copy_output")) }
            }
            .padding(.leading, 10).padding(.trailing, 4).frame(height: 30)
            VStack(alignment: .leading, spacing: 4) {
                Text(prompt + command).font(.caption.monospaced()).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                if !all && lines.count > outputLines {
                    Button(w("cdx_more_lines", ["n": "\(lines.count - outputLines)"])) { all = true }
                        .font(.caption).foregroundStyle(Ink.s45).buttonStyle(.plain)
                }
                if !out.isEmpty {
                    Text(shown).font(.caption.monospaced()).foregroundStyle(Ink.s60).textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                } else if !tool.running {
                    Text(w("cdx_no_output")).font(.caption).foregroundStyle(Ink.s35)
                }
                if tool.truncated { Text(w("hc_cut")).font(.caption).foregroundStyle(Ink.s35) }
            }
            .padding(.horizontal, 10).padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 4) {
                Spacer(minLength: 0)
                footer
            }
            .font(.caption).padding(.horizontal, 10).padding(.top, 2).padding(.bottom, 7)
        }
        .background(Ink.ground, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Ink.line))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-shell")
    }

    @ViewBuilder private var footer: some View {
        if tool.running {
            Text(w("cdx_v_running")).foregroundStyle(Ink.s45).codexShimmer(true)
        } else if tool.stopped {
            Text(w("cdx_stopped")).foregroundStyle(Ink.s45)
        } else if info.exitCode == 0 {
            Image(systemName: "checkmark.circle").foregroundStyle(Ink.s45)
            Text(w("cdx_success")).foregroundStyle(Ink.s45)
        } else if let code = info.exitCode {
            Text(w("cdx_exit_code", ["n": "\(code)"])).foregroundStyle(Ink.del)
        } else if tool.status == "failed" {
            Text(w("hc_failed")).foregroundStyle(Ink.del)
        }
    }
}

/// Most diff lines drawn at once; the rest open on request.
private let diffLines = 400

/// A diff with its line numbers (old, new), a sign and the line; long lines wrap.
struct PhoneCodexDiffView: View {
    let lines: [PhoneDiffLine]
    /// Flush with its container (a changed file's row) rather than a card of its own.
    var flush = false
    @PhoneViewState private var all = false

    var body: some View {
        let shown = all ? lines : Array(lines.prefix(diffLines))
        // Hosts that sent only before / after texts give no line numbers: no columns for them.
        let numbered = lines.contains { $0.old != nil || $0.new != nil }
        let widest = String(lines.reduce(0) { max($0, $1.old ?? 0, $1.new ?? 0) })
        let digits = String(repeating: "8", count: max(2, widest.count))
        Group {
            if lines.isEmpty {
                Text(w("cdx_too_large")).font(.caption).foregroundStyle(Ink.s45).padding(.horizontal, 10).padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(shown.enumerated()), id: \.offset) { i, l in
                        if l.kind == .hunk {
                            // A hunk's header: the code it is in (as git names it), or a gap between two hunks.
                            let label = l.text.replacingOccurrences(of: "^@@[^@]*@@\\s?", with: "", options: .regularExpression)
                            if !(i == 0 && label.isEmpty) {
                                Text(label.isEmpty ? "⋯" : label).font(.caption2.monospaced()).foregroundStyle(Ink.s45).lineLimit(1)
                                    .padding(.horizontal, 10).padding(.vertical, 2)
                                    .frame(maxWidth: .infinity, alignment: .leading).background(Ink.hover)
                            }
                        } else {
                            row(l, numbered: numbered, digits: digits)
                        }
                    }
                    if !all && lines.count > diffLines {
                        Button(w("cdx_more_lines", ["n": "\(lines.count - diffLines)"])) { all = true }
                            .font(.caption).foregroundStyle(Ink.s45).buttonStyle(.plain)
                            .frame(maxWidth: .infinity).padding(.vertical, 4)
                    }
                }
                .padding(.vertical, 4)
            }
        }
        .background(flush ? Color.clear : Ink.ground)
        .clipShape(RoundedRectangle(cornerRadius: flush ? 0 : 10))
        .overlay { if !flush { RoundedRectangle(cornerRadius: 10).strokeBorder(Ink.line) } }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-diff")
    }

    private func row(_ l: PhoneDiffLine, numbered: Bool, digits: String) -> some View {
        let add = l.kind == .add, del = l.kind == .del
        return HStack(alignment: .firstTextBaseline, spacing: 0) {
            if numbered {
                number(l.old, digits).foregroundStyle(del ? Ink.del : Ink.s35)
                number(l.new, digits).foregroundStyle(add ? Ink.add : Ink.s35)
            }
            Text(add ? "+" : del ? "−" : " ").foregroundStyle(add ? Ink.add : del ? Ink.del : Ink.s35).frame(width: 16)
                .accessibilityHidden(true)
            Text(l.text.isEmpty ? " " : l.text).foregroundStyle(Color.primary.opacity(0.85))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.trailing, 8)
        }
        .font(.caption.monospaced())
        .padding(.leading, numbered ? 0 : 6)
        .padding(.vertical, 1)
        .background(add ? Ink.addBG : del ? Ink.delBG : Color.clear)
    }

    /// A line number, right-aligned in a column as wide as the largest one.
    private func number(_ n: Int?, _ digits: String) -> some View {
        ZStack(alignment: .trailing) {
            Text(digits).hidden()
            Text(n.map(String.init) ?? "")
        }
        .padding(.leading, 6).padding(.trailing, 2)
        .accessibilityHidden(true)
    }
}

private struct ToolDetails: View {
    let tool: PhoneChatTool
    var body: some View {
        let input = tool.input.map { $0.text }.flatMap { $0 == "{}" || $0 == "null" ? nil : $0 }
        let out = phoneToolOutput(tool).trimmingCharacters(in: .whitespacesAndNewlines)
        VStack(alignment: .leading, spacing: 6) {
            if let input { codeBox(input) }
            if !out.isEmpty { codeBox(out) }
        }
    }
    private func codeBox(_ text: String) -> some View {
        Text(text).font(.caption.monospaced()).foregroundStyle(Ink.s60).textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 10).padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Ink.ground, in: RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Ink.line))
    }
    static func has(_ x: PhoneChatTool) -> Bool {
        (x.input.map { $0.text != "{}" && $0.text != "null" } ?? false) || !phoneToolOutput(x).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

/// Preview and copy for one changed file (only once the edit is done).
private struct FileButtons: View {
    let f: PhoneFileChange
    let tool: PhoneChatTool
    let cwd: String?
    var body: some View {
        if tool.status != "failed" && !tool.running {
            HStack(spacing: 8) {
                if f.kind != "delete" { PhonePreviewButton(path: f.movePath ?? f.path) }
                if let patch = phoneFilePatch(f, cwd: cwd) { CopyButton(text: patch, label: w("cdx_copy_patch")) }
            }
        }
    }
}

/// One act as a line of its own (inside a group, or a group of one).
private struct ActRow: View {
    let act: PhoneCodexAct
    let cwd: String?
    var body: some View {
        switch act {
        case .exec(let x, let info):
            ActLine(label: actWords(act, cwd: cwd).text, live: x.running, opens: true, id: "codex-act-\(x.callID)", indent: 0) {
                ShellCard(info: info, tool: x)
            }
        case .patch(let x, let files):
            if files.count <= 1 {
                let f = files.first
                ActLine(label: actWords(act, cwd: cwd).text, live: x.running, opens: !(f?.lines.isEmpty ?? true), id: "codex-act-\(x.callID)") {
                    if let f { PhoneCodexDiffView(lines: f.lines) }
                } trailing: {
                    if let f { FileButtons(f: f, tool: x, cwd: cwd) }
                }
            } else {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(files, id: \.path) { f in
                        ActLine(label: fileWords(f, x).text, live: x.running, opens: !f.lines.isEmpty, id: "codex-act-\(x.callID)-\(phoneBaseName(f.path))") {
                            PhoneCodexDiffView(lines: f.lines)
                        } trailing: {
                            FileButtons(f: f, tool: x, cwd: cwd)
                        }
                    }
                }
            }
        case .web(let x, _), .tool(let x):
            ActLine(label: actWords(act, cwd: cwd).text, live: x.running, opens: ToolDetails.has(x), id: "codex-act-\(x.callID)") {
                ToolDetails(tool: x)
            }
        case .thought(let t):
            ActLine(label: actWords(act, cwd: cwd).text, quiet: true, opens: t.text.contains("\n") || t.text.count > 80, id: "codex-act-thought") {
                PhoneMarkdownText(text: t.text, font: .footnote, color: Ink.s45)
            }
        }
    }
}

// MARK: - Units

/// Tool calls in a row: one line naming what runs now, or what was done; opens to each call.
private struct GroupRow: View {
    let acts: [PhoneCodexAct]
    let live: Bool
    let cwd: String?
    var body: some View {
        let calls = acts.filter { $0.tool != nil }
        // A single call is its own line, live or done (the line does not change while it runs).
        if calls.count == 1, !isMultiFile(calls[0]) {
            ActRow(act: calls[0], cwd: cwd)
        } else {
            ActLine(label: label, live: live, opens: true, id: "codex-group-\(calls.first?.tool?.callID ?? "")") {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(acts) { a in ActRow(act: a, cwd: cwd) }
                }
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Ink.line).frame(width: 1) }
            }
        }
    }

    private func isMultiFile(_ a: PhoneCodexAct) -> Bool { if case .patch(_, let files) = a { files.count > 1 } else { false } }

    private var label: AttributedString {
        if !live {
            var words = Words()
            words.plain(phoneCodexSummarySentence(acts))
            let s = phoneGroupSummary(acts)
            words.stat(add: s.add, del: s.del)
            return words.text
        }
        // Live: the call running now; between calls, the reading the agent is in the middle of, or "Thinking".
        if let active = phoneActiveAct(acts), let x = active.tool {
            if x.running { return actWords(active, cwd: cwd).text }
            if case .exec(_, let info) = active, info.explore { return actWords(active, cwd: cwd).text }
        }
        return AttributedString(w("cdx_thinking"))
    }
}

private struct ThoughtRow: View {
    let thought: PhoneThought
    let live: Bool
    var body: some View {
        let took = thought.endedMs - thought.startedMs
        let label = live ? w("cdx_thinking") : took >= 1000 ? w("cdx_thought_for", ["t": phoneCodexDuration(took, zh: zh)]) : w("cdx_thought")
        ActLine(label: AttributedString(label), live: live, quiet: true, opens: !thought.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                id: "codex-thought") {
            PhoneMarkdownText(text: thought.text, font: .footnote, color: Ink.s45)
        }
    }
}

private struct AgentRow: View {
    let tool: PhoneChatTool
    var body: some View {
        let out = phoneToolOutput(tool).trimmingCharacters(in: .whitespacesAndNewlines)
        let prompt = tool.input?["prompt"].string
        var words = Words()
        words.verb(w("cdx_subagent"), failed: tool.status == "failed"); words.plain(" ")
        words.detail(tool.title.replacingOccurrences(of: "^agent\\s*", with: "", options: [.regularExpression, .caseInsensitive]))
        return ActLine(label: words.text, live: tool.running, opens: !out.isEmpty || prompt != nil, id: "codex-agent-\(tool.callID)") {
            VStack(alignment: .leading, spacing: 6) {
                if let prompt { PhoneMarkdownText(text: prompt, font: .footnote, color: Ink.s45) }
                if !out.isEmpty { PhoneMarkdownText(text: out, font: .subheadline) }
            }
        }
    }
}

private struct PlanCard: View {
    let entries: [PhonePlanEntry]
    let live: Bool
    /// Open while the turn runs, folded once it is done, unless the reader chose otherwise.
    @PhoneViewState private var chosen: Bool?
    var body: some View {
        let open = chosen ?? live
        let st = phonePlanStep(entries)
        VStack(alignment: .leading, spacing: 8) {
            Button { withAnimation(.easeOut(duration: 0.2)) { chosen = !open } } label: {
                HStack(spacing: 6) {
                    Image(systemName: "checklist").font(.footnote)
                    Text(w("cdx_plan")).font(.subheadline)
                    Text(st.done == st.total ? w("cdx_plan_done", ["d": "\(st.done)", "n": "\(st.total)"]) : w("cdx_plan_step", ["s": "\(st.step)", "n": "\(st.total)"]))
                        .font(.caption.monospacedDigit()).padding(.horizontal, 8).padding(.vertical, 2)
                        .background(Ink.hover, in: Capsule())
                    if !open, let current = st.current, st.done < st.total {
                        Text(current.content).font(.subheadline).foregroundStyle(Ink.s45).lineLimit(1)
                    }
                    Chevron(open: open)
                }
                .foregroundStyle(Ink.s60)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("codex-plan")
            if open {
                VStack(alignment: .leading, spacing: 7) {
                    ForEach(Array(entries.enumerated()), id: \.offset) { _, e in
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Image(systemName: e.status == "completed" ? "checkmark.circle" : e.status == "in_progress" ? "circle.dotted.circle" : "circle")
                                .font(.footnote)
                                .foregroundStyle(e.status == "completed" ? Ink.add : e.status == "in_progress" ? Ink.full : Ink.s35)
                            Text(e.content).font(.subheadline)
                                .strikethrough(e.status == "completed", color: Ink.s35)
                                .foregroundStyle(e.status == "completed" ? Ink.s45 : e.status == "in_progress" ? Ink.full : Ink.s60)
                        }
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Ink.ground, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Ink.line))
                .transition(.opacity)
            }
        }
    }
}

/// A line across with words in the middle ("You stopped after 12s", "Context automatically compacted").
private struct CodexDivider: View {
    let text: String
    var body: some View {
        HStack(spacing: 10) {
            Rectangle().fill(Ink.line).frame(height: 1)
            Text(text).font(.caption.monospacedDigit()).foregroundStyle(Ink.s45).fixedSize()
            Rectangle().fill(Ink.line).frame(height: 1)
        }
    }
}

private struct UnitView: View {
    let unit: PhoneCodexUnit
    let cwd: String?
    let readonly: Bool
    let answer: (String, String) -> Void
    var body: some View {
        switch unit {
        case .group(_, let acts, let live): GroupRow(acts: acts, live: live, cwd: cwd)
        case .message(_, let text, _): PhoneMarkdownText(text: text, color: Ink.prose)
        case .thought(_, let thought, let live): ThoughtRow(thought: thought, live: live)
        case .agent(_, let tool): AgentRow(tool: tool)
        case .question(_, let q): PhoneQuestionCard(question: q, readonly: readonly, answer: answer)
        case .notice(_, let level, let code, let text):
            if code == "compacted" { CodexDivider(text: w("cdx_compacted")) } else { PhoneNoticeRow(level: level, text: phoneNoticeText(code: code, text: text)) }
        case .steer(_, let text, let by, let ts, let extra): PhoneUserRow(text: text, by: by, ts: ts, extra: extra, bubble: Ink.bubble, radius: 16)
        }
    }
}

// MARK: - The changes a turn made

/// Files shown before "Show N more files".
private let filesShown = 6

private struct ChangesCard: View {
    let files: [PhoneFileChange]
    let cwd: String?
    @PhoneViewState private var more = false
    var body: some View {
        let add = files.reduce(0) { $0 + $1.add }, del = files.reduce(0) { $0 + $1.del }
        var title = Words()
        title.plain(phoneCodexPlural("cdx_files_changed", files.count))
        title.stat(add: add, del: del)
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "doc.badge.plus").font(.footnote).foregroundStyle(Ink.s60)
                Text(title.text).font(.subheadline.weight(.medium)).foregroundStyle(Ink.full)
                Spacer(minLength: 0)
                let patches = files.compactMap { phoneFilePatch($0, cwd: cwd) }
                if !patches.isEmpty { CopyButton(text: patches.joined(), label: w("cdx_copy_patch")) }
            }
            .padding(.leading, 12).padding(.trailing, 6).padding(.vertical, 8)
            ForEach(more ? files : Array(files.prefix(filesShown)), id: \.path) { f in
                Rectangle().fill(Ink.line).frame(height: 1)
                ChangedFile(f: f, cwd: cwd)
            }
            if !more && files.count > filesShown {
                Rectangle().fill(Ink.line).frame(height: 1)
                Button(phoneCodexPlural("cdx_show_more_files", files.count - filesShown)) { more = true }
                    .font(.footnote).foregroundStyle(Ink.s45).buttonStyle(.plain)
                    .padding(.horizontal, 12).padding(.vertical, 8)
            }
        }
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Ink.line))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-changes")
    }
}

private struct ChangedFile: View {
    let f: PhoneFileChange
    let cwd: String?
    @PhoneViewState private var open = false
    var body: some View {
        let rel = phoneRelPath(f.movePath ?? f.path, cwd)
        let name = phoneBaseName(rel)
        var path = AttributedString(String(rel.dropLast(name.count)))
        path.foregroundColor = Ink.s45
        var leaf = AttributedString(name)
        leaf.foregroundColor = Ink.full
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    guard !f.lines.isEmpty else { return }
                    withAnimation(.easeOut(duration: 0.2)) { open.toggle() }
                } label: {
                    HStack(spacing: 6) {
                        // A file without lines (an older host's edit) has nothing to open: no chevron, same indent.
                        Chevron(open: open).opacity(f.lines.isEmpty ? 0 : 1)
                        Text(path + leaf).font(.subheadline).lineLimit(1).truncationMode(.head)
                        Spacer(minLength: 4)
                        if f.kind == "add" { tag("A", Ink.add, Ink.addBG) }
                        if f.kind == "delete" { tag("D", Ink.del, Ink.delBG) }
                        Text(statText).font(.footnote.monospacedDigit())
                    }
                    .padding(.leading, 12).padding(.vertical, 9)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("codex-file-\(name)")
                if f.kind != "delete" { PhonePreviewButton(path: f.movePath ?? f.path) }
            }
            .padding(.trailing, 10)
            if open {
                Rectangle().fill(Ink.line).frame(height: 1)
                PhoneCodexDiffView(lines: f.lines, flush: true).transition(.opacity)
            }
        }
    }
    private var statText: AttributedString { var words = Words(); words.stat(add: f.add, del: f.del); return words.text }
    private func tag(_ s: String, _ fg: Color, _ bg: Color) -> some View {
        Text(s).font(.caption2.weight(.semibold)).foregroundStyle(fg).padding(.horizontal, 5).padding(.vertical, 1)
            .background(bg, in: RoundedRectangle(cornerRadius: 4))
    }
}

// MARK: - The turn

/// One Codex turn: the user's message, the plan, "Worked for …" over the work, the work, how it stands,
/// the answer, how it ended, the files it changed, and copying or continuing under it.
struct PhoneCodexTurnView<Files: View>: View {
    let turn: PhoneChatTurn
    let cwd: String?
    let readonly: Bool
    /// The last turn, stopped: it offers to continue.
    let stopped: Bool
    /// Stop was pressed and the turn has not ended yet.
    var stopping = false
    /// The work is open behind "Worked for …" (kept by the conversation so it survives scrolling).
    @Binding var opened: Bool
    let answer: (String, String) -> Void
    let resume: () -> Void
    /// What else shows under a finished turn (the files it wrote, as preview cards).
    @ViewBuilder let files: () -> Files

    var body: some View {
        let v = phoneCodexTurn(turn, cwd: cwd)
        let folded = v.foldable && !opened
        let running = turn.status == "running"
        VStack(alignment: .leading, spacing: 16) {
            if let text = turn.userText {
                PhoneUserRow(text: text, by: turn.userBy, ts: turn.userTs,
                             extra: PhoneUserExtra(inputID: turn.userInputID, attachments: turn.userAttachments), bubble: Ink.bubble, radius: 16)
            }
            if let plan = v.plan { PlanCard(entries: plan, live: running) }
            if v.foldable {
                VStack(alignment: .leading, spacing: 6) {
                    Button { withAnimation(.easeOut(duration: 0.22)) { opened.toggle() } } label: {
                        HStack(spacing: 5) {
                            elapsedText { w("cdx_worked_for", ["t": $0]) }
                                .font(.subheadline).foregroundStyle(opened ? Ink.full : Ink.s60)
                            Chevron(open: opened)
                        }
                        .padding(.vertical, 2)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("codex-worked")
                    .accessibilityValue(opened ? w("cdx_collapse") : w("cdx_expand"))
                    Rectangle().fill(Ink.line).frame(height: 1)
                }
            }
            // What the user added to the running turn stays in view when the work folds (as in Codex).
            if folded {
                ForEach(v.units.filter { $0.kind == "steer" }) { u in UnitView(unit: u, cwd: cwd, readonly: readonly, answer: answer) }
            } else {
                ForEach(v.units) { u in
                    UnitView(unit: u, cwd: cwd, readonly: readonly, answer: answer)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier("codex-unit-\(u.kind)")
                }
            }
            if v.thinking && !stopping {
                Text(w("cdx_thinking")).font(.subheadline).foregroundStyle(Ink.s60).codexShimmer(true)
                    .accessibilityIdentifier("codex-thinking")
            }
            if v.status == "working" && (stopping || v.answer == nil) {
                elapsedText { stopping ? w("hc_st_stopping") : v.asking ? w("cdx_waiting_answer") : w("cdx_working_for", ["t": $0]) }
                    .font(.footnote).foregroundStyle(Ink.s45)
                    .accessibilityIdentifier("codex-status")
            }
            if let a = v.answer {
                PhoneMarkdownText(text: a.text, color: Ink.prose)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("codex-answer")
            }
            if v.status == "stopped" {
                CodexDivider(text: w("cdx_stopped_after", ["t": phoneCodexDuration(phoneWorkedMs(turn, now: Date().timeIntervalSince1970 * 1000), zh: zh)]))
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("codex-stopped")
            }
            if v.status == "failed" {
                PhoneNoticeRow(level: "error", text: turn.reason.map { "\(w("hc_failed")): \($0)" } ?? w("hc_failed"))
            }
            if !v.changes.isEmpty { ChangesCard(files: v.changes, cwd: cwd) }
            if !running { files() }
            if !running { foot(v) }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-turn-\(turn.n)")
    }

    /// Words with how long the turn has worked, ticking while it runs.
    @ViewBuilder private func elapsedText(_ words: @escaping (String) -> String) -> some View {
        if turn.status == "running" {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let ms = phoneWorkedMs(turn, now: context.date.timeIntervalSince1970 * 1000)
                Text(ms >= 1000 || turn.status != "running" ? words(phoneCodexDuration(ms, zh: zh)) : w("cdx_working"))
                    .monospacedDigit()
            }
        } else {
            Text(words(phoneCodexDuration(phoneWorkedMs(turn, now: Date().timeIntervalSince1970 * 1000), zh: zh))).monospacedDigit()
        }
    }

    /// Under a finished turn: continuing a stopped one, or copying the answer and the turn's numbers.
    @ViewBuilder private func foot(_ v: PhoneCodexTurn) -> some View {
        if stopped && !readonly {
            HStack(spacing: 10) {
                Text(turn.reason.map { "\(w("hc_stopped_lede")) \($0)" } ?? w("hc_stopped_lede")).font(.footnote).foregroundStyle(Ink.s60)
                Spacer(minLength: 0)
                Button { resume() } label: { Label(w("hc_continue"), systemImage: "play.fill") }
                    .buttonStyle(.bordered).controlSize(.small).tint(.primary)
                    .accessibilityIdentifier("chat-continue")
            }
            .padding(10)
            .background(Ink.ground, in: RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Ink.line))
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("chat-stopped-\(turn.n)")
        } else {
            let text = v.answer?.text ?? ""
            let tokens = turn.usage?.output.map { (((turn.usage?.input ?? 0) + $0) / 100).rounded() / 10 }
            if !text.isEmpty || tokens != nil || (!v.foldable && v.status == "worked") {
                HStack(spacing: 8) {
                    if !text.isEmpty { CopyButton(text: text, label: w("cdx_copy_response")) }
                    if !v.foldable && v.status == "worked" {
                        Text(w("cdx_worked_for", ["t": phoneCodexDuration(phoneWorkedMs(turn, now: 0), zh: zh)])).monospacedDigit()
                    }
                    if let tokens { Text(w("hc_tokens", ["n": tokens == tokens.rounded() ? "\(Int(tokens))" : "\(tokens)"])).monospacedDigit() }
                }
                .font(.caption).foregroundStyle(Ink.s45)
                .padding(.top, -8)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("codex-foot-\(turn.n)")
            }
        }
    }
}
