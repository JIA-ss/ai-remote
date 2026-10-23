import Foundation

// The chat composer's helpers and the rest of the desktop's pure chat logic the phone needs: the `/`
// command menu and `@` file menu, image limits, messages on their way (sending / not delivered), the
// folder picker's path rules, sessions by folder, and which files a turn can preview. Ports of the
// desktop's apps/desktop/src/lib/chat-input.ts, the composer and folder parts of lib/chat.ts and
// lib/preview.ts; keep them in step. Pure, so they run and are tested on macOS
// (tools/test-phone-chat-model.sh).
//
// Offsets into the composer's text are UTF-16 offsets, the units UITextView's selection uses.

// MARK: - Commands and files

/// A command or skill the agent offers (the host's chat/event.rs `SlashCommand`).
public struct PhoneSlashCommand: Equatable, Sendable, Identifiable {
    public let name: String
    public let description: String?
    /// `command` or `skill`.
    public let kind: String
    /// What choosing it puts in the message (`/name`, or `$name` for a Codex skill).
    public let insert: String
    public let hint: String?
    public var id: String { kind + ":" + name }
    public init(name: String, description: String? = nil, kind: String = "command", insert: String? = nil, hint: String? = nil) {
        self.name = name; self.description = description; self.kind = kind; self.insert = insert ?? "/" + name; self.hint = hint
    }
    public init(_ v: JSONValue) {
        let name = v["name"].string ?? ""
        self.init(name: name, description: v["description"].string, kind: v["kind"].string ?? "command",
                  insert: v["insert"].string, hint: v["hint"].string)
    }
}

/// The menu the caret is in: `/` at the start of the message, or `@` at the start of a word.
public struct PhoneAutocomplete: Equatable, Sendable {
    /// `slash` or `mention`.
    public let mode: String
    /// What was typed after the trigger.
    public let query: String
    /// Where the trigger is, and the end of the word the caret is in (UTF-16 offsets).
    public let start: Int
    public let end: Int
}

private func isSpace(_ unit: unichar) -> Bool {
    guard let scalar = Unicode.Scalar(unit) else { return false }
    return CharacterSet.whitespacesAndNewlines.contains(scalar)
}

public func phoneAutocompleteAt(_ text: String, caret: Int) -> PhoneAutocomplete? {
    let s = text as NSString
    let caret = max(0, min(caret, s.length))
    var start = caret
    while start > 0 && !isSpace(s.character(at: start - 1)) { start -= 1 }
    var end = caret
    while end < s.length && !isSpace(s.character(at: end)) { end += 1 }
    let word = s.substring(with: NSRange(location: start, length: caret - start))
    let before = s.substring(to: start)
    if word.hasPrefix("/"), before.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !word.dropFirst().contains("/") {
        return PhoneAutocomplete(mode: "slash", query: String(word.dropFirst()), start: start, end: end)
    }
    if word.hasPrefix("@") { return PhoneAutocomplete(mode: "mention", query: String(word.dropFirst()), start: start, end: end) }
    return nil
}

/// Commands matching `query`: names that start with it first, then names and descriptions that contain it.
public func phoneMatchCommands(_ commands: [PhoneSlashCommand], _ query: String, limit: Int = 50) -> [PhoneSlashCommand] {
    let q = query.lowercased()
    if q.isEmpty { return Array(commands.prefix(limit)) }
    func rank(_ c: PhoneSlashCommand) -> Int {
        let n = c.name.lowercased()
        if n.hasPrefix(q) { return 0 }
        if n.contains(q) { return 1 }
        if (c.description ?? "").lowercased().contains(q) { return 2 }
        return 3
    }
    let ranked: [(c: PhoneSlashCommand, r: Int, i: Int)] = commands.enumerated().map { (c: $0.element, r: rank($0.element), i: $0.offset) }
    let kept = ranked.filter { $0.r < 3 }.sorted { (x, y) -> Bool in x.r != y.r ? x.r < y.r : x.i < y.i }
    return kept.prefix(limit).map(\.c)
}

/// Replaces the word the menu was for with `insert` and a space; returns the text and where the caret goes.
public func phoneApplyChoice(_ text: String, _ ac: PhoneAutocomplete, insert: String) -> (text: String, caret: Int) {
    let s = text as NSString
    let after = s.substring(from: min(ac.end, s.length))
    let glue = after.hasPrefix(" ") ? "" : " "
    let next = s.substring(to: min(ac.start, s.length)) + insert + glue + after
    return (next, ac.start + (insert as NSString).length + 1)
}

/// A path for `@`: quoted when it has spaces, so the agent reads it as one.
public func phoneMentionText(_ path: String) -> String {
    path.contains(where: \.isWhitespace) ? "@\"\(path)\"" : "@" + path
}

/// Files matching `query` (the host already filtered them; this orders file-name matches first).
public func phoneRankFiles(_ files: [String], _ query: String, limit: Int = 50) -> [String] {
    let q = query.lowercased()
    if q.isEmpty { return Array(files.prefix(limit)) }
    func rank(_ f: String) -> Int {
        let base = phoneBaseName(f).lowercased()
        return base.hasPrefix(q) ? 0 : base.contains(q) ? 1 : 2
    }
    let ranked: [(f: String, r: Int, i: Int)] = files.enumerated().map { (f: $0.element, r: rank($0.element), i: $0.offset) }
    let sorted = ranked.sorted { (x, y) -> Bool in
        if x.r != y.r { return x.r < y.r }
        if x.f.count != y.f.count { return x.f.count < y.f.count }
        return x.i < y.i
    }
    return sorted.prefix(limit).map(\.f)
}

// MARK: - Images

/// What the conversation keeps of an image sent with a message (its bytes stay with the sender).
public struct PhoneAttachment: Equatable, Sendable {
    public let name: String
    public let mime: String
    public let bytes: Int
    public init(name: String, mime: String, bytes: Int) { self.name = name; self.mime = mime; self.bytes = bytes }
    init(_ v: JSONValue) { self.init(name: v["name"].string ?? "", mime: v["mime"].string ?? "", bytes: Int(v["bytes"].number ?? 0)) }
}

/// An image to send: its encoded bytes and type.
public struct PhoneImage: Equatable, Sendable, Identifiable {
    public let id: String
    public let name: String
    public let mime: String
    public let data: Data
    public init(id: String = UUID().uuidString, name: String, mime: String, data: Data) {
        self.id = id; self.name = name; self.mime = mime; self.data = data
    }
    /// As the `input` command carries it (base64, no `data:` prefix).
    public var payload: JSONValue { .object(["name": .string(name), "mime": .string(mime), "data": .string(data.base64EncodedString())]) }
}

/// The host's limits (chat/mod.rs): types every agent reads, how many, how big decoded.
public let phoneImageTypes = ["image/png", "image/jpeg", "image/gif", "image/webp"]
public let phoneMaxImages = 4
public let phoneMaxImageBytes = 3_932_160
/// Images are scaled down to this long edge before sending.
public let phoneImageEdge = 2048

/// Why an image cannot be attached as it is (`type`, `count`, `size`), or nil.
public func phoneImageProblem(mime: String, bytes: Int, have: Int) -> String? {
    if have >= phoneMaxImages { return "count" }
    if !phoneImageTypes.contains(mime.lowercased()) { return "type" }
    if bytes > phoneMaxImageBytes { return "size" }
    return nil
}

/// The size an image is scaled to so its long edge is at most `edge` (never scaled up).
public func phoneFitWithin(width: Int, height: Int, edge: Int = phoneImageEdge) -> (width: Int, height: Int, scaled: Bool) {
    let long = max(width, height)
    if long <= edge || long == 0 { return (width, height, false) }
    let k = Double(edge) / Double(long)
    return (max(1, Int((Double(width) * k).rounded())), max(1, Int((Double(height) * k).rounded())), true)
}

/// The input command for the host, with images when there are any (see `phoneInputCommand`).
public func phoneInputCommand(id: String, text: String, mode: String?, images: [PhoneImage]) -> [String: JSONValue] {
    var command = phoneInputCommand(id: id, text: text, mode: mode)
    if !images.isEmpty, case .object(var input)? = command["input"] {
        input["attachments"] = .array(images.map(\.payload))
        command["input"] = .object(input)
    }
    return command
}

// MARK: - Lines from the host

/// What a line from the chat channel is: the end of the chat (`closed`), a numbered `event`, the
/// agent's `commands` as last known (unnumbered, right after the hello), a `reply` to this device (an
/// `ack`, a refusal naming a message, an answer with a `rid`), or a refusal of something else (`notice`).
public func phoneChatLineKind(_ m: JSONValue) -> String? {
    guard case .object = m else { return nil }
    let t = m["t"].string
    if t == "closed" || t == "bye" { return "closed" }
    if m["seq"].number != nil { return "event" }
    if t == "commands" { return "commands" }
    if t == "ack" || m["rid"] != .null || (t == "error" && m["input_id"].string != nil) { return "reply" }
    if t == "error" { return "notice" }
    return nil
}

// MARK: - Messages on their way

/// A message this device sent and the conversation does not show yet: `sending` until the host
/// acknowledges it (`acked`), then until it appears as a `user` event or in the queue; `failed` when
/// the host refused it, the connection dropped before the ack, or no ack came in time.
public struct PhoneOutgoing: Equatable, Sendable, Identifiable {
    public var id: String
    public let text: String
    public let mode: String?
    public var at: Double
    /// `sending`, `acked` or `failed`.
    public var state = "sending"
    public var error: String?
    /// The host refused this id: a retry needs a new one (the host may remember this one).
    public var refused = false
    /// The images sent with it, for the bubble and for a retry.
    public let images: [PhoneImage]
    public init(id: String, text: String, mode: String?, at: Double, images: [PhoneImage] = []) {
        self.id = id; self.text = text; self.mode = mode; self.at = at; self.images = images
    }
}

/// An unacknowledged message counts as not sent after this long (hosts that acknowledge only).
public let phoneSendTimeoutMs: Double = 15_000

/// Updates messages on their way against the conversation: delivered ones (a `user` event or a queued
/// entry with their id) leave, dropped ones fail, and unacknowledged ones fail after
/// `phoneSendTimeoutMs` when the host acknowledges messages at all.
public func phoneSettleOutgoing(_ out: [PhoneOutgoing], log: PhoneChatLog, now: Double, hostAcks: Bool) -> [PhoneOutgoing] {
    var next: [PhoneOutgoing] = []
    for var o in out {
        if log.inputs.contains(o.id) || log.queued.contains(where: { $0.id == o.id }) || (o.state == "acked" && log.state == "ended") { continue }
        if let dropped = log.dropped.first(where: { $0.id == o.id }), o.state != "failed" {
            o.state = "failed"; o.error = dropped.text; o.refused = true
        } else if hostAcks, o.state == "sending", now - o.at > phoneSendTimeoutMs {
            o.state = "failed"; o.error = "timeout"
        }
        next.append(o)
    }
    return next
}

/// Applies a reply from the host to messages on their way (an `ack`, or a refusal naming one).
public func phoneOutgoingReply(_ out: [PhoneOutgoing], _ m: JSONValue) -> [PhoneOutgoing] {
    guard let id = m["input_id"].string, let i = out.firstIndex(where: { $0.id == id }) else { return out }
    var next = out
    if m["t"].string == "ack" {
        if next[i].state == "sending" { next[i].state = "acked" }
    } else if m["t"].string == "error" {
        next[i].state = "failed"; next[i].error = m["text"].string ?? m["code"].string ?? "refused"; next[i].refused = true
    }
    return next
}

/// The connection dropped: what the host had not acknowledged is not sent.
public func phoneOutgoingDisconnected(_ out: [PhoneOutgoing]) -> [PhoneOutgoing] {
    out.map { o in
        guard o.state == "sending" else { return o }
        var f = o; f.state = "failed"; f.error = "disconnected"; return f
    }
}

/// Sending a failed message again: the same id when the host may have it (it ignores repeats), a new
/// one after a refusal.
public func phoneRetryOutgoing(_ o: PhoneOutgoing, newID: String, now: Double) -> PhoneOutgoing {
    var r = o
    if o.refused { r.id = newID }
    r.state = "sending"; r.error = nil; r.refused = false; r.at = now
    return r
}

/// Why a message did not arrive, in words.
public func phoneOutgoingError(_ error: String?) -> String {
    switch error {
    case "timeout"?: "被控端没有回应"
    case "disconnected"?: "连接断开了"
    case let e?: e
    default: "没有送达"
    }
}

// MARK: - Folders on the host
// Paths are the host's, so they may be Windows (`D:\work\x`, `\\server\share`) or Unix (`/home/x`), and
// `~` is the host user's home (the host expands it). Nothing here touches the local file system.

private func matches(_ s: String, _ pattern: String) -> NSTextCheckingResult? {
    (try? NSRegularExpression(pattern: pattern))?.firstMatch(in: s, range: NSRange(location: 0, length: (s as NSString).length))
}

private func isDrive(_ s: String) -> Bool { matches(s, "^[A-Za-z]:$") != nil }

/// Whether `s` names a folder rather than being a search: absolute, `~`, a drive or a share.
public func phoneLooksLikePath(_ s: String) -> Bool {
    let v = s.trimmingCharacters(in: .whitespaces)
    return v == "~" || v.hasPrefix("~/") || v.hasPrefix("~\\") || v.hasPrefix("/") || matches(v, "^[A-Za-z]:([\\\\/]|$)") != nil || v.hasPrefix("\\\\")
}

/// `p` without trailing separators, except where that is a root (`/`, `C:\`).
public func phoneTrimFolder(_ p: String) -> String {
    let v = p.trimmingCharacters(in: .whitespaces)
    var t = v
    while let last = t.last, last == "/" || last == "\\" { t.removeLast() }
    if t.isEmpty { return v.isEmpty ? "" : (v.first == "\\" ? "\\" : "/") }
    if isDrive(t) { return t + (v.contains("/") && !v.contains("\\") ? "/" : "\\") }
    return t
}

/// A path as its root (`/`, `C:\`, `\\server\share\`, `~`, or "" when relative) and the names below it.
public func phoneFolderParts(_ p: String) -> (root: String, names: [String], sep: String) {
    let v = phoneTrimFolder(p)
    let sep = v.contains("\\") || isDrive(v) ? "\\" : "/"
    var root = ""
    let ns = v as NSString
    if let unc = matches(v, "^\\\\\\\\[^\\\\/]+[\\\\/][^\\\\/]+") {
        root = ns.substring(with: unc.range) + "\\"
    } else if matches(v, "^[A-Za-z]:") != nil {
        root = ns.substring(to: 2) + (ns.length > 2 && ns.substring(with: NSRange(location: 2, length: 1)) == "/" ? "/" : "\\")
    } else if v.hasPrefix("~") {
        root = "~"
    } else if v.hasPrefix("/") || v.hasPrefix("\\") {
        root = String(v.prefix(1))
    }
    let rest = ns.substring(from: min((root as NSString).length, ns.length))
    return (root, rest.split(whereSeparator: { $0 == "/" || $0 == "\\" }).map(String.init), sep)
}

private func buildFolder(_ root: String, _ names: [String], _ sep: String) -> String {
    if names.isEmpty { return root }
    if root.isEmpty { return names.joined(separator: sep) }
    return (root.hasSuffix("/") || root.hasSuffix("\\") ? root : root + sep) + names.joined(separator: sep)
}

/// What a folder is called: its last name (`app` for `D:\work\app`), or the root itself.
public func phoneFolderName(_ p: String) -> String {
    let (root, names, _) = phoneFolderParts(p)
    if let last = names.last { return last }
    if let unc = matches(root, "^\\\\\\\\[^\\\\]+\\\\([^\\\\]+)\\\\$") { return (root as NSString).substring(with: unc.range(at: 1)) }
    return matches(root, "^[A-Za-z]:[\\\\/]$") != nil ? String(root.prefix(2)) : root
}

/// The folder above `p`, or nil at a root.
public func phoneParentFolder(_ p: String) -> String? {
    let (root, names, sep) = phoneFolderParts(p)
    return names.isEmpty ? nil : buildFolder(root, Array(names.dropLast()), sep)
}

/// `name` inside `dir`, with `dir`'s separator.
public func phoneChildFolder(_ dir: String, _ name: String) -> String {
    let (root, names, sep) = phoneFolderParts(dir)
    return buildFolder(root, names + [name], sep)
}

/// The steps from the root down to `p`, for a breadcrumb.
public func phoneFolderCrumbs(_ p: String) -> [(name: String, path: String)] {
    let (root, names, sep) = phoneFolderParts(p)
    var out: [(name: String, path: String)] = root.isEmpty ? [] : [(phoneFolderName(root), root)]
    for i in names.indices { out.append((names[i], buildFolder(root, Array(names[...i]), sep))) }
    return out
}

/// Whether two host paths name the same folder (Windows paths compare without case).
public func phoneSameFolder(_ a: String, _ b: String) -> Bool {
    let x = phoneTrimFolder(a), y = phoneTrimFolder(b)
    func windows(_ v: String) -> Bool { matches(v, "^([A-Za-z]:|\\\\\\\\)") != nil || (v.contains("\\") && !v.contains("/")) }
    if !windows(x) && !windows(y) { return x == y }
    func norm(_ v: String) -> String { v.replacingOccurrences(of: "/", with: "\\").lowercased() }
    return norm(x) == norm(y)
}

/// `p` with the host user's home shortened to `~` (when that home is known).
public func phoneTildeFolder(_ p: String, home: String?) -> String {
    guard let home, !home.isEmpty else { return p }
    let h = phoneTrimFolder(home)
    if phoneSameFolder(p, h) { return "~" }
    let hn = phoneFolderParts(h).names
    let (root, pn, sep) = phoneFolderParts(p)
    if hn.isEmpty || pn.count <= hn.count { return p }
    return phoneSameFolder(buildFolder(root, Array(pn.prefix(hn.count)), sep), h) ? "~" + sep + pn.dropFirst(hn.count).joined(separator: sep) : p
}

/// The folders to offer first: recent ones, then those of the host's open sessions; no repeats.
public func phoneFolderChoices(recent: [String], sessions: [String] = [], limit: Int = 8) -> [String] {
    var out: [String] = []
    for f in recent + sessions {
        let v = phoneTrimFolder(f)
        if !v.isEmpty, !out.contains(where: { phoneSameFolder($0, v) }) { out.append(v) }
    }
    return Array(out.prefix(limit))
}

/// `folders` that match what was typed (name or path, without case); name matches first.
public func phoneMatchFolders(_ folders: [String], _ query: String) -> [String] {
    let q = query.trimmingCharacters(in: .whitespaces).lowercased()
    if q.isEmpty { return folders }
    func starts(_ f: String) -> Bool { phoneFolderName(f).lowercased().hasPrefix(q) }
    let hits = folders.enumerated().filter { $0.element.lowercased().contains(q) }
    return hits.sorted { starts($0.element) != starts($1.element) ? starts($0.element) : $0.offset < $1.offset }.map(\.element)
}

/// The subfolders in a `files_list` result, hidden ones (`.name`) only when asked for.
public func phoneListedFolders(_ entries: [JSONValue], query: String = "") -> [String] {
    let q = query.trimmingCharacters(in: .whitespaces).lowercased()
    return entries.filter { $0["kind"].string == "dir" || $0["kind"].string == "link" }.compactMap { $0["name"].string }
        .filter { (q.hasPrefix(".") || !$0.hasPrefix(".")) && (q.isEmpty || $0.lowercased().contains(q)) }
}

/// The host's sessions by folder (Orca's and Codex's "project → its threads"): the folder with the
/// latest activity first, and inside each the latest session first.
public func phoneSessionsByFolder(_ sessions: [PhoneChatSession]) -> [(folder: String, sessions: [PhoneChatSession])] {
    var groups: [(folder: String, sessions: [PhoneChatSession])] = []
    for s in sessions.sorted(by: { $0.lastActivity > $1.lastActivity }) {
        if let i = groups.firstIndex(where: { phoneSameFolder($0.folder, s.cwd) }) { groups[i].sessions.append(s) }
        else { groups.append((phoneTrimFolder(s.cwd), [s])) }
    }
    return groups
}

/// How long ago `then` was (milliseconds), in the largest whole unit: "刚刚", "3 分钟前", "2 小时前", "5 天前".
public func phoneAgo(_ then: Double, now: Double) -> String {
    let m = Int(max(0, now - then) / 60_000)
    if m < 1 { return "刚刚" }
    if m < 60 { return "\(m) 分钟前" }
    if m < 60 * 24 { return "\(m / 60) 小时前" }
    return "\(m / 1440) 天前"
}

// MARK: - Previews of files the agent made or named

/// What `path` is previewed as (`html`, `markdown`, `image`), or nil when it cannot be previewed.
public func phonePreviewKind(_ path: String) -> String? {
    let name = phoneBaseName(path)
    guard let dot = name.lastIndex(of: "."), dot != name.startIndex else { return nil }
    switch name[name.index(after: dot)...].lowercased() {
    case "html", "htm": return "html"
    case "md", "markdown": return "markdown"
    case "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp": return "image"
    default: return nil
    }
}

/// Largest file the host lets a preview read.
public let phoneMaxPreview = 16 * 1024 * 1024
/// Most text an HTML or Markdown preview renders; the source view shows the rest.
public let phoneMaxRendered = 2 * 1024 * 1024
/// Most files shown under one turn.
public let phoneMaxInlinePreviews = 4

/// The file a link or a bit of inline code in a reply names, when it names a previewable one:
/// `out/report.html`, `./notes.md`, `C:\work\chart.png`, `file:///tmp/a.svg`, `report.html#top`. Web
/// addresses, other schemes and text with spaces are not files.
public func phoneFileTarget(_ text: String) -> String? {
    var s = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if s.isEmpty || s.count > 1024 || s.replacingOccurrences(of: "%20", with: "").contains(where: \.isWhitespace) { return nil }
    if s.lowercased().hasPrefix("file://") {
        s = String(s.dropFirst(7))
        if matches(s, "^/[A-Za-z]:/") != nil { s = String(s.dropFirst()) }
        guard let decoded = s.removingPercentEncoding else { return nil }
        s = decoded
    } else if matches(s, "^[A-Za-z][A-Za-z0-9+.-]*:") != nil, matches(s, "^[A-Za-z]:[\\\\/]") == nil {
        return nil
    }
    if let cut = s.firstIndex(where: { $0 == "#" || $0 == "?" }) { s = String(s[..<cut]) }
    if let r = matches(s, ":\\d+(:\\d+)?$") { s = (s as NSString).substring(to: r.range.location) }
    if s.isEmpty || s.hasSuffix("/") || s.hasSuffix("\\") { return nil }
    return phonePreviewKind(s) != nil ? s : nil
}

/// The document an HTML preview renders: a policy at the top of its head that nothing is fetched from
/// the network (images only from `data:` and `blob:`, styles inline), no scripts, and that links open
/// nowhere. The phone also turns JavaScript off for the view and refuses every navigation after the
/// first load (`PhoneHTMLPreview`).
public func phonePreviewDocument(_ source: String) -> String {
    let guardTags = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; "
        + "font-src data:; media-src data: blob:; base-uri 'none'; form-action 'none'\"><base target=\"_blank\">"
    let ns = source as NSString
    if let head = matches(source, "(?i)<head\\b[^>]*>") {
        let at = head.range.location + head.range.length
        return ns.substring(to: at) + guardTags + ns.substring(from: at)
    }
    if let doctype = matches(source, "(?i)^\\s*<!doctype[^>]*>") {
        let at = doctype.range.location + doctype.range.length
        return ns.substring(to: at) + guardTags + ns.substring(from: at)
    }
    return guardTags + source
}

/// The previewable files a finished turn made or named, in order: files its tools wrote (an edit's
/// diff, a location of a tool that is not a read or a delete), then files its last reply names (a
/// link or inline code). One per file name; failed tools and deleted files are left out.
public func phoneTurnFiles(_ blocks: [PhoneChatBlock], max: Int = phoneMaxInlinePreviews) -> [String] {
    var out: [String] = []
    var seen = Set<String>()
    func add(_ p: String?) {
        guard let p, phonePreviewKind(p) != nil else { return }
        let name = phoneBaseName(p).lowercased()
        if seen.insert(name).inserted { out.append(p) }
    }
    for b in blocks {
        guard case .tool(let x) = b, x.status != "failed", x.kind != "read", x.kind != "delete" else { continue }
        for c in x.content { if case .diff(let path, let old, let new) = c, !(new.isEmpty && !(old ?? "").isEmpty) { add(path) } }
        for l in x.locations { add(l) }
    }
    let reply = blocks.reversed().compactMap { b -> String? in if case .text(_, let t, _, _) = b { t } else { nil } }.first
    if let reply, let re = try? NSRegularExpression(pattern: "\\]\\(([^)\\s]+)\\)|`([^`\\s]+)`") {
        let ns = reply as NSString
        for m in re.matches(in: reply, range: NSRange(location: 0, length: ns.length)) {
            let r = m.range(at: 1).location != NSNotFound ? m.range(at: 1) : m.range(at: 2)
            add(phoneFileTarget(ns.substring(with: r)))
        }
    }
    return Array(out.prefix(max))
}

/// The MIME type of an image preview, by its extension.
public func phoneImageMime(_ path: String) -> String {
    switch (phoneBaseName(path) as NSString).pathExtension.lowercased() {
    case "png": "image/png"
    case "jpg", "jpeg": "image/jpeg"
    case "gif": "image/gif"
    case "webp": "image/webp"
    case "svg": "image/svg+xml"
    case "bmp": "image/bmp"
    default: "application/octet-stream"
    }
}

/// "12 KB", "3.4 MB".
public func phoneSizeLabel(_ n: Int) -> String {
    if n < 1024 { return "\(n) B" }
    if n < 1024 * 1024 { return "\(Int((Double(n) / 1024).rounded())) KB" }
    return String(format: "%.1f MB", Double(n) / 1_048_576)
}
