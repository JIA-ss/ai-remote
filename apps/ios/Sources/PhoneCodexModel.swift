import Foundation

// How a Codex conversation is shown on the phone: the way Codex's own app lays out a turn. A port of
// the desktop's apps/desktop/src/lib/codex.ts (same algorithms, same names where Swift allows); keep the
// two in step. Pure, so it runs and is tested on macOS (tools/test-phone-chat-model.sh); the views are
// in PhoneCodexRows.swift.
//
// A turn there is: the user's message on the right; "Working for …" over a hairline once the work
// starts; the agent's work as a short list of one-line units — consecutive tool calls folded into one
// line that names what is happening now ("Running `npm test`", "Reading app.ts") and, once done, what
// was done ("Edited files, read files, ran commands"), with the agent's remarks between them; a line
// saying what it is at while nothing else does (its reasoning's title, or "Thinking"); then, once the
// answer starts, all of that folds behind "Worked for 1m 23s", the answer follows, and the files the
// turn changed close it. Reasoning is not a step, the plan shows above the composer, and questions are
// asked in a card in place of the composer, leaving one "Asked 2 questions" line once answered. Ported
// up to the desktop's feat/codex-chat-view d937c69. The layout was studied from Codex's app; the code
// and wording here are this project's own.

/// Whether a session's agent is Codex: by its protocol, or (hosts that do not say) its id or name.
public func phoneIsCodexRuntime(id: String?, name: String?, protocolName: String?) -> Bool {
    if let protocolName, !protocolName.isEmpty { return protocolName == "codex-app-server" }
    return id == "codex" || phoneMatches(name ?? "", "\\bcodex\\b", caseInsensitive: true)
}

// MARK: - Small regex helpers (Foundation's ICU regex, so the same on macOS and iOS)

func phoneMatches(_ s: String, _ pattern: String, caseInsensitive: Bool = false) -> Bool {
    s.range(of: pattern, options: caseInsensitive ? [.regularExpression, .caseInsensitive] : .regularExpression) != nil
}

/// The first match of `pattern` in `s`: its range and its capture groups (nil where a group did not take part).
func phoneFirstMatch(_ s: String, _ pattern: String) -> (range: Range<String.Index>, groups: [String?])? {
    guard let re = try? NSRegularExpression(pattern: pattern),
          let m = re.firstMatch(in: s, range: NSRange(s.startIndex..., in: s)),
          let whole = Range(m.range, in: s) else { return nil }
    let groups = (1..<max(1, m.numberOfRanges)).map { i -> String? in Range(m.range(at: i), in: s).map { String(s[$0]) } }
    return (whole, groups)
}

private func trimmed(_ s: String) -> String { s.trimmingCharacters(in: .whitespacesAndNewlines) }

// MARK: - Commands

/// What a command does, as Codex reads it: reads a file, lists files, searches, or something else.
public enum PhoneCmdAction: Equatable, Sendable {
    case read(path: String, name: String)
    case listFiles(path: String?)
    case search(query: String?, path: String?)
    case unknown
}

public struct PhoneExecInfo: Equatable, Sendable {
    /// The command as typed, without the shell Codex runs it in.
    public let command: String
    public let cwd: String?
    public let actions: [PhoneCmdAction]
    /// Only reads, searches and listings: shown as "Read app.ts", not as a command.
    public let explore: Bool
    public let exitCode: Int?
    public let durationMs: Double?
    /// The output, without the "(exit code N)" line older hosts add.
    public let output: String
}

/// Splits a command line into words the way a POSIX shell quotes them.
public func phoneSplitWords(_ s: String) -> [String] {
    let c = Array(s)
    var out: [String] = []
    var cur = ""
    var started = false
    var i = 0
    while i < c.count {
        let ch = c[i]
        if ch == "'" {
            started = true
            if let end = c[(i + 1)...].firstIndex(of: "'") {
                cur += String(c[(i + 1)..<end])
                i = end
            } else {
                cur += String(c[(i + 1)...])
                i = c.count
            }
        } else if ch == "\"" {
            started = true
            i += 1
            while i < c.count && c[i] != "\"" {
                if c[i] == "\\", i + 1 < c.count, "\"\\$`".contains(c[i + 1]) { i += 1 }
                cur.append(c[i])
                i += 1
            }
        } else if ch == "\\", i + 1 < c.count, c[i + 1].isWhitespace || "'\"\\".contains(c[i + 1]) {
            started = true
            i += 1
            cur.append(c[i])
        } else if ch.isWhitespace {
            if started { out.append(cur) }
            cur = ""
            started = false
        } else {
            started = true
            cur.append(ch)
        }
        i += 1
    }
    if started { out.append(cur) }
    return out
}

/// The program a word names, lowercased, without its folder and `.exe`.
private func program(_ w: String) -> String {
    var p = (w.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? w).lowercased()
    if p.hasSuffix(".exe") { p.removeLast(4) }
    return p
}

/// A command without the shell around it (`/bin/zsh -lc 'ls -la'` → `ls -la`); hosts send it unwrapped.
public func phoneUnwrapShell(_ raw: String) -> String {
    let words = phoneSplitWords(trimmed(raw))
    guard !words.isEmpty else { return raw }
    let p = program(words[0])
    if ["bash", "zsh", "sh", "dash", "fish", "ksh"].contains(p) {
        var at = 1
        while at < words.count && words[at].hasPrefix("-") { at += 1 }
        if words[1..<at].contains(where: { $0.contains("c") }) && words.count == at + 1 { return trimmed(words[at]) }
    } else if p == "powershell" || p == "pwsh" {
        if let at = words.firstIndex(where: { ["-command", "-c"].contains($0.lowercased()) }), at > 0, at + 1 < words.count {
            return trimmed(words[(at + 1)...].joined(separator: " "))
        }
    } else if p == "cmd" {
        if let at = words.firstIndex(where: { $0.lowercased() == "/c" }), at > 0, at + 1 < words.count {
            return trimmed(words[(at + 1)...].joined(separator: " "))
        }
    }
    return trimmed(raw)
}

/// A command line's commands (split at `&&`, `||`, `;` and newlines outside quotes), each as its pipeline's
/// stages. Nil when the line does what a plain reading cannot follow: a redirection (`>`, `<`, `2>&1`),
/// a command substitution, a background job. Such a line shows as a command, never as a read.
private func commandList(_ cmd: String) -> [[String]]? {
    let c = Array(cmd)
    var out: [[String]] = [[]]
    var cur = ""
    var quote: Character?
    func end(_ next: Bool) {
        out[out.count - 1].append(trimmed(cur))
        cur = ""
        if next { out.append([]) }
    }
    var i = 0
    while i < c.count {
        let ch = c[i]
        let next: Character? = i + 1 < c.count ? c[i + 1] : nil
        if let q = quote {
            if q == "\"" && (ch == "`" || (ch == "$" && next == "(")) { return nil }
            cur.append(ch)
            if ch == "\\" && q == "\"" && i + 1 < c.count {
                i += 1
                cur.append(c[i])
            } else if ch == q {
                quote = nil
            }
            i += 1
            continue
        }
        if ch == "'" || ch == "\"" {
            quote = ch
            cur.append(ch)
        } else if ch == "`" || (ch == "$" && next == "(") || ch == ">" || ch == "<" {
            return nil
        } else if (ch == "&" && next == "&") || (ch == "|" && next == "|") {
            end(true)
            i += 1
        } else if ch == "&" {
            return nil
        } else if ch == ";" || ch.isNewline {
            end(true)
        } else if ch == "|" {
            end(false)
        } else {
            cur.append(ch)
        }
        i += 1
    }
    end(false)
    return out.map { $0.filter { !$0.isEmpty } }.filter { !$0.isEmpty }
}

/// Commands that only reshape what comes down a pipe (and so may follow a read or a search).
private let pipeFilters: Set<String> = ["head", "tail", "wc", "sort", "uniq", "nl", "cut", "tr", "column", "fold", "fmt", "grep", "egrep", "fgrep",
    "rg", "sed", "jq", "cat", "less", "more", "select-object", "select", "select-string", "sort-object", "measure-object", "format-table",
    "out-string", "ft", "sls"]

/// Options of search tools that take a value (so the value is not the pattern).
private let searchValueFlags: Set<String> = ["-g", "--glob", "-t", "--type", "-T", "--type-not", "-m", "--max-count", "-A", "-B", "-C",
    "--context", "--after-context", "--before-context", "-f", "--file", "--max-depth", "-d", "--include", "--exclude",
    "--exclude-dir", "--color", "--colors", "-j", "--threads", "-M", "--max-columns", "--sort", "--sortr", "--type-add", "-e", "--regexp"]

private func positional(_ words: [String], _ valueFlags: Set<String> = []) -> [String] {
    var out: [String] = []
    var i = 1
    while i < words.count {
        let w = words[i]
        if w == "--" {
            out += words[(i + 1)...]
            break
        }
        if w.hasPrefix("-") && w.count > 1 {
            if valueFlags.contains(w) { i += 1 }
            i += 1
            continue
        }
        out.append(w)
        i += 1
    }
    return out
}

/// PowerShell's parameters (`-Path x`, case-insensitive) and the words that are not parameters.
private let psSwitches: Set<String> = ["-recurse", "-force", "-raw", "-wait", "-casesensitive", "-simplematch", "-list", "-name", "-file",
    "-directory", "-hidden", "-quiet", "-notmatch", "-allmatches", "-asbytestream"]

private enum PSValue: Equatable { case value(String), flag }

private func psArgs(_ words: [String]) -> (params: [String: PSValue], rest: [String]) {
    var params: [String: PSValue] = [:]
    var rest: [String] = []
    let isParam = { (w: String) in phoneMatches(w, "^-[A-Za-z]") }
    var i = 1
    while i < words.count {
        let w = words[i]
        if isParam(w) {
            let name = w.lowercased()
            if !psSwitches.contains(name), i + 1 < words.count, !isParam(words[i + 1]) {
                params[name] = .value(words[i + 1])
                i += 1
            } else {
                params[name] = .flag
            }
        } else {
            rest.append(w)
        }
        i += 1
    }
    return (params, rest)
}

/// The first of `keys` given (its value, or nil when it came without one), else `fallback` — JS's `a ?? b ?? c`.
private func psPick(_ params: [String: PSValue], _ keys: [String], _ fallback: String?) -> String? {
    for k in keys { if let v = params[k] { if case .value(let s) = v { return s }; return nil } }
    return fallback
}

/// Whether a command writes, deletes or runs something else although its program usually only reads.
private func changesThings(_ p: String, _ words: [String]) -> Bool {
    switch p {
    case "sed": return words.contains { phoneMatches($0, "^-[a-zA-Z]*i") || $0.hasPrefix("--in-place") }
    case "find": return words.contains { ["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls"].contains($0) }
    case "fd", "fdfind": return words.contains { ["-x", "-X", "--exec", "--exec-batch"].contains($0) }
    case "rg": return words.contains { $0 == "--pre" || $0.hasPrefix("--pre=") }
    default: return false
    }
}

private let readers: Set<String> = ["cat", "bat", "batcat", "less", "more", "nl"]

/// What one command does; nil when it does nothing worth a word (echo, cd).
private func actionOf(_ words: [String]) -> [PhoneCmdAction]? {
    let p = program(words.first ?? "")
    if changesThings(p, words) { return [.unknown] }
    guard let one = actionOfOne(words) else { return nil }
    guard case .read = one else { return [one] }
    // `cat a b` reads both.
    if readers.contains(p) {
        let files = positional(words)
        if files.count > 1 { return files.map { .read(path: $0, name: phoneBaseName($0)) } }
    }
    return [one]
}

private func read(_ path: String) -> PhoneCmdAction { .read(path: path, name: phoneBaseName(path)) }

private func actionOfOne(_ words: [String]) -> PhoneCmdAction? {
    let p = program(words.first ?? "")
    let args = positional(words)
    let last = args.last
    switch p {
    case "echo", "printf", "true", "pwd", ":", "cd":
        return nil
    case "cat", "bat", "batcat", "less", "more", "nl", "type":
        return last.map(read) ?? .unknown
    case "head", "tail":
        let files = positional(words, ["-n", "-c", "--lines", "--bytes"])
        return files.last.map(read) ?? .unknown
    case "get-content", "gc":
        let ps = psArgs(words)
        return psPick(ps.params, ["-path", "-literalpath"], ps.rest.first).map(read) ?? .unknown
    case "sed":
        // `sed -n '1,80p' file` prints part of a file.
        guard words.contains("-n"), args.count >= 2, phoneMatches(args[0], "^\\d*,?\\d*p$"), let last else { return .unknown }
        return read(last)
    case "rg", "grep", "egrep", "fgrep", "ag", "ack":
        let pos = positional(words, searchValueFlags)
        if words.contains("--files") { return .listFiles(path: pos.first) }
        if let e = words.firstIndex(where: { $0 == "-e" || $0 == "--regexp" }), e > 0 {
            return .search(query: e + 1 < words.count ? words[e + 1] : nil, path: pos.first)
        }
        return .search(query: pos.first, path: pos.count > 1 ? pos[1] : nil)
    case "select-string", "sls":
        let ps = psArgs(words)
        let query = psPick(ps.params, ["-pattern"], ps.rest.first)
        let path = psPick(ps.params, ["-path", "-literalpath"], ps.params["-pattern"] != nil ? ps.rest.first : ps.rest.count > 1 ? ps.rest[1] : nil)
        return .search(query: query, path: path)
    case "git":
        if words.count > 1, words[1] == "grep" {
            let pos = positional(Array(words.dropFirst()), searchValueFlags)
            return .search(query: pos.first, path: pos.count > 1 ? pos[1] : nil)
        }
        if words.count > 1, words[1] == "ls-files" { return .listFiles(path: positional(Array(words.dropFirst())).first) }
        return .unknown
    case "fd", "fdfind":
        let pos = positional(words, searchValueFlags)
        return pos.isEmpty ? .listFiles(path: nil) : .search(query: pos[0], path: pos.count > 1 ? pos[1] : nil)
    case "find":
        let root = words.count > 1 && !words[1].hasPrefix("-") ? words[1] : nil
        if let name = words.firstIndex(where: { $0 == "-name" || $0 == "-iname" || $0 == "-path" }), name > 0 {
            return .search(query: name + 1 < words.count ? words[name + 1] : nil, path: root)
        }
        return .listFiles(path: root)
    case "ls", "dir":
        return .listFiles(path: last)
    case "get-childitem", "gci":
        let ps = psArgs(words)
        return .listFiles(path: psPick(ps.params, ["-path", "-literalpath"], ps.rest.first))
    case "tree", "eza", "exa":
        return .listFiles(path: last)
    default:
        return .unknown
    }
}

/// Codex's reading of a command where the host did not send one: reads, searches and listings, else unknown.
public func phoneParseCommand(_ cmd: String) -> [PhoneCmdAction] {
    guard let commands = commandList(cmd) else { return [.unknown] }
    var out: [PhoneCmdAction] = []
    for stages in commands {
        // What comes after the first command of a pipeline may only reshape its output.
        for stage in stages.dropFirst() {
            let w = phoneSplitWords(stage)
            let p = program(w.first ?? "")
            if !pipeFilters.contains(p) || changesThings(p, w) { return [.unknown] }
        }
        guard let a = actionOf(phoneSplitWords(stages[0])) else { continue }
        out += a
    }
    if out.isEmpty || out.contains(.unknown) { return [.unknown] }
    return out
}

private func hostActions(_ v: JSONValue) -> [PhoneCmdAction]? {
    guard case .array(let list) = v else { return nil }
    return list.map { a in
        func s(_ k: String) -> String? { if let x = a[k].string, !x.isEmpty { x } else { nil } }
        switch a["type"].string {
        case "read": return .read(path: s("path") ?? s("name") ?? "", name: s("name") ?? phoneBaseName(s("path") ?? ""))
        case "listFiles", "list_files": return .listFiles(path: s("path"))
        case "search": return .search(query: s("query"), path: s("path"))
        default: return .unknown
        }
    }
}

/// A shell tool call as Codex shows it: the command, what it does, how it ended.
public func phoneExecInfo(_ x: PhoneChatTool) -> PhoneExecInfo {
    let input = x.input ?? .null
    var fromTitle = x.title
    if let r = fromTitle.range(of: "^Shell\\s+", options: .regularExpression) { fromTitle.removeSubrange(r) }
    let command = input["command"].string ?? phoneUnwrapShell(fromTitle)
    var actions = hostActions(input["actions"]) ?? phoneParseCommand(command)
    if actions.isEmpty { actions = [.unknown] }
    let explore = !actions.contains(.unknown)
    // Codex streams a command's output in pieces (each is the next bytes, not a line of its own).
    var output = x.content.compactMap { if case .text(let t) = $0 { t } else { nil } }.joined()
    var exitCode = x.exitCode
    if let m = phoneFirstMatch(output, "\\n?\\(exit code (-?\\d+)\\)\\s*$") {
        let code = m.groups.first.flatMap { $0 }.flatMap { Int($0) }
        output = String(output[..<m.range.lowerBound])
        if exitCode == nil { exitCode = code }
    }
    // Completed without a code: it succeeded, unless it was only closed by the end of its turn.
    if exitCode == nil && x.status == "completed" && !x.stopped && !x.endedByTurn { exitCode = 0 }
    let durationMs = x.durationMs ?? x.endedMs.map { $0 - x.startedMs }
    return PhoneExecInfo(command: command, cwd: input["cwd"].string, actions: actions, explore: explore, exitCode: exitCode,
                         durationMs: durationMs, output: output)
}

/// A path as Codex shows it: relative to the folder the agent works in when it is inside it.
public func phoneRelPath(_ path: String, _ cwd: String?) -> String {
    guard let cwd, !cwd.isEmpty, !path.isEmpty else { return path }
    func norm(_ p: String) -> String {
        var s = p.replacingOccurrences(of: "\\", with: "/")
        while s.hasSuffix("/") { s.removeLast() }
        return s
    }
    let p = norm(path), base = norm(cwd)
    if p == base { return "." }
    if p.lowercased().hasPrefix(base.lowercased() + "/") { return String(p.dropFirst(base.count + 1)) }
    return path
}

// MARK: - File changes

public struct PhoneDiffLine: Equatable, Sendable {
    public enum Kind: String, Sendable { case hunk, context, add, del }
    public let kind: Kind
    public let text: String
    public let old: Int?
    public let new: Int?
    public init(_ kind: Kind, _ text: String, old: Int? = nil, new: Int? = nil) {
        self.kind = kind; self.text = text; self.old = old; self.new = new
    }
}

public struct PhoneFileChange: Equatable, Sendable {
    public var path: String
    public var movePath: String?
    /// add, delete or update.
    public var kind: String
    public var add: Int
    public var del: Int
    public var lines: [PhoneDiffLine]
    /// The host cut its diff short (a very large change).
    public var truncated = false
    /// Several edits of the file put together: their hunks count from different versions of it.
    public var merged = false
}

/// Whether a "diff" is a unified diff, rather than the content Codex sends for an added or deleted file.
private func looksLikeDiff(_ diff: String) -> Bool {
    phoneMatches(diff.replacingOccurrences(of: "^\\s*\\n", with: "", options: .regularExpression), "^(@@ -\\d|--- |diff --git )")
}

/// The lines of a unified diff with their line numbers (a new file's "diff" may be its content).
public func phoneParseUnifiedDiff(_ diff: String, kind: String = "update") -> (lines: [PhoneDiffLine], add: Int, del: Int) {
    var raw = diff.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
    if raw.last == "" { raw.removeLast() }
    var lines: [PhoneDiffLine] = []
    var add = 0, del = 0
    if kind != "update" ? !looksLikeDiff(diff) : !raw.contains(where: { phoneMatches($0, "^@@ -\\d") }) {
        // No hunks: an added file comes as its content, a deleted one as what it held.
        for (i, text) in raw.enumerated() {
            if kind == "delete" {
                lines.append(PhoneDiffLine(.del, text, old: i + 1)); del += 1
            } else if kind == "add" {
                lines.append(PhoneDiffLine(.add, text, new: i + 1)); add += 1
            }
        }
        return (lines, add, del)
    }
    var o = 0, n = 0
    var inHunk = false
    for l in raw {
        if l.hasPrefix("@@"), let h = phoneFirstMatch(l, "^@@ -(\\d+)(?:,\\d+)? \\+(\\d+)(?:,\\d+)? @@(.*)$") {
            o = Int(h.groups[0] ?? "") ?? 0
            n = Int(h.groups[1] ?? "") ?? 0
            inHunk = true
            lines.append(PhoneDiffLine(.hunk, l))
            continue
        }
        if !inHunk || l.hasPrefix("\\") { continue }
        if l.hasPrefix("+") {
            lines.append(PhoneDiffLine(.add, String(l.dropFirst()), new: n)); n += 1; add += 1
        } else if l.hasPrefix("-") {
            lines.append(PhoneDiffLine(.del, String(l.dropFirst()), old: o)); o += 1; del += 1
        } else {
            lines.append(PhoneDiffLine(.context, String(l.dropFirst()), old: o, new: n)); o += 1; n += 1
        }
    }
    return (lines, add, del)
}

/// The files an edit changed, one entry per file (hosts that send only per-hunk diffs are merged per path).
public func phoneFileChanges(_ x: PhoneChatTool) -> [PhoneFileChange] {
    let files = (x.input ?? .null)["files"].array
    if !files.isEmpty {
        let fromContent = contentFiles(x)
        return files.map { f in
            let kind = f["kind"].string == "add" || f["kind"].string == "delete" ? f["kind"].string! : "update"
            let path = f["path"].string ?? ""
            let movePath = f["move_path"].string
            let diff = f["diff"].string ?? ""
            let truncated = f["truncated"] == .bool(true)
            // Past the host's budget a file comes without its diff: what the call's content has of it.
            if diff.isEmpty, truncated, var fallback = fromContent.first(where: { $0.path == path }) {
                fallback.kind = kind; fallback.movePath = movePath; fallback.truncated = true
                return fallback
            }
            let parsed = phoneParseUnifiedDiff(diff, kind: kind)
            return PhoneFileChange(path: path, movePath: movePath, kind: kind, add: parsed.add, del: parsed.del, lines: parsed.lines, truncated: truncated)
        }
    }
    return contentFiles(x)
}

/// The files of an edit from its per-hunk diffs (older hosts), merged per path, without line numbers.
private func contentFiles(_ x: PhoneChatTool) -> [PhoneFileChange] {
    var out: [PhoneFileChange] = []
    // A text's last newline is not a line of its own.
    func trim(_ t: String) -> String { t.hasSuffix("\n") ? String(t.dropLast()) : t }
    for c in x.content {
        guard case .diff(let path, let old, let new) = c else { continue }
        let at: Int
        if let i = out.firstIndex(where: { $0.path == path }) {
            at = i
            if !out[i].lines.isEmpty { out[i].lines.append(PhoneDiffLine(.hunk, "")) }
        } else {
            out.append(PhoneFileChange(path: path, movePath: nil, kind: old == nil ? "add" : new.isEmpty ? "delete" : "update", add: 0, del: 0, lines: []))
            at = out.count - 1
        }
        for r in phoneDiffRows(old.map(trim), trim(new)) {
            switch r {
            case .gap: out[at].lines.append(PhoneDiffLine(.hunk, ""))
            case .context(let t): out[at].lines.append(PhoneDiffLine(.context, t))
            case .add(let t): out[at].lines.append(PhoneDiffLine(.add, t)); out[at].add += 1
            case .del(let t): out[at].lines.append(PhoneDiffLine(.del, t)); out[at].del += 1
            }
        }
    }
    if out.isEmpty {
        for p in x.locations { out.append(PhoneFileChange(path: p, movePath: nil, kind: "update", add: 0, del: 0, lines: [])) }
    }
    return out
}

/// A path joined to the folder it is relative to (a path that is absolute already stays as it is).
public func phoneJoinPath(_ cwd: String?, _ rel: String) -> String {
    guard let cwd, !cwd.isEmpty, !phoneMatches(rel, "^([A-Za-z]:[\\\\/]|[\\\\/])") else { return rel }
    let sep = cwd.contains("\\") && !cwd.contains("/") ? "\\" : "/"
    var base = cwd
    while base.hasSuffix("/") || base.hasSuffix("\\") { base.removeLast() }
    return base + sep + (sep == "\\" ? rel.replacingOccurrences(of: "/", with: "\\") : rel)
}

/// The files of a whole-turn diff (`diff --git a/x b/x` sections, as git and Codex write them).
public func phoneParseTurnDiff(_ text: String, cwd: String? = nil) -> [PhoneFileChange] {
    var out: [PhoneFileChange] = []
    var sections: [[String]] = []
    for l in text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n") {
        if l.hasPrefix("diff --git ") { sections.append([l]) } else if !sections.isEmpty { sections[sections.count - 1].append(l) }
    }
    for lines in sections {
        let named = phoneFirstMatch(lines[0], "^diff --git a/(.+?) b/(.+)$")?.groups ?? [nil, nil]
        var from = named[0], to = named[1]
        var kind = "update"
        let at = lines.firstIndex { $0.hasPrefix("@@") }
        for l in lines[1..<(at ?? lines.count)] {
            if l.hasPrefix("new file mode") { kind = "add" }
            else if l.hasPrefix("deleted file mode") { kind = "delete" }
            else if l.hasPrefix("rename from ") { from = String(l.dropFirst(12)) }
            else if l.hasPrefix("rename to ") { to = String(l.dropFirst(10)) }
            else if l == "--- /dev/null" { kind = "add" }
            else if l == "+++ /dev/null" { kind = "delete" }
        }
        if from == nil && to == nil { continue }
        let body = at.map { lines[$0...].joined(separator: "\n") } ?? ""
        let parsed = body.isEmpty ? (lines: [PhoneDiffLine](), add: 0, del: 0) : phoneParseUnifiedDiff(body, kind: "update")
        let path = phoneJoinPath(cwd, (kind == "add" ? to : from) ?? to ?? "")
        let moved: String? = if kind == "update", let from, let to, from != to { phoneJoinPath(cwd, to) } else { nil }
        out.append(PhoneFileChange(path: path, movePath: moved, kind: kind, add: parsed.add, del: parsed.del, lines: parsed.lines))
    }
    return out
}

/// The files a whole turn changed: Codex's own diff of the turn where the host passes it on (each file
/// once, as its net change), otherwise each edit's files merged per path in the order first changed.
public func phoneTurnChanges(_ turn: PhoneChatTurn, cwd: String? = nil) -> [PhoneFileChange] {
    if let diff = turn.diff, !diff.truncated {
        let fromDiff = phoneParseTurnDiff(diff.text, cwd: cwd)
        if !fromDiff.isEmpty { return fromDiff }
    }
    var out: [PhoneFileChange] = []
    for b in turn.blocks {
        guard case .tool(let x) = b, phoneCodexIsEdit(x), x.status != "failed" else { continue }
        for f in phoneFileChanges(x) {
            guard let i = out.firstIndex(where: { $0.path == f.path }) else {
                out.append(f)
                continue
            }
            // Created then deleted in the same turn: nothing is left of it.
            if out[i].kind == "add" && f.kind == "delete" {
                out.remove(at: i)
                continue
            }
            out[i].add += f.add
            out[i].del += f.del
            out[i].lines += f.lines
            out[i].movePath = f.movePath ?? out[i].movePath
            // Deleted then written again: it changed; deleted at the end: it is gone. An added file stays added.
            out[i].kind = out[i].kind == "delete" && f.kind == "add" ? "update" : f.kind == "delete" ? "delete" : out[i].kind
            if f.truncated { out[i].truncated = true }
            // The edits' hunks count from different versions of the file: shown in turn, never a patch.
            out[i].merged = true
            // A file the turn created has, in the end, the lines it was created with and those added since.
            if out[i].kind == "add" {
                out[i].add -= f.del
                out[i].del = 0
            }
        }
    }
    return out
}

/// A file's change as a patch git applies (`git apply`), or nil when it cannot be one: an older host's
/// diffs carry no line numbers.
public func phoneFilePatch(_ f: PhoneFileChange, cwd: String?) -> String? {
    let changed = f.lines.filter { $0.kind != .hunk }
    if changed.isEmpty || f.truncated || f.merged || changed.contains(where: { $0.old == nil && $0.new == nil }) { return nil }
    func rel(_ p: String) -> String {
        phoneRelPath(p, cwd).replacingOccurrences(of: "\\", with: "/").replacingOccurrences(of: "^/+", with: "", options: .regularExpression)
    }
    let a = rel(f.path), b = rel(f.movePath ?? f.path)
    var head = ["diff --git a/\(a) b/\(b)"]
    if f.kind == "add" { head += ["new file mode 100644", "--- /dev/null", "+++ b/\(b)"] }
    else if f.kind == "delete" { head += ["deleted file mode 100644", "--- a/\(a)", "+++ /dev/null"] }
    else { head += ["--- a/\(a)", "+++ b/\(b)"] }
    var body: [String] = []
    // The content of an added or deleted file comes without a hunk header of its own.
    if !f.lines.contains(where: { $0.kind == .hunk && phoneMatches($0.text, "^@@ -\\d") }) {
        body.append(f.kind == "delete" ? "@@ -1,\(changed.count) +0,0 @@" : "@@ -0,0 +1,\(changed.count) @@")
    }
    for l in f.lines {
        switch l.kind {
        case .hunk: body.append(l.text)
        case .add: body.append("+" + l.text)
        case .del: body.append("-" + l.text)
        case .context: body.append(" " + l.text)
        }
    }
    return (head + body).joined(separator: "\n") + "\n"
}

/// An edit (Codex's file changes, or any tool that came with a diff).
public func phoneCodexIsEdit(_ x: PhoneChatTool) -> Bool {
    x.kind == "edit" || x.kind == "delete" || x.kind == "move" || x.content.contains { if case .diff = $0 { true } else { false } }
}

// MARK: - The units of a turn

/// One tool call of the agent's work.
public enum PhoneCodexAct: Equatable, Sendable, Identifiable {
    case exec(PhoneChatTool, PhoneExecInfo)
    case patch(PhoneChatTool, [PhoneFileChange])
    case web(PhoneChatTool, query: String)
    case tool(PhoneChatTool)

    public var id: String { key }
    public var key: String { "k:\(tool.callID)" }
    public var tool: PhoneChatTool {
        switch self {
        case .exec(let x, _), .patch(let x, _), .web(let x, _), .tool(let x): x
        }
    }
}

public enum PhoneCodexUnit: Equatable, Sendable, Identifiable {
    /// Tool calls in a row, shown as one line.
    case group(key: String, acts: [PhoneCodexAct], live: Bool)
    /// What the agent said between steps.
    case message(key: String, text: String, streaming: Bool)
    /// A sub-agent the agent started.
    case agent(key: String, PhoneChatTool)
    /// Questions the agent asked (one request), once they are answered or closed.
    case asked(key: String, [PhoneQuestion])
    case notice(key: String, level: String, code: String, text: String)
    /// A message the user added to the running turn.
    case steer(key: String, text: String, by: String, ts: Double, extra: PhoneUserExtra)

    public var id: String { key }
    public var key: String {
        switch self {
        case .group(let k, _, _), .message(let k, _, _), .agent(let k, _), .asked(let k, _), .notice(let k, _, _, _), .steer(let k, _, _, _, _): k
        }
    }
    public var kind: String {
        switch self {
        case .group: "group"
        case .message: "message"
        case .agent: "agent"
        case .asked: "asked"
        case .notice: "notice"
        case .steer: "steer"
        }
    }
}

/// The agent's answer to the turn, shown after the work.
public struct PhoneCodexAnswer: Equatable, Sendable {
    public let id: String
    public let text: String
    public let streaming: Bool
    /// When it started (a message folded before hosts said so has none).
    public var startedMs: Double?
}

public struct PhoneCodexTurn: Equatable, Sendable {
    public var units: [PhoneCodexUnit]
    public var answer: PhoneCodexAnswer?
    /// The work folds behind "Worked for …" (the answer started and the turn was not stopped).
    public var foldable: Bool
    /// working, worked, stopped or failed.
    public var status: String
    /// The agent is thinking now (nothing else shows that it is working).
    public var thinking: Bool
    /// The title of the agent's latest reasoning (its summary's last line), said while it works.
    public var title: String?
    /// A question waits for the user's answer.
    public var asking: Bool
    /// The turn's plan as last updated (shown above the composer, not as a step of the work).
    public var plan: [PhonePlanEntry]?
    public var changes: [PhoneFileChange]
}

/// A sub-agent call (Codex's collaboration tools).
public func phoneCodexIsAgent(_ x: PhoneChatTool) -> Bool {
    x.kind == "think" && phoneMatches(x.title, "^(agent|Task|Agent)\\b")
}

private func actOf(_ x: PhoneChatTool) -> PhoneCodexAct {
    if x.kind == "execute" { return .exec(x, phoneExecInfo(x)) }
    if phoneCodexIsEdit(x) { return .patch(x, phoneFileChanges(x)) }
    if x.kind == "fetch", phoneMatches(x.title, "^search\\b") {
        var fromTitle = x.title
        if let r = fromTitle.range(of: "^search\\s*", options: .regularExpression) { fromTitle.removeSubrange(r) }
        return .web(x, query: x.input?["query"].string ?? fromTitle)
    }
    return .tool(x)
}

/// The block that is the turn's answer, if it has started.
private func answerIndex(_ turn: PhoneChatTurn) -> Int? {
    let b = turn.blocks
    var last = b.count - 1
    // Reasoning, notices and a plan after the answer do not make it less of an answer.
    func trailing(_ x: PhoneChatBlock) -> Bool {
        switch x {
        case .thought, .notice, .plan: true
        default: false
        }
    }
    while last >= 0, trailing(b[last]) { last -= 1 }
    guard last >= 0, case .text(_, let text, let phase, _) = b[last], !trimmed(text).isEmpty else { return nil }
    if phase == "final_answer" { return last }
    if phase == "commentary" { return nil }
    // A host that does not say: the last text of a turn that completed, after all its work (a turn that
    // failed or was stopped has no answer, only what it said on the way).
    let phased = b.contains { if case .text(_, _, let p, _) = $0 { p != nil } else { false } }
    return !phased && turn.status == "completed" ? last : nil
}

/// The units of a turn, its answer, and how its work is summed up (`cwd`: what the turn's diff is relative to).
public func phoneCodexTurn(_ turn: PhoneChatTurn, cwd: String? = nil) -> PhoneCodexTurn {
    let live = turn.status == "running"
    let b = turn.blocks
    let ans = answerIndex(turn)
    let n = turn.n
    var units: [PhoneCodexUnit] = []
    // The open group, as an index into units.
    var group: Int?
    var plan: [PhonePlanEntry]?
    var title: String?
    for i in b.indices {
        if i == ans { continue }
        let x = b[i]
        switch x {
        case .plan(let entries):
            // A plan update is not a step: it neither shows in the work nor splits a group.
            plan = entries
            continue
        case .thought(let thought):
            // Reasoning is not a step either (as in Codex's app): only its title says what the agent is at.
            title = phoneReasoningTitle(thought.text) ?? title
            continue
        case .tool(let tool) where !phoneCodexIsAgent(tool):
            if group == nil {
                units.append(.group(key: "g\(n):\(tool.callID)", acts: [], live: false))
                group = units.count - 1
            }
            if let g = group, case .group(let key, var acts, let l) = units[g] {
                acts.append(actOf(tool))
                units[g] = .group(key: key, acts: acts, live: l)
            }
            continue
        case .question(let q) where live && !q.answered:
            // A question still waiting is asked in its own card, in place of the composer.
            continue
        default:
            break
        }
        group = nil
        switch x {
        case .text(let id, let text, _, _):
            if !trimmed(text).isEmpty { units.append(.message(key: "m\(n):\(id)", text: text, streaming: false)) }
        case .tool(let tool):
            units.append(.agent(key: "a\(n):\(tool.callID)", tool))
        case .question(let q):
            // One line per request (an older host does not say: questions in a row are one request).
            let same: Int? = if let request = q.request {
                units.firstIndex { if case .asked(_, let qs) = $0 { qs.first?.request == request } else { false } }
            } else if let last = units.indices.last, case .asked(_, let qs) = units[last], qs.first?.request == nil {
                last
            } else {
                nil
            }
            if let same, case .asked(let k, var qs) = units[same] {
                qs.append(q)
                units[same] = .asked(key: k, qs)
            } else {
                units.append(.asked(key: "q\(n):\(q.request ?? q.qid)", [q]))
            }
        case .notice(let level, let code, let text):
            units.append(.notice(key: "n\(n):\(i)", level: level, code: code, text: text))
        case .user(let text, let by, let ts):
            units.append(.steer(key: "s\(n):\(i)", text: text, by: by, ts: ts,
                                extra: PhoneUserExtra(inputID: turn.steerInputs[i], attachments: turn.steerAttachments[i] ?? [])))
        case .thought, .plan: break
        }
    }
    let lastIsText = { if case .text = b.last { true } else { false } }()
    let asking = live && b.contains { if case .question(let q) = $0 { !q.answered } else { false } }
    var thinking = false
    if live {
        for u in units.indices {
            if case .group(let k, let acts, _) = units[u] {
                units[u] = .group(key: k, acts: acts, live: acts.contains { $0.tool.running })
            }
        }
        // The last group stays the live line until something else follows it (the next call may come).
        if let last = units.indices.last, ans == nil {
            switch units[last] {
            case .group(let k, let acts, _): units[last] = .group(key: k, acts: acts, live: true)
            case .message(let k, let text, _) where lastIsText: units[last] = .message(key: k, text: text, streaming: true)
            default: break
            }
        }
        // Nothing on screen says the agent is busy: it is thinking.
        let busy = units.contains { u in
            switch u {
            case .group(_, let acts, _): acts.contains { $0.tool.running }
            case .message(_, _, let streaming): streaming
            case .agent(_, let tool): tool.running
            default: false
            }
        } || (ans != nil && ans == b.count - 1)
        thinking = !busy && !asking
    }
    var answer: PhoneCodexAnswer?
    if let ans, case .text(let id, let text, _, let started) = b[ans] {
        answer = PhoneCodexAnswer(id: id, text: text, streaming: live && ans == b.count - 1, startedMs: started)
    }
    let status = live ? "working" : turn.status == "interrupted" ? "stopped" : turn.status == "failed" ? "failed" : "worked"
    return PhoneCodexTurn(
        units: units,
        answer: answer,
        // A compaction alone is not work worth folding away.
        foldable: answer != nil && turn.status != "interrupted"
            && units.contains { if case .notice(_, _, let code, _) = $0 { code != "compacted" } else { true } },
        status: status,
        thinking: thinking,
        title: live ? title : nil,
        asking: asking,
        plan: plan,
        changes: live ? [] : phoneTurnChanges(turn, cwd: cwd))
}

/// What a group did, by kind, in the order Codex names them: tools, edits, reads, commands, web searches,
/// and last the edits that failed.
public struct PhoneSummaryPart: Equatable, Sendable {
    /// tools, edited, read, ran, web or failed.
    public let part: String
    public let n: Int
}

/// A command that only fetches a page off the internet (`curl https://…`), which Codex counts as a web search.
public func phoneIsWebFetch(_ info: PhoneExecInfo) -> Bool {
    // One plain curl that only reads: no pipe, chain or redirection, nothing written, sent or run.
    guard let list = commandList(info.command), list.count == 1, list[0].count == 1 else { return false }
    let words = phoneSplitWords(list[0][0])
    guard program(words.first ?? "") == "curl" else { return false }
    let writes = { (w: String) in
        phoneMatches(w, "^--(request|data|json|form|upload|output|remote-name|remote-header-name|config|create-dirs)") || phoneMatches(w, "^-[A-Za-z]*[XdFToOKJ]")
    }
    guard !words.dropFirst().contains(where: writes),
          let url = words.first(where: { phoneMatches($0, "^https?://", caseInsensitive: true) }) else { return false }
    let host = url.replacingOccurrences(of: "^https?://(\\[[^\\]]*\\]|[^/:?#]*).*$", with: "$1", options: [.regularExpression, .caseInsensitive]).lowercased()
    return !phoneMatches(host, "^(localhost|127\\.\\d+\\.\\d+\\.\\d+|\\[::1\\]|0\\.0\\.0\\.0)$") && !host.hasSuffix(".localhost")
}

/// A web search's words for its line: the query, with the sites it is limited to after a bar.
public func phoneWebDetail(_ query: String) -> String {
    var sites: [String] = []
    var rest = ""
    let ns = query as NSString
    var at = 0
    if let re = try? NSRegularExpression(pattern: "\\bsite:(\\S+)", options: [.caseInsensitive]) {
        for m in re.matches(in: query, range: NSRange(location: 0, length: ns.length)) {
            rest += ns.substring(with: NSRange(location: at, length: m.range.location - at)) + " "
            sites.append(ns.substring(with: m.range(at: 1)).replacingOccurrences(of: "^www\\.", with: "", options: [.regularExpression, .caseInsensitive]))
            at = m.range.location + m.range.length
        }
    }
    rest += ns.substring(from: at)
    rest = trimmed(rest.replacingOccurrences(of: "\\s+OR\\s+", with: " ", options: .regularExpression)
        .replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression))
    return sites.isEmpty ? rest : trimmed("\(rest) | \(sites.joined(separator: " · "))")
}

public func phoneGroupSummary(_ acts: [PhoneCodexAct]) -> (parts: [PhoneSummaryPart], add: Int, del: Int) {
    var edited: [String] = [], failed: [String] = [], read: [String] = []
    var explored = 0, ran = 0, web = 0, tools = 0, add = 0, del = 0
    for a in acts {
        switch a {
        case .patch(let x, let files):
            if x.status == "failed" && !x.stopped {
                for f in files where !failed.contains(f.path) { failed.append(f.path) }
                continue
            }
            for f in files {
                if !edited.contains(f.path) { edited.append(f.path) }
                add += f.add
                del += f.del
            }
        case .exec(let x, let info):
            if phoneIsWebFetch(info) && (x.running || info.exitCode == 0) {
                web += 1
            } else if info.explore {
                explored += 1
                for x in info.actions { if case .read(let path, _) = x, !read.contains(path) { read.append(path) } }
            } else {
                ran += 1
            }
        case .web: web += 1
        case .tool: tools += 1
        }
    }
    var parts: [PhoneSummaryPart] = []
    if tools > 0 { parts.append(PhoneSummaryPart(part: "tools", n: tools)) }
    if !edited.isEmpty { parts.append(PhoneSummaryPart(part: "edited", n: edited.count)) }
    if explored > 0 { parts.append(PhoneSummaryPart(part: "read", n: max(read.count, 1))) }
    if ran > 0 { parts.append(PhoneSummaryPart(part: "ran", n: ran)) }
    if web > 0 { parts.append(PhoneSummaryPart(part: "web", n: web)) }
    // Edits that did not apply are said last, so a group of only those still has words.
    if !failed.isEmpty { parts.append(PhoneSummaryPart(part: "failed", n: failed.count)) }
    return (parts, add, del)
}

/// The act a live group's line names: the one running now, else the last one.
public func phoneActiveAct(_ acts: [PhoneCodexAct]) -> PhoneCodexAct? {
    acts.last(where: { $0.tool.running }) ?? acts.last
}

/// How long a turn worked: up to the moment its answer started (the answer is not work), else to its
/// end, or to now while it runs.
public func phoneWorkedMs(_ turn: PhoneChatTurn, now: Double) -> Double {
    let start = turn.startedMs ?? (turn.userText != nil ? turn.userTs : now)
    // A turn stopped or failed while answering says how long it ran in all.
    if let ans = answerIndex(turn), case .text(_, _, _, let answered?) = turn.blocks[ans], turn.status == "completed" || turn.status == "running" {
        return max(0, answered - start)
    }
    if let d = turn.durationMs, turn.status != "running" { return d }
    return max(0, now - start)
}

/// Inline Markdown as the plain text it shows (`code`, *emphasis* and links lose their marks).
func phoneInlineText(_ s: String) -> String {
    (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))).map { String($0.characters) } ?? s
}

/// The title reasoning gives itself: its summary's last line (bold, or as plain text), without comments.
public func phoneReasoningTitle(_ text: String) -> String? {
    let bare = text.replacingOccurrences(of: "<!--[\\s\\S]*?(-->|$)", with: "", options: .regularExpression)
    guard let line = bare.components(separatedBy: "\n").map(trimmed).last(where: { !$0.isEmpty }) else { return nil }
    let title = trimmed(phoneFirstMatch(line, "^\\*\\*(.+)\\*\\*$")?.groups.first.flatMap { $0 } ?? phoneInlineText(line))
    return title.isEmpty ? nil : title
}

/// A duration in units of days, hours, minutes and seconds, each as the language writes it ("1m", "1分钟").
private func durationParts(_ ms: Double, _ lang: PhoneCodexLang) -> [(n: Int, text: String)] {
    let locale = Locale(identifier: lang == .zh ? "zh-CN" : "en")
    var s = Int(max(0, (ms / 1000).rounded(.down)))
    let units: [(Duration.UnitsFormatStyle.Unit, Int)] = [(.days, 86_400), (.hours, 3_600), (.minutes, 60), (.seconds, 1)]
    return units.map { unit, size in
        let n = s / size
        s -= n * size
        let style = Duration.UnitsFormatStyle(allowedUnits: [unit], width: .narrow, zeroValueUnits: .show(length: 1)).locale(locale)
        return (n, Duration.seconds(n * size).formatted(style))
    }
}

/// A command's run time, as its line says it ("12s", "1m 0s", "1h 0m 3s"): every unit from the largest
/// one on; nil under a second.
public func phoneDurationA(_ ms: Double?, lang: PhoneCodexLang = .current) -> String? {
    guard let ms, ms >= 1000 else { return nil }
    let p = durationParts(ms, lang)
    let first = p.firstIndex { $0.n > 0 } ?? p.count - 1
    return p[first...].map(\.text).joined(separator: " ")
}

/// How long a turn worked ("0s", "1m", "1h 5s"): the units that are not zero.
public func phoneDurationB(_ ms: Double, lang: PhoneCodexLang = .current) -> String {
    let p = durationParts(ms, lang).filter { $0.n > 0 }
    return p.isEmpty ? durationParts(0, lang)[3].text : p.map(\.text).joined(separator: " ")
}

/// The step of a plan the agent is on ("Step 2 / 5"): the one in progress, else the first not done, else the last.
public func phonePlanStep(_ entries: [PhonePlanEntry]) -> (step: Int, total: Int, done: Int, current: PhonePlanEntry?) {
    let done = entries.filter { $0.status == "completed" }.count
    let at = entries.firstIndex { $0.status == "in_progress" } ?? entries.firstIndex { $0.status != "completed" }
    return (at.map { $0 + 1 } ?? entries.count, entries.count, done, at.map { entries[$0] })
}

/// The running turn's plan as last updated, if it has one (shown above the composer).
public func phoneLivePlan(_ turns: [PhoneChatTurn]) -> [PhonePlanEntry]? {
    guard let turn = turns.last, turn.status == "running" else { return nil }
    for b in turn.blocks.reversed() { if case .plan(let entries) = b { return entries.isEmpty ? nil : entries } }
    return nil
}

/// Questions that wait for the user's answer: the latest request of the running turn.
public struct PhoneCodexRequest: Equatable, Sendable {
    public let turn: Int
    /// Nil from an older host, which names no request (all the turn's open questions are one).
    public let request: String?
    public let questions: [PhoneQuestion]
}

public func phonePendingRequest(_ turns: [PhoneChatTurn]) -> PhoneCodexRequest? {
    guard let turn = turns.last(where: { $0.status == "running" }) else { return nil }
    let open = turn.blocks.compactMap { b -> PhoneQuestion? in if case .question(let q) = b, !q.answered { q } else { nil } }
    guard let last = open.last else { return nil }
    return PhoneCodexRequest(turn: turn.n, request: last.request, questions: open.filter { $0.request == last.request })
}

/// An option's label without Codex's " (Recommended)" mark, and whether it had it.
public func phoneSplitRecommended(_ label: String) -> (label: String, recommended: Bool) {
    if let m = phoneFirstMatch(label, "^(.*\\S)\\s+\\(Recommended\\)$"), let inner = m.groups.first ?? nil { return (inner, true) }
    return (label, false)
}

/// What was chosen and typed so far in a request's card: the question on show, the option picked per
/// question (none: a typed answer, or nothing), the text typed per question. The first option is
/// picked from the start.
public struct PhoneCodexDraft: Equatable, Sendable {
    public var at = 0
    public var picks: [String: Int] = [:]
    public var texts: [String: String] = [:]
    /// The questions it is for.
    public let qids: [String]
    public init(_ questions: [PhoneQuestion]) {
        qids = questions.map(\.qid)
        for q in questions where !q.options.isEmpty { picks[q.qid] = 0 }
    }
    /// A draft kept from before, if it is for exactly these questions; anything else starts afresh.
    public static func restore(_ had: PhoneCodexDraft?, for questions: [PhoneQuestion]) -> PhoneCodexDraft {
        guard var had, had.qids.sorted() == questions.map(\.qid).sorted(), !questions.isEmpty else { return PhoneCodexDraft(questions) }
        had.at = min(had.at, questions.count - 1)
        return had
    }
    /// A question's answer: the option chosen, else what was typed, else nothing (skipped).
    public func answer(_ q: PhoneQuestion) -> String {
        if let k = picks[q.qid], q.options.indices.contains(k) { return trimmed(q.options[k]) }
        return trimmed(texts[q.qid] ?? "")
    }
    /// Every question's answer, an empty one for a skipped question: one `answer` command each.
    public func answers(_ questions: [PhoneQuestion]) -> [(qid: String, text: String)] {
        questions.map { ($0.qid, answer($0)) }
    }
}

/// A run of terminal output in one style (from its ANSI escapes).
public struct PhoneAnsiRun: Equatable, Sendable {
    public var text: String
    /// Colors: an index into the 16 terminal colors ("c3"), or "rgb(r g b)" for 256-color and true color.
    public var fg: String?
    public var bg: String?
    public var bold = false
    public var dim = false
    public var italic = false
    public var underline = false
    public var strike = false
    public var hidden = false
    public init(text: String, fg: String? = nil, bg: String? = nil, bold: Bool = false, dim: Bool = false, italic: Bool = false,
                underline: Bool = false, strike: Bool = false, hidden: Bool = false) {
        self.text = text; self.fg = fg; self.bg = bg; self.bold = bold; self.dim = dim; self.italic = italic
        self.underline = underline; self.strike = strike; self.hidden = hidden
    }
}

/// The 256-color palette beyond the 16 named colors: a 6×6×6 cube, then 24 grays.
private func color256(_ n: Int) -> String? {
    if n < 16 { return "c\(n % 8)" }
    if n < 232 {
        let k = n - 16
        func v(_ x: Int) -> Int { x == 0 ? 0 : 55 + x * 40 }
        return "rgb(\(v(k / 36)) \(v((k / 6) % 6)) \(v(k % 6)))"
    }
    if n < 256 {
        let g = 8 + (n - 232) * 10
        return "rgb(\(g) \(g) \(g))"
    }
    return nil
}

/// An escape sequence at `i` (styles, cursor and erase commands, titles and links): its length, one
/// piece that takes no room.
private func escapeLength(_ c: [Character], _ i: Int) -> Int {
    guard i + 1 < c.count else { return 1 }
    func letter(_ x: Character) -> Bool { x.isASCII && x.isLetter }
    switch c[i + 1] {
    case "[":
        var j = i + 2
        while j < c.count, "0123456789;?".contains(c[j]) { j += 1 }
        return j < c.count && letter(c[j]) ? j + 1 - i : 2
    case "]":
        var j = i + 2
        while j < c.count, c[j] != "\u{7}", c[j] != "\u{1b}" { j += 1 }
        if j < c.count, c[j] == "\u{7}" { return j + 1 - i }
        if j + 1 < c.count, c[j] == "\u{1b}", c[j + 1] == "\\" { return j + 2 - i }
        return 2
    case "(", ")":
        return i + 2 < c.count && c[i + 2].isASCII && (c[i + 2].isLetter || c[i + 2].isNumber) ? 3 : 2
    case "\r", "\u{2028}", "\u{2029}":
        return 1
    default:
        return 2
    }
}

/// One line as a terminal leaves it: `\r` goes back to its start and what follows writes over it,
/// `\b` steps back one character (at the end of the line it takes that character away, as Codex shows
/// it), ESC[K / ESC[1K / ESC[2K erase to its end / from its start / all of it. Escapes take no room:
/// they stay with the character that follows them.
private func editLine(_ line: String) -> String {
    guard line.contains("\r") || line.contains("\u{8}") else { return line }
    struct Cell { var pre: String; var ch: Character; var erased = false }
    let c = Array(line)
    var cells: [Cell] = []
    var pending = ""
    var at = 0
    var i = 0
    while i < c.count {
        let ch = c[i]
        if ch == "\u{1b}" {
            let n = escapeLength(c, i)
            let seq = String(c[i..<(i + n)])
            i += n
            guard let how = phoneFirstMatch(seq, "^\\u001B\\[([012]?)K$").map({ $0.groups.first.flatMap { $0 } ?? "" }) else {
                pending += seq
                continue
            }
            let kind = how.isEmpty ? "0" : how
            if kind == "0" {
                if cells.count > at { cells.removeLast(cells.count - at) }
            } else {
                var k = kind == "2" ? cells.count - 1 : min(at, cells.count - 1)
                while k >= 0 {
                    cells[k] = Cell(pre: cells[k].pre, ch: " ", erased: true)
                    k -= 1
                }
            }
            if kind == "2", cells.count > at { cells.removeLast(cells.count - at) }
            continue
        }
        i += 1
        if ch == "\r" {
            at = 0
        } else if ch == "\u{8}" {
            at = max(0, at - 1)
            if at == cells.count - 1 {
                pending = cells[at].pre + pending
                cells.removeLast()
            }
        } else {
            while cells.count < at { cells.append(Cell(pre: "", ch: " ")) }
            if at < cells.count { cells[at] = Cell(pre: cells[at].pre + pending, ch: ch) } else { cells.append(Cell(pre: pending, ch: ch)) }
            pending = ""
            at += 1
        }
    }
    // What an erase left blank at the end is not text.
    while let last = cells.last, last.erased {
        pending = last.pre + pending
        cells.removeLast()
    }
    return cells.map { $0.pre + String($0.ch) }.joined() + pending
}

/// Terminal output as styled runs: SGR colors (16, 256 and true color, front and back), bold, dim,
/// italic, underline, strikethrough, hidden; `\r` rewrites the line it is on, `\b` takes back a
/// character; other escapes are dropped. Text only, never markup.
public func phoneParseAnsi(_ input: String) -> [PhoneAnsiRun] {
    // An escape cut off at the end (output still streaming, or cut by the host) waits for its rest.
    // Line ends are \n (a \r\n line end is not a return to the line's start).
    let text = input.replacingOccurrences(of: "\\u001B(?:\\[[0-9;?]*|\\][^\\u0007\\u001B]*|[()])?\\z", with: "", options: .regularExpression)
        .replacingOccurrences(of: "\r+\n", with: "\n", options: .regularExpression)
        .components(separatedBy: "\n").map(editLine).joined(separator: "\n") as NSString
    var runs: [PhoneAnsiRun] = []
    var style = PhoneAnsiRun(text: "")
    func push(_ t: String) {
        guard !t.isEmpty else { return }
        if var last = runs.last {
            last.text = ""
            if last == style {
                runs[runs.count - 1].text += t
                return
            }
        }
        var run = style
        run.text = t
        runs.append(run)
    }
    let pattern = "\\u001B\\[([0-9;]*)m|\\u001B\\[[0-9;?]*[A-Za-z]|\\u001B\\][^\\u0007\\u001B]*(\\u0007|\\u001B\\\\)|\\u001B[()][A-Za-z0-9]|\\u001B."
    guard let re = try? NSRegularExpression(pattern: pattern) else { return [PhoneAnsiRun(text: text as String)] }
    var at = 0
    for m in re.matches(in: text as String, range: NSRange(location: 0, length: text.length)) {
        push(text.substring(with: NSRange(location: at, length: m.range.location - at)))
        at = m.range.location + m.range.length
        guard m.range(at: 1).location != NSNotFound else { continue }
        let sgr = text.substring(with: m.range(at: 1))
        let codes = sgr.isEmpty ? [0] : sgr.components(separatedBy: ";").map { Int($0) ?? 0 }
        var i = 0
        while i < codes.count {
            let c = codes[i]
            switch c {
            case 0: style = PhoneAnsiRun(text: "")
            case 1: style.bold = true
            case 2: style.dim = true
            case 3: style.italic = true
            case 4: style.underline = true
            case 8: style.hidden = true
            case 9: style.strike = true
            case 22: style.bold = false; style.dim = false
            case 23: style.italic = false
            case 24: style.underline = false
            case 28: style.hidden = false
            case 29: style.strike = false
            case 30...37, 90...97: style.fg = "c\(c % 10)"
            case 39: style.fg = nil
            case 40...47, 100...107: style.bg = "c\(c % 10)"
            case 49: style.bg = nil
            case 38, 48:
                let code = { (k: Int) in k < codes.count ? codes[k] : nil }
                if code(i + 1) == 5 {
                    let col = color256(code(i + 2) ?? -1)
                    if c == 38 { style.fg = col } else { style.bg = col }
                    i += 2
                } else if code(i + 1) == 2 {
                    let rgb = [code(i + 2), code(i + 3), code(i + 4)].map { max(0, min(255, $0 ?? 0)) }
                    let col = "rgb(\(rgb[0]) \(rgb[1]) \(rgb[2]))"
                    if c == 38 { style.fg = col } else { style.bg = col }
                    i += 4
                }
            default: break
            }
            i += 1
        }
    }
    push(text.substring(from: at))
    return runs
}

/// How long until a question goes on without an answer, as the card says it ("1:45").
public func phoneCodexClock(_ ms: Double) -> String {
    let s = Int(max(0, (ms / 1000).rounded(.up)))
    return "\(s / 60):" + (s % 60 < 10 ? "0" : "") + "\(s % 60)"
}

/// When a message was sent, as its line says it: the time today, the weekday this week, else the date.
public func phoneFormatMsgTime(_ ts: Double, now: Double, lang: PhoneCodexLang = .current, calendar: Calendar = .current) -> String {
    let locale = Locale(identifier: lang == .zh ? "zh-CN" : "en")
    let d = Date(timeIntervalSince1970: ts / 1000)
    func format(_ template: String) -> String {
        let f = DateFormatter()
        f.locale = locale
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.setLocalizedDateFormatFromTemplate(template)
        return f.string(from: d)
    }
    let time = format("jmm")
    let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: d), to: calendar.startOfDay(for: Date(timeIntervalSince1970: now / 1000))).day ?? 0
    if days <= 0 { return time }
    if days <= 6 { return "\(format("EEEE")) \(time)" }
    return "\(format("MMMd")) \(time)"
}

// MARK: - Words

/// The Codex view's words, in English and Simplified Chinese: the desktop's `cdx_*` strings (and the
/// few `hc_*` ones it shares), so both apps say the same. The rest of the iPhone app is Chinese; this
/// view follows the phone's language as the desktop app follows the computer's.
public enum PhoneCodexLang: Sendable {
    case en, zh
    /// The phone's first preferred language decides.
    public static var current: PhoneCodexLang {
        (Locale.preferredLanguages.first ?? "").lowercased().hasPrefix("zh") ? .zh : .en
    }
}

/// The string for `key`, with `{name}` placeholders filled from `vars`.
public func phoneCodexText(_ key: String, _ vars: [String: String] = [:], lang: PhoneCodexLang = .current) -> String {
    let s = (lang == .zh ? PhoneCodexStrings.zh[key] : PhoneCodexStrings.en[key]) ?? PhoneCodexStrings.en[key] ?? key
    var filled = s
    for (k, v) in vars { filled = filled.replacingOccurrences(of: "{\(k)}", with: v) }
    // Chinese templates space Latin names ("Claude Code 在等你"); a Chinese name needs no space.
    if lang == .zh { filled = filled.replacingOccurrences(of: "([\\u3400-\\u9fff])\\s+(?=[\\u3400-\\u9fff])", with: "$1", options: .regularExpression) }
    return filled
}

/// `key_one` or `key_many` by `n`, with `{n}` filled.
public func phoneCodexPlural(_ key: String, _ n: Int, lang: PhoneCodexLang = .current) -> String {
    phoneCodexText("\(key)_\(n == 1 ? "one" : "many")", ["n": "\(n)"], lang: lang)
}

/// A finished group's sentence: what it did first, then the rest ("Edited files, read files, ran a
/// command" / "编辑了文件，读取了文件，运行了命令"); "Completed" when it did nothing to name.
public func phoneCodexSummarySentence(_ acts: [PhoneCodexAct], lang: PhoneCodexLang = .current) -> String {
    let parts = phoneGroupSummary(acts).parts.enumerated().map { i, p in
        phoneCodexPlural("cdx_s_\(p.part)\(i == 0 ? "_lead" : "")", p.n, lang: lang)
    }
    if parts.isEmpty { return phoneCodexText("cdx_s_done", lang: lang) }
    return parts.joined(separator: lang == .zh ? "，" : ", ")
}

// The desktop's words (apps/desktop/src/lib/strings/en.ts and zh.ts), the keys this view uses; cdx_collapse / cdx_expand are
// the phone's own (a line's accessibility value).
enum PhoneCodexStrings {
    static let en: [String: String] = [
        "hc_cut": "Cut here; the whole output is in the host's transcript.",
        "hc_failed": "Failed",
        "hc_st_stopping": "Stopping…",
        "hc_stopped_lede": "You stopped this turn.",
        "hc_continue": "Continue",
        "cdx_worked_for": "Worked for {t}",
        "cdx_working_for": "Working for {t}",
        "cdx_working": "Working",
        "cdx_stopped_after": "You stopped after {t}",
        "cdx_failed_after": "Failed after {t}",
        "cdx_thinking": "Thinking",
        "cdx_s_done": "Completed",
        "cdx_s_edited_lead_one": "Edited a file",
        "cdx_s_edited_lead_many": "Edited files",
        "cdx_s_edited_one": "edited a file",
        "cdx_s_edited_many": "edited files",
        "cdx_s_read_lead_one": "Read files",
        "cdx_s_read_lead_many": "Read files",
        "cdx_s_read_one": "read files",
        "cdx_s_read_many": "read files",
        "cdx_s_ran_lead_one": "Ran a command",
        "cdx_s_ran_lead_many": "Ran commands",
        "cdx_s_ran_one": "ran a command",
        "cdx_s_ran_many": "ran commands",
        "cdx_s_web_lead_one": "Searched the web",
        "cdx_s_web_lead_many": "Searched the web",
        "cdx_s_web_one": "searched the web",
        "cdx_s_web_many": "searched the web",
        "cdx_s_tools_lead_one": "Called a tool",
        "cdx_s_tools_lead_many": "Called tools",
        "cdx_s_tools_one": "called a tool",
        "cdx_s_tools_many": "called tools",
        "cdx_s_failed_lead_one": "Failed to edit a file",
        "cdx_s_failed_lead_many": "Failed to edit files",
        "cdx_s_failed_one": "failed to edit a file",
        "cdx_s_failed_many": "failed to edit files",
        "cdx_v_read": "Read",
        "cdx_v_reading": "Reading",
        "cdx_v_searched": "Searched",
        "cdx_v_searching": "Searching",
        "cdx_v_listed": "Listed",
        "cdx_v_listing": "Listing",
        "cdx_v_ran": "Ran",
        "cdx_v_running": "Running",
        "cdx_v_stopped": "Stopped",
        "cdx_v_edited": "Edited",
        "cdx_v_editing": "Editing",
        "cdx_v_created": "Created",
        "cdx_v_creating": "Creating",
        "cdx_v_deleted": "Deleted",
        "cdx_v_deleting": "Deleting",
        "cdx_v_moved": "Moved",
        "cdx_v_called": "Called",
        "cdx_v_calling": "Calling",
        "cdx_v_web": "Searched the web",
        "cdx_v_web_ing": "Searching the web",
        "cdx_v_failed": "Failed",
        "cdx_v_failed_edit": "Failed to edit",
        "cdx_d_for": "for {q}",
        "cdx_d_in": "in {p}",
        "cdx_d_files": "files",
        "cdx_d_files_in": "files in {p}",
        "cdx_d_to": "to {p}",
        "cdx_after_time": "after {t}",
        "cdx_in_time": "in {t}",
        "cdx_shell": "Shell",
        "cdx_success": "Success",
        "cdx_exit_code": "Exit code {n}",
        "cdx_exit_short": "exit {n}",
        "cdx_stopped": "Stopped",
        "cdx_no_output": "No output",
        "cdx_copy_cmd": "Copy command",
        "cdx_copy_output": "Copy output",
        "cdx_copy_patch": "Copy patch",
        "cdx_edited_file": "Edited {name}",
        "cdx_edited_files": "Edited {n} files",
        "cdx_review": "Review",
        "cdx_review_changes": "Review changes",
        "cdx_collapse_files": "Collapse files",
        "cdx_too_large_inline": "Too large to show inline",
        "cdx_show_more_files_one": "Show {n} more file",
        "cdx_show_more_files_many": "Show {n} more files",
        "cdx_plan": "Plan",
        "cdx_plan_step": "Step {s} / {n}",
        "cdx_copy_response": "Copy response",
        "cdx_compacted": "Context automatically compacted",
        "cdx_subagent": "Subagent",
        "cdx_q_asked": "Asked",
        "cdx_q_n_one": "{n} question",
        "cdx_q_n_many": "{n} questions",
        "cdx_q_no_answer": "No answer provided",
        "cdx_q_page": "{i} / {n}",
        "cdx_q_prev": "Previous question",
        "cdx_q_next": "Next question",
        "cdx_q_close": "Close",
        "cdx_q_skip": "Skip",
        "cdx_q_next_btn": "Next",
        "cdx_q_other_ph": "No, and tell Codex what to do differently",
        "cdx_q_type_here": "Type here",
        "cdx_q_recommended": "Recommended",
        "cdx_q_expires": "Codex goes on without an answer in {t}",
        "cdx_q_secret_ph": "Type the answer (not shown)",
        "cdx_exit_unknown": "Exit code unknown",
        "cdx_show_all": "Show all",
        "cdx_waiting_answer": "Waiting for your answer",
        "cdx_too_large": "Too large to show here",
        "cdx_more_lines": "{n} more lines",
        "cdx_collapse": "Collapse",
        "cdx_expand": "Expand",
    ]
    static let zh: [String: String] = [
        "hc_cut": "此处已截断，完整输出在被控端的会话记录里。",
        "hc_failed": "失败",
        "hc_st_stopping": "正在停止…",
        "hc_stopped_lede": "你停止了这一轮。",
        "hc_continue": "继续",
        "cdx_worked_for": "用时 {t}",
        "cdx_working_for": "已处理 {t}",
        "cdx_working": "处理中",
        "cdx_stopped_after": "你在 {t} 后停止了",
        "cdx_failed_after": "{t} 后失败",
        "cdx_thinking": "思考中",
        "cdx_s_done": "已完成",
        "cdx_s_edited_lead_one": "编辑了文件",
        "cdx_s_edited_lead_many": "编辑了文件",
        "cdx_s_edited_one": "编辑了文件",
        "cdx_s_edited_many": "编辑了文件",
        "cdx_s_read_lead_one": "读取了文件",
        "cdx_s_read_lead_many": "读取了文件",
        "cdx_s_read_one": "读取了文件",
        "cdx_s_read_many": "读取了文件",
        "cdx_s_ran_lead_one": "运行了命令",
        "cdx_s_ran_lead_many": "运行了命令",
        "cdx_s_ran_one": "运行了命令",
        "cdx_s_ran_many": "运行了命令",
        "cdx_s_web_lead_one": "搜索了网页",
        "cdx_s_web_lead_many": "搜索了网页",
        "cdx_s_web_one": "搜索了网页",
        "cdx_s_web_many": "搜索了网页",
        "cdx_s_tools_lead_one": "调用了工具",
        "cdx_s_tools_lead_many": "调用了工具",
        "cdx_s_tools_one": "调用了工具",
        "cdx_s_tools_many": "调用了工具",
        "cdx_s_failed_lead_one": "编辑文件失败",
        "cdx_s_failed_lead_many": "编辑文件失败",
        "cdx_s_failed_one": "编辑文件失败",
        "cdx_s_failed_many": "编辑文件失败",
        "cdx_v_read": "读取了",
        "cdx_v_reading": "正在读取",
        "cdx_v_searched": "搜索了",
        "cdx_v_searching": "正在搜索",
        "cdx_v_listed": "列出了",
        "cdx_v_listing": "正在列出",
        "cdx_v_ran": "运行了",
        "cdx_v_running": "正在运行",
        "cdx_v_stopped": "停止了",
        "cdx_v_edited": "编辑了",
        "cdx_v_editing": "正在编辑",
        "cdx_v_created": "创建了",
        "cdx_v_creating": "正在创建",
        "cdx_v_deleted": "删除了",
        "cdx_v_deleting": "正在删除",
        "cdx_v_moved": "移动了",
        "cdx_v_called": "调用了",
        "cdx_v_calling": "正在调用",
        "cdx_v_web": "搜索了网页",
        "cdx_v_web_ing": "正在搜索网页",
        "cdx_v_failed": "失败",
        "cdx_v_failed_edit": "未能编辑",
        "cdx_d_for": "{q}",
        "cdx_d_in": "（{p}）",
        "cdx_d_files": "文件",
        "cdx_d_files_in": "{p} 中的文件",
        "cdx_d_to": "到 {p}",
        "cdx_after_time": "· {t}后",
        "cdx_in_time": "用时 {t}",
        "cdx_shell": "终端",
        "cdx_success": "成功",
        "cdx_exit_code": "退出码 {n}",
        "cdx_exit_short": "退出码 {n}",
        "cdx_stopped": "已停止",
        "cdx_no_output": "没有输出",
        "cdx_copy_cmd": "复制命令",
        "cdx_copy_output": "复制输出",
        "cdx_copy_patch": "复制补丁",
        "cdx_edited_file": "已编辑 {name}",
        "cdx_edited_files": "已编辑 {n} 个文件",
        "cdx_review": "审阅",
        "cdx_review_changes": "查看更改",
        "cdx_collapse_files": "收起文件",
        "cdx_too_large_inline": "太大，无法内联显示",
        "cdx_show_more_files_one": "再显示 {n} 个文件",
        "cdx_show_more_files_many": "再显示 {n} 个文件",
        "cdx_plan": "计划",
        "cdx_plan_step": "第 {s} / {n} 步",
        "cdx_copy_response": "复制回答",
        "cdx_compacted": "上下文已自动压缩",
        "cdx_subagent": "子智能体",
        "cdx_q_asked": "已询问",
        "cdx_q_n_one": "{n} 个问题",
        "cdx_q_n_many": "{n} 个问题",
        "cdx_q_no_answer": "未提供答案",
        "cdx_q_page": "{i} / {n}",
        "cdx_q_prev": "上一题",
        "cdx_q_next": "下一题",
        "cdx_q_close": "关闭",
        "cdx_q_skip": "跳过",
        "cdx_q_next_btn": "下一步",
        "cdx_q_other_ph": "不，并告诉 Codex 该怎么做",
        "cdx_q_type_here": "在此输入",
        "cdx_q_recommended": "推荐",
        "cdx_q_expires": "{t} 后 Codex 会不等回答继续",
        "cdx_q_secret_ph": "输入答案（不显示）",
        "cdx_exit_unknown": "退出码未知",
        "cdx_show_all": "显示全部",
        "cdx_waiting_answer": "等你回答",
        "cdx_too_large": "太大，这里不显示",
        "cdx_more_lines": "还有 {n} 行",
        "cdx_collapse": "收起",
        "cdx_expand": "展开",
    ]
}
