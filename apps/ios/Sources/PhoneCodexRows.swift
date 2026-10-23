import SwiftUI
#if os(iOS)
import UIKit
#endif

// A Codex turn on the phone, laid out the way Codex's own app shows one (PhoneCodexModel.swift has the
// structure): the user's message on the right; "Working for …" over a hairline once the work starts,
// held as "Worked for …" once the answer starts and then the toggle that folds the work; the agent's
// work as quiet one-line units that open in place — a command opens to its shell output, an edit to
// its diff with line numbers — with what the agent said between them; a line saying what it is at
// while nothing else does (its reasoning's title, or "Thinking"); the answer; the files the turn
// changed ("Edited 3 files", Review opening every diff); copying the answer and when it came. The
// phone version of the desktop's apps/desktop/src/ui/codex/CodexTurn.tsx and styles/codex.css: the
// same measures (ink at 60 % for activity and 45 % for details, a faint ground for shells and diffs,
// green and red diff rows, Codex's terminal colors), with a tap where the desktop has a hover, so
// chevrons always show (dimmer until opened). The plan and the question card sit above the composer
// (PhoneCodexComposer.swift).

/// The view's words in the phone's language (PhoneCodexLang).
func phoneCodexWord(_ key: String, _ vars: [String: String] = [:]) -> String { phoneCodexText(key, vars) }
private func w(_ key: String, _ vars: [String: String] = [:]) -> String { phoneCodexText(key, vars) }

/// The desktop's codex.css colors: ink, its 60 / 45 / 35 % shades, hairlines, faint grounds, diff colors.
enum PhoneCodexInk {
    static let full = Color.primary
    static let s60 = Color.primary.opacity(0.6)
    static let s45 = Color.primary.opacity(0.45)
    static let s35 = Color.primary.opacity(0.35)
    static let s30 = Color.primary.opacity(0.3)
    static let prose = Color.primary.opacity(0.92)
    static let line = Color.primary.opacity(0.1)
    static let ground = Color.primary.opacity(0.035)
    static let hover = Color.primary.opacity(0.05)
    static let bubble = Color.primary.opacity(0.07)
    static let add = Color(red: 0.16, green: 0.62, blue: 0.33)
    static let del = Color(red: 0.86, green: 0.25, blue: 0.24)
    static let addBG = Color.green.opacity(0.14)
    static let delBG = Color.red.opacity(0.13)
    /// The page itself (the dot of a chosen option, the text of a filled button).
    #if os(iOS)
    static let canvas = Color(uiColor: .systemBackground)
    #else
    static let canvas = Color(nsColor: .windowBackgroundColor)
    #endif
    /// The plan ring's done part and a created file's dot (Codex's accent).
    static let accent = Color(red: 0x33 / 255, green: 0x9c / 255, blue: 0xff / 255)

    /// The eight terminal colors of a command's output (bright ones share them), dark and light.
    static func ansi(_ i: Int, dark: Bool) -> Color {
        let darkHex: [UInt32] = [0xffffff, 0xf67576, 0x85df7b, 0xfa994c, 0x3d8dff, 0xb06dff, 0x6dcbf4, 0x999999]
        let lightHex: [UInt32] = [0x000000, 0xd53538, 0x008809, 0xbd5800, 0x001bcb, 0x751ed9, 0x0071ea, 0x666666]
        let hex = (dark ? darkHex : lightHex)[max(0, min(7, i))]
        return Color(red: Double((hex >> 16) & 0xff) / 255, green: Double((hex >> 8) & 0xff) / 255, blue: Double(hex & 0xff) / 255)
    }

    /// A run's color: "c3" (a terminal color) or "rgb(r g b)".
    static func ansi(_ spec: String?, dark: Bool) -> Color? {
        guard let spec else { return nil }
        if spec.hasPrefix("c"), let i = Int(spec.dropFirst()) { return ansi(i, dark: dark) }
        let n = spec.replacingOccurrences(of: "[^0-9 ]", with: "", options: .regularExpression).split(separator: " ").compactMap { Double($0) }
        return n.count == 3 ? Color(red: n[0] / 255, green: n[1] / 255, blue: n[2] / 255) : nil
    }
}
private typealias Ink = PhoneCodexInk

// MARK: - Pieces

/// Text the agent is at now shimmers: a brighter band sweeps across it. `cadenced` (Codex's way for
/// what it is doing now): 600 ms after it shows and then every 4 s, one sweep of a second; otherwise
/// (a sub-agent at work, a shell still running) all the time. None with reduced motion.
private struct CodexShimmer: ViewModifier {
    let on: Bool
    var cadenced = true
    @Environment(\.accessibilityReduceMotion) private var reduce
    @PhoneViewState private var start = Date()
    func body(content: Content) -> some View {
        if on && !reduce {
            content.overlay {
                TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
                    let t = context.date.timeIntervalSince(start)
                    let phase: Double? = cadenced
                        ? (t < 0.6 ? nil : { let c = (t - 0.6).truncatingRemainder(dividingBy: 4); return c < 1 ? c : nil }())
                        : t.truncatingRemainder(dividingBy: 2.2) / 2.2
                    GeometryReader { g in
                        if let phase {
                            let band = max(48, g.size.width * 0.35)
                            LinearGradient(colors: [.clear, Color.primary.opacity(0.9), .clear], startPoint: .leading, endPoint: .trailing)
                                .frame(width: band)
                                .offset(x: -band + (g.size.width + band) * phase)
                        }
                    }
                }
                .mask(content)
                .allowsHitTesting(false)
                .accessibilityHidden(true)
            }
            .onAppear { start = Date() }
        } else {
            content
        }
    }
}

extension View {
    fileprivate func codexShimmer(_ on: Bool, cadenced: Bool = true) -> some View { modifier(CodexShimmer(on: on, cadenced: cadenced)) }
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
struct PhoneCodexCopyButton: View {
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
private typealias CopyButton = PhoneCodexCopyButton

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
    mutating func stat(add: Int, del: Int, lead: String = "  ") {
        if add > 0 {
            var x = AttributedString(lead + "+\(add)")
            x.font = Font.footnote.monospacedDigit(); x.foregroundColor = Ink.add
            text += x
        }
        if del > 0 {
            var x = AttributedString((add > 0 ? " " : lead) + "−\(del)")
            x.font = Font.footnote.monospacedDigit(); x.foregroundColor = Ink.del
            text += x
        }
    }
}

/// A one-line activity: its words, and (when it has any) details that open below it on a tap.
private struct ActLine<Details: View, Trailing: View>: View {
    let label: AttributedString
    /// An icon before the words (a finished group's: what it did first).
    var icon: String?
    var live = false
    /// Live, it shimmers all the time (a sub-agent at work) rather than now and then.
    var steady = false
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
                        if let icon { Image(systemName: icon).font(.footnote).foregroundStyle(open ? Ink.full : Ink.s60).accessibilityHidden(true) }
                        Text(label).font(.subheadline).lineLimit(1).truncationMode(.tail)
                            .foregroundStyle(open ? Ink.full : Ink.s60)
                            .codexShimmer(live, cadenced: !steady)
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
    init(label: AttributedString, icon: String? = nil, live: Bool = false, steady: Bool = false, opens: Bool, id: String, indent: CGFloat = 0,
         @ViewBuilder details: @escaping () -> Details) {
        self.init(label: label, icon: icon, live: live, steady: steady, opens: opens, id: id, indent: indent, details: details, trailing: { EmptyView() })
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
    if done, let took = phoneDurationA(info.durationMs) { words.detail(" " + w(x.stopped ? "cdx_after_time" : "cdx_in_time", ["t": took])) }
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
        if !query.isEmpty { words.detail(": " + phoneWebDetail(query)) }
        return words
    case .tool(let x):
        var words = Words()
        words.verb(w(running(x) ? "cdx_v_calling" : x.status == "failed" ? "cdx_v_failed" : "cdx_v_called"), failed: failed(x)); words.plain(" ")
        let title = x.title.trimmingCharacters(in: .whitespacesAndNewlines)
        words.code(title.isEmpty ? "tool" : title)
        return words
    }
}

// MARK: - Details: a command's shell, an edit's diff, a tool's input and output

#if os(iOS)
private let outputLine = UIFont.monospacedSystemFont(ofSize: 11, weight: .medium).lineHeight
#else
private let outputLine: CGFloat = 13.5
#endif
/// How tall a command's output box grows before it scrolls (as on the desktop).
private let outputMax: CGFloat = 144

/// A command's output in its terminal colors (ANSI escapes), as one attributed text.
private func ansiText(_ output: String, dark: Bool) -> AttributedString {
    var out = AttributedString()
    for run in phoneParseAnsi(output) {
        var x = AttributedString(run.text)
        if let fg = PhoneCodexInk.ansi(run.fg, dark: dark) { x.foregroundColor = run.dim ? fg.opacity(0.6) : fg }
        else if run.dim { x.foregroundColor = Ink.s60.opacity(0.6) }
        if let bg = PhoneCodexInk.ansi(run.bg, dark: dark) { x.backgroundColor = bg }
        if run.bold { x.font = Font.system(size: 11, weight: .bold, design: .monospaced) }
        if run.italic { x.font = (x.font ?? Font.system(size: 11, weight: .medium, design: .monospaced)).italic() }
        if run.underline { x.underlineStyle = .single }
        if run.strike { x.strikethroughStyle = .single }
        if run.hidden { x.foregroundColor = .clear }
        out += x
    }
    return out
}

/// A command's output: its end in view (the box starts scrolled to the bottom), the rest a scroll
/// away, long lines scrolling sideways; the top fades when there is more above.
private struct OutputBox: View {
    let text: String
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        let lines = CGFloat(text.components(separatedBy: "\n").count)
        let full = lines * outputLine + 4
        let height = min(outputMax, full)
        ScrollView([.horizontal, .vertical], showsIndicators: false) {
            Text(ansiText(text, dark: scheme == .dark))
                .font(.system(size: 11, weight: .medium, design: .monospaced))
                .foregroundStyle(Ink.s60)
                .fixedSize()
                .padding(.horizontal, 10).padding(.vertical, 2)
                .textSelection(.enabled)
                .accessibilityIdentifier("codex-shell-output")
        }
        .defaultScrollAnchor(.bottomLeading)
        .frame(height: height)
        .mask {
            if full > outputMax {
                LinearGradient(stops: [.init(color: .clear, location: 0), .init(color: .black, location: 24 / outputMax), .init(color: .black, location: 1)],
                               startPoint: .top, endPoint: .bottom)
            } else {
                Color.black
            }
        }
    }
}

/// A command, two lines at most until tapped.
private struct CommandLine: View {
    let command: String
    @PhoneViewState private var all = false
    var body: some View {
        let long = command.components(separatedBy: "\n").count > 2 || command.count > 160
        var prompt = AttributedString("$ ")
        prompt.foregroundColor = Ink.s35
        var text = AttributedString(command)
        text.foregroundColor = Color.primary.opacity(0.8)
        return Text(prompt + text).font(.footnote.monospaced())
            .lineLimit(long && !all ? 2 : nil)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .onTapGesture { if long { all.toggle() } }
            .accessibilityHint(long && !all ? w("cdx_show_all") : "")
    }
}

private struct ShellCard: View {
    let info: PhoneExecInfo
    let tool: PhoneChatTool

    var body: some View {
        var out = info.output
        while out.hasSuffix("\n") { out.removeLast() }
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 2) {
                Text(w("cdx_shell")).font(.caption).foregroundStyle(Ink.s45)
                Spacer(minLength: 0)
                CopyButton(text: info.command, label: w("cdx_copy_cmd"))
                if !out.isEmpty { CopyButton(text: out, label: w("cdx_copy_output")) }
            }
            .padding(.leading, 10).padding(.trailing, 4).frame(height: 30)
            VStack(alignment: .leading, spacing: 4) {
                CommandLine(command: info.command).padding(.horizontal, 10)
                if !out.isEmpty {
                    OutputBox(text: out)
                } else if !tool.running {
                    Text(w("cdx_no_output")).font(.caption).foregroundStyle(Ink.s35).padding(.horizontal, 10)
                }
                if tool.truncated { Text(w("hc_cut")).font(.caption).foregroundStyle(Ink.s35).padding(.horizontal, 10) }
            }
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            // How it ended, in the same quiet gray whatever it was; nothing yet while it runs.
            HStack(spacing: 4) {
                Spacer(minLength: 0)
                footer
            }
            .font(.caption).foregroundStyle(Ink.s45)
            .frame(minHeight: 14)
            .padding(.horizontal, 10).padding(.top, 2).padding(.bottom, 7)
        }
        .background(Ink.ground, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Ink.line))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-shell")
    }

    @ViewBuilder private var footer: some View {
        if tool.running {
            EmptyView()
        } else if tool.stopped {
            Text(w("cdx_stopped"))
        } else if info.exitCode == 0 {
            Image(systemName: "checkmark.circle").accessibilityHidden(true)
            Text(w("cdx_success"))
        } else if let code = info.exitCode {
            Text(w("cdx_exit_code", ["n": "\(code)"]))
        } else {
            Text(w("cdx_exit_unknown"))
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
        case .web(let x, let query):
            // A web search is one line that does not open; one without a query says nothing.
            if x.running || !query.trimmingCharacters(in: .whitespaces).isEmpty {
                ActLine(label: actWords(act, cwd: cwd).text, live: x.running, opens: false, id: "codex-act-\(x.callID)") { EmptyView() }
            }
        case .tool(let x):
            ActLine(label: actWords(act, cwd: cwd).text, live: x.running, opens: ToolDetails.has(x), id: "codex-act-\(x.callID)") {
                ToolDetails(tool: x)
            }
        }
    }
}

// MARK: - Units

/// What a finished group did first, as an icon before its sentence.
private func partIcon(_ part: String) -> String {
    switch part {
    case "tools": "powerplug"
    case "edited", "failed": "square.and.pencil"
    case "read": "doc.text"
    case "ran": "terminal"
    case "web": "globe"
    default: "checkmark.circle"
    }
}

/// Tool calls in a row: one line naming what runs now (or, between calls, what the agent is at), or
/// what was done; opens to each call. A live line holds each label for at least a second, so a quick
/// run of calls does not flicker past.
private struct GroupRow: View {
    let acts: [PhoneCodexAct]
    let live: Bool
    let cwd: String?
    /// The title of the agent's latest reasoning, said while no call runs.
    let title: String?
    @PhoneViewState private var shown: (key: String, label: AttributedString)?
    @PhoneViewState private var shownAt = Date.distantPast

    var body: some View {
        // A single call is its own line, live or done (the line does not change while it runs).
        if acts.count == 1, !isMultiFile(acts[0]) {
            ActRow(act: acts[0], cwd: cwd)
        } else {
            let now = liveLabel
            let parts = phoneGroupSummary(acts).parts
            ActLine(label: live ? (shown?.label ?? now.label) : AttributedString(phoneCodexSummarySentence(acts)),
                    icon: live ? nil : partIcon(parts.first?.part ?? ""), live: live, opens: true, id: "codex-group-\(acts.first?.tool.callID ?? "")") {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(acts) { a in ActRow(act: a, cwd: cwd) }
                }
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Ink.line).frame(width: 1) }
            }
            .task(id: live ? now.key : "") {
                guard live else { return }
                if let shown, shown.key != now.key {
                    let wait = shownAt.addingTimeInterval(1).timeIntervalSinceNow
                    if wait > 0 { try? await Task.sleep(for: .seconds(wait)) }
                    if Task.isCancelled { return }
                }
                shown = (now.key, liveLabel.label)
                shownAt = Date()
            }
        }
    }

    private func isMultiFile(_ a: PhoneCodexAct) -> Bool { if case .patch(_, let files) = a { files.count > 1 } else { false } }

    /// Live: the call running now; between calls, the reading the agent is in the middle of, its
    /// reasoning's title, or "Thinking".
    private var liveLabel: (key: String, label: AttributedString) {
        if let active = phoneActiveAct(acts) {
            let explore = { if case .exec(_, let info) = active { info.explore } else { false } }()
            if active.tool.running || explore { return ("a:\(active.key):\(active.tool.running)", actWords(active, cwd: cwd).text) }
        }
        if let title { return ("t:\(title)", AttributedString(title)) }
        return ("thinking", AttributedString(w("cdx_thinking")))
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
        return ActLine(label: words.text, live: tool.running, steady: true, opens: !out.isEmpty || prompt != nil, id: "codex-agent-\(tool.callID)") {
            VStack(alignment: .leading, spacing: 6) {
                if let prompt { PhoneMarkdownText(text: prompt, font: .footnote, color: Ink.s45) }
                if !out.isEmpty { PhoneMarkdownText(text: out, font: .subheadline) }
            }
        }
    }
}

/// Questions the agent asked (one request), once answered or closed: "Asked 2 questions", opening to
/// each with its answer.
private struct AskedRow: View {
    let questions: [PhoneQuestion]
    var body: some View {
        var words = Words()
        words.verb(w("cdx_q_asked")); words.plain(" ")
        words.detail(phoneCodexPlural("cdx_q_n", questions.count))
        return ActLine(label: words.text, icon: "bubble.left.and.text.bubble.right", opens: true, id: "codex-asked-\(questions.first?.request ?? questions.first?.qid ?? "")") {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(questions, id: \.qid) { q in
                    let given = q.answers ?? (q.answer.map { $0.isEmpty ? [] : [$0] } ?? [])
                    VStack(alignment: .leading, spacing: 4) {
                        Text(q.prompt).foregroundStyle(Ink.s60)
                        Text(given.isEmpty ? w("cdx_q_no_answer") : q.secret ? "••••••" : given.joined(separator: ", ")).foregroundStyle(Ink.s30)
                    }
                    .font(.footnote)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("codex-asked-\(q.qid)")
                }
            }
        }
    }
}

private struct UnitView: View {
    let unit: PhoneCodexUnit
    let cwd: String?
    let title: String?
    var body: some View {
        switch unit {
        case .group(_, let acts, let live): GroupRow(acts: acts, live: live, cwd: cwd, title: title)
        case .message(_, let text, _): PhoneMarkdownText(text: text, color: Ink.prose)
        case .agent(_, let tool): AgentRow(tool: tool)
        case .asked(_, let questions): AskedRow(questions: questions)
        case .notice(_, let level, let code, let text):
            if code == "compacted" {
                Label(w("cdx_compacted"), systemImage: "arrow.down.right.and.arrow.up.left").font(.footnote).foregroundStyle(Ink.s45)
            } else {
                PhoneNoticeRow(level: level, text: phoneNoticeText(code: code, text: text))
            }
        case .steer(_, let text, let by, let ts, let extra): PhoneUserRow(text: text, by: by, ts: ts, extra: extra, bubble: Ink.bubble, radius: 16)
        }
    }
}

// MARK: - The changes a turn made

/// Files listed before "Show N more files".
private let filesShown = 3
/// A diff longer than this is not drawn inline.
private let inlineLines = 5000

/// The files the turn changed, as Codex sums them up under its answer: "Edited 3 files" with their
/// counts beside a diff icon, Review opening every diff at once; with several files a short list of
/// them (three, then the rest on request), each opening to its own diff.
private struct ChangesCard: View {
    let files: [PhoneFileChange]
    let cwd: String?
    @PhoneViewState private var more = false
    @PhoneViewState private var review = false
    var body: some View {
        let add = files.reduce(0) { $0 + $1.add }, del = files.reduce(0) { $0 + $1.del }
        let single = files.count == 1 ? files[0] : nil
        let title = single.map { w("cdx_edited_file", ["name": phoneBaseName($0.movePath ?? $0.path)]) } ?? w("cdx_edited_files", ["n": "\(files.count)"])
        var stat = Words()
        stat.stat(add: add, del: del, lead: "")
        let patches = files.compactMap { phoneFilePatch($0, cwd: cwd) }
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Button { withAnimation(.easeOut(duration: 0.2)) { review.toggle() } } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "plus.forwardslash.minus").font(.body).foregroundStyle(Ink.s60)
                            .frame(width: 40, height: 40)
                            .background(Color.primary.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
                        VStack(alignment: .leading, spacing: 1) {
                            Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Ink.full).lineLimit(1)
                            if add > 0 || del > 0 { Text(stat.text).font(.footnote) }
                        }
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("codex-changes-title")
                if !patches.isEmpty { CopyButton(text: patches.joined(), label: w("cdx_copy_patch")) }
                Button(w("cdx_review")) { withAnimation(.easeOut(duration: 0.2)) { review.toggle() } }
                    .font(.footnote).foregroundStyle(Ink.full).buttonStyle(.plain)
                    .padding(.horizontal, 12).frame(height: 28)
                    .overlay(Capsule().strokeBorder(Color.primary.opacity(0.16)))
                    .accessibilityIdentifier("codex-review")
                    .accessibilityValue(review ? w("cdx_collapse") : w("cdx_expand"))
            }
            .padding(.leading, 8).padding(.trailing, 10).padding(.vertical, 8)
            if let single {
                if review {
                    Rectangle().fill(Ink.line).frame(height: 1)
                    PhoneCodexDiffView(lines: single.lines, flush: true).transition(.opacity)
                }
            } else {
                ForEach(more ? files : Array(files.prefix(filesShown)), id: \.path) { f in
                    Rectangle().fill(Ink.line).frame(height: 1)
                    ChangedFile(f: f, cwd: cwd, all: review)
                }
                if files.count > filesShown {
                    Rectangle().fill(Ink.line).frame(height: 1)
                    Button { withAnimation(.easeOut(duration: 0.2)) { more.toggle() } } label: {
                        HStack(spacing: 4) {
                            Text(more ? w("cdx_collapse_files") : phoneCodexPlural("cdx_show_more_files", files.count - filesShown))
                            Image(systemName: "chevron.right").font(.caption2.weight(.semibold)).rotationEffect(.degrees(more ? -90 : 90))
                        }
                        .font(.footnote).foregroundStyle(Ink.s45)
                        .padding(.horizontal, 12).padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("codex-more-files")
                }
            }
        }
        // On iOS 26.2, accessibility snapshots could loop in the enclosing LazyVStack's layout
        // with this expanding subtree clipped. Keep the border without clipping the whole card.
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Ink.line))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("codex-changes")
    }
}

private struct ChangedFile: View {
    let f: PhoneFileChange
    let cwd: String?
    /// Review opened (or closed) every file.
    let all: Bool
    /// Opened or closed by hand, until Review opens or closes them all.
    @PhoneViewState private var own: Bool?
    var body: some View {
        let rel = phoneRelPath(f.movePath ?? f.path, cwd)
        let name = phoneBaseName(rel)
        var path = AttributedString(String(rel.dropLast(name.count)))
        path.foregroundColor = Ink.s45
        var leaf = AttributedString(name)
        leaf.foregroundColor = Ink.full
        let big = f.lines.count > inlineLines
        let can = !f.lines.isEmpty && !big
        let open = can && (own ?? all)
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    guard can else { return }
                    withAnimation(.easeOut(duration: 0.2)) { own = !open }
                } label: {
                    HStack(spacing: 6) {
                        // A file without lines (an older host's edit) has nothing to open: no chevron, same indent.
                        Chevron(open: open).opacity(can ? 1 : 0)
                        Text(path + leaf).font(.footnote).lineLimit(1).truncationMode(.head)
                        Spacer(minLength: 4)
                        if big { Text(w("cdx_too_large_inline")).font(.caption).foregroundStyle(Ink.s45) }
                        Text(statText).font(.footnote.monospacedDigit())
                        if f.kind == "add" { Circle().fill(Ink.accent.opacity(0.7)).frame(width: 6, height: 6).accessibilityLabel(w("cdx_v_created")) }
                        if f.kind == "delete" { Circle().fill(Ink.del.opacity(0.7)).frame(width: 6, height: 6).accessibilityLabel(w("cdx_v_deleted")) }
                    }
                    .padding(.leading, 12).frame(minHeight: 36)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("codex-file-\(name)")
                if f.kind != "delete" { PhonePreviewButton(path: f.movePath ?? f.path) }
            }
            .padding(.trailing, 10)
            .background(Ink.ground)
            if open {
                Rectangle().fill(Ink.line).frame(height: 1)
                PhoneCodexDiffView(lines: f.lines, flush: true).transition(.opacity)
            }
        }
        .onChange(of: all) { _, _ in own = nil }
    }
    private var statText: AttributedString { var words = Words(); words.stat(add: f.add, del: f.del, lead: ""); return words.text }
}

// MARK: - The turn

/// "Working for 12s" over a hairline once the work starts, held still once the answer starts ("Worked
/// for 1m 23s", then the toggle that folds the work); a stopped or failed turn says after how long.
private struct TimerDivider: View {
    let turn: PhoneChatTurn
    let v: PhoneCodexTurn
    let stopping: Bool
    @Binding var opened: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if v.foldable {
                Button { withAnimation(.easeOut(duration: 0.22)) { opened.toggle() } } label: {
                    HStack(spacing: 4) {
                        Text(label(now: Date())).monospacedDigit()
                        Chevron(open: opened)
                    }
                    .font(.footnote).foregroundStyle(opened ? Ink.full : Ink.s60)
                    .padding(.vertical, 2)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("codex-worked")
                .accessibilityValue(opened ? w("cdx_collapse") : w("cdx_expand"))
            } else if turn.status == "running" && v.answer == nil && !stopping {
                // Ticking while it works.
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    Text(label(now: context.date)).monospacedDigit()
                        .font(.footnote).foregroundStyle(Ink.s60)
                        .accessibilityIdentifier("codex-worked")
                }
            } else {
                Text(label(now: Date())).monospacedDigit()
                    .font(.footnote).foregroundStyle(Ink.s60)
                    .accessibilityIdentifier("codex-worked")
            }
            Rectangle().fill(Ink.line).frame(height: 1)
        }
    }

    private func label(now: Date) -> String {
        let ms = phoneWorkedMs(turn, now: now.timeIntervalSince1970 * 1000)
        let t = phoneDurationB(ms)
        if stopping { return w("hc_st_stopping") }
        switch v.status {
        case "stopped": return w("cdx_stopped_after", ["t": t])
        case "failed": return w("cdx_failed_after", ["t": t])
        case "worked": return w("cdx_worked_for", ["t": t])
        default: return v.answer != nil ? w("cdx_worked_for", ["t": t]) : ms >= 1000 ? w("cdx_working_for", ["t": t]) : w("cdx_working")
        }
    }
}

/// One Codex turn: the user's message, "Working for …" over the work, the work, what the agent is at,
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
    let resume: () -> Void
    /// What else shows under a finished turn (the files it wrote, as preview cards).
    @ViewBuilder let files: () -> Files

    var body: some View {
        let v = phoneCodexTurn(turn, cwd: cwd)
        let folded = v.foldable && !opened
        let running = turn.status == "running"
        let divider = !v.units.isEmpty || v.status == "stopped"
        VStack(alignment: .leading, spacing: 16) {
            if let text = turn.userText {
                PhoneUserRow(text: text, by: turn.userBy, ts: turn.userTs,
                             extra: PhoneUserExtra(inputID: turn.userInputID, attachments: turn.userAttachments), bubble: Ink.bubble, radius: 16)
            }
            if divider { TimerDivider(turn: turn, v: v, stopping: stopping, opened: $opened) }
            // What the user added to the running turn stays in view when the work folds (as in Codex).
            if folded {
                ForEach(v.units.filter { $0.kind == "steer" }) { u in UnitView(unit: u, cwd: cwd, title: v.title) }
            } else {
                ForEach(v.units) { u in
                    UnitView(unit: u, cwd: cwd, title: v.title)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier("codex-unit-\(u.kind)")
                }
            }
            if running { statusLine(v, divider: divider) }
            if let a = v.answer {
                PhoneMarkdownText(text: a.text, color: Ink.prose)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("codex-answer")
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

    /// At the turn's end while it runs: waiting for an answer, stopping, or what the agent is at
    /// (its reasoning's title, or "Thinking") when nothing else shows it working.
    @ViewBuilder private func statusLine(_ v: PhoneCodexTurn, divider: Bool) -> some View {
        if v.asking {
            Label(w("cdx_waiting_answer"), systemImage: "bubble.left.and.exclamationmark.bubble.right")
                .font(.subheadline).foregroundStyle(Ink.s60)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("codex-status")
        } else if stopping {
            if !divider { Text(w("hc_st_stopping")).font(.subheadline).foregroundStyle(Ink.s60).accessibilityIdentifier("codex-status") }
        } else if v.thinking {
            let words = v.title ?? w("cdx_thinking")
            Text(words).font(.subheadline).foregroundStyle(Ink.s60).lineLimit(1)
                .codexShimmer(true)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(words)
                .accessibilityIdentifier("codex-thinking")
        }
    }

    /// Under a finished turn: continuing a stopped one, or copying the answer and when it came.
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
        } else if let a = v.answer, !a.text.isEmpty {
            HStack(spacing: 6) {
                CopyButton(text: a.text, label: w("cdx_copy_response"))
                if let started = a.startedMs {
                    Text(phoneFormatMsgTime(started, now: Date().timeIntervalSince1970 * 1000)).monospacedDigit()
                }
            }
            .font(.caption).foregroundStyle(Ink.s45)
            .padding(.top, -8)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("codex-foot-\(turn.n)")
        }
    }
}
