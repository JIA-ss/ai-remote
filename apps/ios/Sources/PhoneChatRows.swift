import SwiftUI
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

// The rows of a chat turn (phoneTurnRows): the user's message, a run of tool calls folded into one
// sentence, a tool line with its command and output, an edit as a diff card, reasoning, a plan, a
// sub-agent, a question, a notice, and how the turn ended. The phone version of the desktop's
// apps/desktop/src/ui/chat/Rows.tsx, which follows Orca's native chat (stablyai/orca, MIT License,
// Copyright (c) 2026 Lovecast Inc.). Messages are shown as plain text with inline Markdown
// (AttributedString, which never renders HTML) and fenced code as monospaced blocks.

/// One row.
struct PhoneChatRowView: View {
    let row: PhoneChatRow
    let readonly: Bool
    let answer: (String, String) -> Void

    var body: some View {
        switch row {
        // A message that joined the running turn (steer) is shown where it landed, like any other.
        case .user(_, let text, let by, let ts, _, let extra): PhoneUserRow(text: text, by: by, ts: ts, extra: extra)
        case .text(_, let text, _): PhoneMarkdownText(text: text)
        case .thought(_, let thought, let live): PhoneThoughtRow(thought: thought, live: live)
        case .run(_, let members, let tools, let live): PhoneRunRow(members: members, tools: tools, live: live)
        case .agent(_, let tool): PhoneAgentRow(tool: tool)
        case .plan(_, let entries): PhonePlanRow(entries: entries)
        case .question(_, let q): PhoneQuestionCard(question: q, readonly: readonly, answer: answer)
        case .notice(_, let level, let code, let text): PhoneNoticeRow(level: level, text: phoneNoticeText(code: code, text: text))
        }
    }
}

// MARK: - Pieces

/// A fold header: icon, words, then whatever follows, and a chevron when it opens.
private struct FoldHeader<Trailing: View>: View {
    let icon: String
    let open: Bool
    let opens: Bool
    @ViewBuilder let label: () -> Trailing
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon).font(.footnote).foregroundStyle(.secondary).frame(width: 18)
            label()
            Spacer(minLength: 0)
            if opens {
                Image(systemName: "chevron.right").font(.caption2.weight(.semibold)).foregroundStyle(.tertiary)
                    .rotationEffect(.degrees(open ? 90 : 0))
            }
        }
        .contentShape(Rectangle())
    }
}

private func kindIcon(_ kind: String) -> String {
    switch kind {
    case "execute": "terminal"
    case "read": "doc.text"
    case "edit", "move": "pencil"
    case "delete": "doc.badge.minus"
    case "search": "magnifyingglass"
    case "fetch": "globe"
    case "think": "brain"
    default: "wrench.and.screwdriver"
    }
}

private func catIcon(_ cat: String) -> String {
    switch cat {
    case "command": "terminal"
    case "read": "doc.text"
    case "search": "magnifyingglass"
    case "web": "globe"
    case "change": "pencil"
    default: "wrench.and.screwdriver"
    }
}

/// Text with inline Markdown (bold, code, links) and fenced code blocks; never HTML.
struct PhoneMarkdownText: View {
    let text: String
    var font: Font = .body
    var color: Color = .primary
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(Self.parts(text).enumerated()), id: \.offset) { _, part in
                if part.code {
                    ScrollView(.horizontal, showsIndicators: false) {
                        Text(part.text).font(.footnote.monospaced()).textSelection(.enabled).padding(10)
                    }
                    .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
                } else {
                    Text(Self.inline(part.text)).font(font).foregroundStyle(color).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }
    static func inline(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(text)
    }
    /// Plain text and fenced code, in order.
    static func parts(_ text: String) -> [(code: Bool, text: String)] {
        var out: [(code: Bool, text: String)] = []
        var prose: [String] = [], code: [String] = [], inCode = false
        func flush() {
            let p = prose.joined(separator: "\n").trimmingCharacters(in: .newlines)
            if !p.trimmingCharacters(in: .whitespaces).isEmpty { out.append((false, p)) }
            prose = []
        }
        for line in text.components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                if inCode { out.append((true, code.joined(separator: "\n"))); code = [] } else { flush() }
                inCode.toggle()
            } else if inCode { code.append(line) } else { prose.append(line) }
        }
        if inCode { out.append((true, code.joined(separator: "\n"))) } else { flush() }
        return out
    }
}

// MARK: - The user's message

struct PhoneUserRow: View {
    let text: String
    let by: String
    let ts: Double
    @PhoneViewState private var open = false
    private var long: Bool { text.components(separatedBy: "\n").count > 12 || text.count > 1200 }

    var body: some View {
        HStack {
            Spacer(minLength: 40)
            VStack(alignment: .trailing, spacing: 4) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text).textSelection(.enabled).lineLimit(long && !open ? 12 : nil)
                    if long {
                        Button(open ? "收起" : "展开全文") { open.toggle() }.font(.caption)
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Color.blue.opacity(0.15), in: RoundedRectangle(cornerRadius: 14))
                HStack(spacing: 6) {
                    if !by.isEmpty { Text(by) }
                    if ts > 0 { Text(Date(timeIntervalSince1970: ts / 1000), style: .time) }
                }
                .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Tools

/// One tool call: its kind, what it acted on, how long it took, and its output behind a fold.
struct PhoneToolLine: View {
    let tool: PhoneChatTool
    @PhoneViewState private var open = false
    @PhoneViewState private var all = false

    private var detail: Bool {
        !tool.content.isEmpty || phoneToolCommand(tool) != nil || !tool.locations.isEmpty
    }

    var body: some View {
        let label = phoneToolLabel(tool)
        VStack(alignment: .leading, spacing: 6) {
            Button { if detail { open.toggle() } } label: {
                FoldHeader(icon: kindIcon(tool.kind), open: open, opens: detail) {
                    // A command is its own label ("npm test"), as in Orca; other tools say what they did.
                    if tool.kind != "execute" || label.target.isEmpty { Text(label.verb).font(.footnote.weight(.medium)) }
                    if !label.target.isEmpty {
                        Text(label.target).font(tool.kind == "execute" ? .footnote.monospaced() : .footnote)
                            .foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    }
                    Spacer(minLength: 4)
                    PhoneElapsed(start: tool.startedMs, end: tool.endedMs, running: tool.running)
                    if tool.running { ProgressView().controlSize(.mini) }
                    else if tool.stopped { Text("已停止").font(.caption2).foregroundStyle(.orange) }
                    else if tool.status == "failed" { Image(systemName: "xmark").font(.caption2.weight(.bold)).foregroundStyle(.red).accessibilityLabel("失败") }
                }
            }
            .buttonStyle(.plain)
            if open { detailView }
        }
        .accessibilityIdentifier("chat-tool-\(tool.callID)")
    }

    @ViewBuilder private var detailView: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let cmd = phoneToolCommand(tool) {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text("$ " + cmd).font(.caption.monospaced()).textSelection(.enabled).padding(8)
                }
                .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 6))
            }
            if !tool.locations.isEmpty, tool.kind != "execute" {
                ForEach(tool.locations, id: \.self) { Text($0).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle) }
            }
            let out = phoneToolOutput(tool)
            if !out.isEmpty {
                let shown = all ? (text: out, hidden: 0) : phoneTailLines(out, 14)
                if shown.hidden > 0 {
                    Button("显示全部 \(shown.hidden + shown.text.components(separatedBy: "\n").count) 行") { all = true }.font(.caption)
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(shown.text).font(.caption.monospaced()).foregroundStyle(tool.status == "failed" ? Color.red : Color.secondary)
                        .textSelection(.enabled).padding(8)
                }
                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 6))
                if all { Button("收起") { all = false }.font(.caption) }
            }
            if tool.truncated { Text("此处已截断，完整输出在被控端的会话记录里。").font(.caption2).foregroundStyle(.secondary) }
        }
        .padding(.leading, 26)
    }
}

/// How long something took (or has run so far), shown from one second on.
struct PhoneElapsed: View {
    let start: Double
    let end: Double?
    let running: Bool
    var body: some View {
        if running {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                Text(phoneDuration(max(0, context.date.timeIntervalSince1970 * 1000 - start))).font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
            }
        } else if let end, end - start >= 1000 {
            Text(phoneDuration(end - start)).font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
        }
    }
}

/// Tool calls in a row, summarized in one sentence that opens to the individual calls.
struct PhoneRunRow: View {
    let members: [PhoneRunMember]
    let tools: [PhoneChatTool]
    let live: Bool
    @PhoneViewState private var open = false

    var body: some View {
        if tools.count == 1, members.count == 1, let only = tools.first {
            if phoneIsEdit(only) { PhoneEditRow(tool: only) } else { PhoneToolLine(tool: only) }
        } else {
            let cats = phoneRunSummary(tools)
            let outcome = phoneRunOutcome(tools)
            let latest = live ? (tools.last(where: \.running) ?? tools.last) : nil
            VStack(alignment: .leading, spacing: 8) {
                Button { open.toggle() } label: {
                    FoldHeader(icon: cats.count == 1 ? catIcon(cats[0].cat) : "wrench.and.screwdriver", open: open, opens: true) {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(phoneRunSentence(tools, live: live)).font(.footnote.weight(.medium))
                                if let stat = phoneRunDiffStat(tools) {
                                    Text("+\(stat.add)").font(.caption.monospacedDigit()).foregroundStyle(.green)
                                    Text("−\(stat.del)").font(.caption.monospacedDigit()).foregroundStyle(.red)
                                }
                                if outcome.failed > 0 { Text("· \(outcome.failed) 个失败").font(.caption2).foregroundStyle(.secondary) }
                                if outcome.stopped > 0 { Text("· \(outcome.stopped) 个被停止").font(.caption2).foregroundStyle(.secondary) }
                                if !live, outcome.done, outcome.failed == 0, outcome.stopped == 0 {
                                    Image(systemName: "checkmark").font(.caption2.weight(.bold)).foregroundStyle(.green)
                                }
                                if live { ProgressView().controlSize(.mini) }
                            }
                            if let latest {
                                let target = phoneToolLabel(latest).target
                                Text(target.isEmpty ? latest.title : target).font(.caption.monospaced()).foregroundStyle(.secondary)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                        }
                    }
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("chat-run")
                if open {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(members.enumerated()), id: \.offset) { _, member in
                            switch member {
                            case .tool(let tool):
                                if phoneIsEdit(tool) { PhoneEditRow(tool: tool) } else { PhoneToolLine(tool: tool) }
                            case .thought(let thought): PhoneThoughtRow(thought: thought, live: false, inRun: true)
                            }
                        }
                    }
                    .padding(.leading, 12)
                    .overlay(alignment: .leading) { Rectangle().fill(Color.secondary.opacity(0.2)).frame(width: 1) }
                }
            }
        }
    }
}

// MARK: - Edits

/// An edit the agent made, as one card per file.
struct PhoneEditRow: View {
    let tool: PhoneChatTool
    var body: some View {
        let diffs: [(path: String, old: String?, new: String)] = tool.content.compactMap {
            if case .diff(let path, let old, let new) = $0 { (path, old, new) } else { nil }
        }
        if diffs.isEmpty {
            PhoneToolLine(tool: tool)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(diffs.enumerated()), id: \.offset) { _, d in PhoneDiffCard(path: d.path, old: d.old, new: d.new, status: tool.status) }
                if tool.truncated { Text("此处已截断，完整输出在被控端的会话记录里。").font(.caption2).foregroundStyle(.secondary) }
            }
        }
    }
}

struct PhoneDiffCard: View {
    let path: String
    let old: String?
    let new: String
    let status: String
    @PhoneViewState private var open = false

    var body: some View {
        let rows = phoneDiffRows(old, new)
        let stat = phoneDiffStat([.diff(path: path, old: old, new: new)])
        let created = (old ?? "").isEmpty
        let deleted = new.isEmpty && !created
        VStack(alignment: .leading, spacing: 0) {
            Button { if !rows.isEmpty { open.toggle() } } label: {
                FoldHeader(icon: created ? "doc.badge.plus" : deleted ? "doc.badge.minus" : "pencil", open: open, opens: !rows.isEmpty) {
                    Text(created ? "新建" : deleted ? "删除" : "编辑了").font(.footnote.weight(.medium))
                    Text(phoneBaseName(path)).font(.footnote.monospaced()).lineLimit(1).truncationMode(.middle)
                    if let stat {
                        Text("+\(stat.add)").font(.caption.monospacedDigit()).foregroundStyle(.green)
                        Text("−\(stat.del)").font(.caption.monospacedDigit()).foregroundStyle(.red)
                    }
                    if status == "failed" { Text("失败").font(.caption2).foregroundStyle(.red) }
                    if status == "in_progress" || status == "pending" { ProgressView().controlSize(.mini) }
                }
                .padding(10)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("chat-diff")
            .overlay(alignment: .trailing) {
                Button { phoneCopy(Self.patch(path, rows)) } label: { Image(systemName: "doc.on.doc").font(.caption) }
                    .buttonStyle(.plain).foregroundStyle(.secondary).padding(.trailing, 34)
                    .accessibilityLabel("复制 diff")
            }
            if open {
                Divider()
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, r in diffLine(r) }
                }
                .padding(.vertical, 6)
            }
        }
        .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color.secondary.opacity(0.18)))
    }

    @ViewBuilder private func diffLine(_ r: PhoneDiffRow) -> some View {
        switch r {
        case .gap(let n):
            Text("⋯ \(n) 行未改动").font(.caption2).foregroundStyle(.secondary).padding(.horizontal, 10).padding(.vertical, 3)
        case .context(let t): line(" ", t, .clear, .primary)
        case .add(let t): line("+", t, Color.green.opacity(0.14), .primary)
        case .del(let t): line("-", t, Color.red.opacity(0.12), .primary)
        }
    }
    private func line(_ sign: String, _ text: String, _ bg: Color, _ fg: Color) -> some View {
        // Long lines wrap rather than scroll sideways.
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(sign).foregroundStyle(.secondary)
            Text(text.isEmpty ? " " : text).foregroundStyle(fg).fixedSize(horizontal: false, vertical: true)
        }
        .font(.caption.monospaced()).padding(.horizontal, 10).padding(.vertical, 1)
        .frame(maxWidth: .infinity, alignment: .leading).background(bg)
    }

    /// The diff as unified patch text, for the copy button.
    static func patch(_ path: String, _ rows: [PhoneDiffRow]) -> String {
        let body = rows.map { r -> String in
            switch r {
            case .gap: "@@"
            case .context(let t): " " + t
            case .add(let t): "+" + t
            case .del(let t): "-" + t
            }
        }
        return (["--- \(path)", "+++ \(path)"] + body).joined(separator: "\n")
    }
}

// MARK: - Reasoning, plans, sub-agents

struct PhoneThoughtRow: View {
    let thought: PhoneThought
    let live: Bool
    var inRun = false
    @PhoneViewState private var open = false
    var body: some View {
        if !thought.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || live {
            let took = thought.endedMs - thought.startedMs
            VStack(alignment: .leading, spacing: 6) {
                Button { open.toggle() } label: {
                    FoldHeader(icon: "brain", open: open, opens: true) {
                        if inRun {
                            Text("思考过程").font(.footnote.weight(.medium))
                            Text(thought.text.components(separatedBy: "\n").first ?? "").font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                        } else {
                            Text(live ? "思考中…" : took >= 1000 ? "思考了 \(phoneDuration(took))" : "思考过程")
                                .font(.footnote.weight(.medium)).foregroundStyle(.secondary)
                        }
                    }
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("chat-thought")
                if open { PhoneMarkdownText(text: thought.text, font: .footnote, color: .secondary).padding(.leading, 26) }
            }
        }
    }
}

struct PhonePlanRow: View {
    let entries: [PhonePlanEntry]
    var body: some View {
        let done = entries.filter { $0.status == "completed" }.count
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "checklist").font(.footnote).foregroundStyle(.secondary)
                Text("计划").font(.footnote.weight(.semibold))
                Spacer()
                Text("完成 \(done)/\(entries.count)").font(.caption.monospacedDigit()).foregroundStyle(.secondary)
            }
            ForEach(Array(entries.enumerated()), id: \.offset) { _, e in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: e.status == "completed" ? "checkmark.circle.fill" : e.status == "in_progress" ? "circle.dotted.circle" : "circle")
                        .font(.caption).foregroundStyle(e.status == "completed" ? Color.green : e.status == "in_progress" ? Color.blue : Color.secondary)
                    Text(e.content).font(.footnote).strikethrough(e.status == "completed", color: .secondary)
                        .foregroundStyle(e.status == "completed" ? .secondary : .primary)
                }
            }
        }
        .padding(12)
        .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
    }
}

struct PhoneAgentRow: View {
    let tool: PhoneChatTool
    @PhoneViewState private var open = false
    var body: some View {
        let out = phoneToolOutput(tool)
        VStack(alignment: .leading, spacing: 6) {
            Button { if !out.isEmpty { open.toggle() } } label: {
                FoldHeader(icon: "person.2", open: open, opens: !out.isEmpty) {
                    Text("子智能体").font(.footnote.weight(.medium))
                    let target = phoneToolLabel(tool).target
                    Text(target.isEmpty ? tool.title : target).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                    Spacer(minLength: 4)
                    PhoneElapsed(start: tool.startedMs, end: tool.endedMs, running: tool.running)
                    if tool.running { ProgressView().controlSize(.mini) }
                    else if tool.status == "failed" { Image(systemName: "xmark").font(.caption2.weight(.bold)).foregroundStyle(.red) }
                    else { Image(systemName: "checkmark").font(.caption2.weight(.bold)).foregroundStyle(.green) }
                }
            }
            .buttonStyle(.plain)
            if open { PhoneMarkdownText(text: out, font: .footnote).padding(.leading, 26) }
        }
    }
}

// MARK: - Questions and notices

struct PhoneQuestionCard: View {
    let question: PhoneQuestion
    let readonly: Bool
    let answer: (String, String) -> Void
    @PhoneViewState private var text = ""

    var body: some View {
        if question.answered {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "questionmark.bubble").font(.footnote).foregroundStyle(.secondary)
                Text(question.prompt).font(.footnote).foregroundStyle(.secondary).lineLimit(2)
                Spacer(minLength: 4)
                Text(question.answer.map { "已回答：\($0)" } ?? "已回答").font(.caption).foregroundStyle(.secondary)
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                Label("智能体在问你", systemImage: "questionmark.bubble").font(.caption.weight(.semibold)).foregroundStyle(.blue)
                PhoneMarkdownText(text: question.prompt)
                if !readonly {
                    ForEach(Array(question.options.enumerated()), id: \.offset) { i, option in
                        Button { answer(question.qid, option) } label: {
                            HStack(spacing: 10) {
                                Text("\(i + 1)").font(.caption.monospacedDigit().weight(.semibold)).frame(width: 22, height: 22)
                                    .background(Color.blue.opacity(0.12), in: RoundedRectangle(cornerRadius: 6))
                                Text(option).font(.subheadline).multilineTextAlignment(.leading)
                                Spacer(minLength: 0)
                            }
                            .padding(8).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
                        .accessibilityIdentifier("chat-option-\(i + 1)")
                    }
                    HStack {
                        TextField(question.options.isEmpty ? "输入回答后按回车" : "或者输入别的回答", text: $text)
                            .textFieldStyle(.roundedBorder).onSubmit { send() }
                        Button("回答") { send() }.buttonStyle(.borderedProminent).disabled(text.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                }
            }
            .padding(12)
            .background(Color.blue.opacity(0.05), in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.blue.opacity(0.25)))
        }
    }
    private func send() {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !value.isEmpty { answer(question.qid, value) }
    }
}

struct PhoneNoticeRow: View {
    let level: String
    let text: String
    var body: some View {
        Label(text, systemImage: level == "error" ? "exclamationmark.circle" : level == "warn" ? "exclamationmark.triangle" : "info.circle")
            .font(.caption).foregroundStyle(level == "error" ? Color.red : level == "warn" ? Color.orange : Color.secondary)
    }
}

// MARK: - The end of a turn

/// Under a finished turn: how it ended, how long it took, and copying the reply; after a stop the
/// person can ask the agent to continue.
struct PhoneTurnFoot: View {
    let turn: PhoneChatTurn
    let stopped: Bool
    let readonly: Bool
    let resume: () -> Void

    var body: some View {
        if turn.status != "running" {
            VStack(alignment: .leading, spacing: 6) {
                if turn.status == "failed" {
                    PhoneNoticeRow(level: "error", text: "失败" + (turn.reason.map { "：\($0)" } ?? ""))
                }
                if turn.status == "interrupted", stopped, !readonly {
                    HStack {
                        Text("你停止了这一轮。" + (turn.reason.map { " \($0)" } ?? "")).font(.footnote).foregroundStyle(.secondary)
                        Spacer()
                        Button { resume() } label: { Label("继续", systemImage: "play.fill") }
                            .buttonStyle(.bordered).controlSize(.small).accessibilityIdentifier("chat-continue")
                    }
                    .padding(10)
                    .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("chat-stopped-\(turn.n)")
                } else {
                    HStack(spacing: 10) {
                        Text(turn.ending ?? "").font(.caption).foregroundStyle(turn.status == "completed" ? Color.secondary : Color.orange)
                            .accessibilityIdentifier("chat-turn-end-\(turn.n)")
                        if let tokens = tokens { Text("\(tokens)k tokens").font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
                        if !turn.reply.isEmpty {
                            Button { phoneCopy(turn.reply) } label: { Image(systemName: "doc.on.doc") }
                                .buttonStyle(.plain).font(.caption).foregroundStyle(.secondary).accessibilityLabel("复制回复")
                        }
                    }
                }
            }
        }
    }
    private var tokens: String? {
        guard let output = turn.usage?.output else { return nil }
        return String(format: "%.1f", ((turn.usage?.input ?? 0) + output) / 1000)
    }
}

/// While the turn runs: what the agent does now, and for how long.
struct PhoneWorkingRow: View {
    let label: String
    let since: Double?
    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(label).font(.footnote).foregroundStyle(.secondary)
            if let since { PhoneElapsed(start: since, end: nil, running: true) }
        }
        .accessibilityIdentifier("chat-working")
    }
}

/// How full the agent's context window is; red past 90 %.
struct PhoneContextRing: View {
    let usage: PhoneContextUsage
    var body: some View {
        let p = usage.fraction
        HStack(spacing: 4) {
            ZStack {
                Circle().stroke(Color.secondary.opacity(0.25), lineWidth: 2.5)
                Circle().trim(from: 0, to: p).stroke(p >= 0.9 ? Color.red : Color.blue, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .rotationEffect(.degrees(-90))
            }
            .frame(width: 16, height: 16)
            Text("\(Int((p * 100).rounded()))%").font(.caption2.monospacedDigit()).foregroundStyle(p >= 0.9 ? Color.red : Color.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("上下文窗口：已用 \(Self.tokens(usage.used)) / \(Self.tokens(usage.window)) tokens（\(Int((p * 100).rounded()))%）")
        .accessibilityIdentifier("chat-context")
    }
    static func tokens(_ n: Double) -> String {
        n >= 1_000_000 ? String(format: "%.1fM", n / 1_000_000) : n >= 1000 ? "\(Int((n / 1000).rounded()))k" : "\(Int(n))"
    }
}

/// Puts text on the pasteboard.
func phoneCopy(_ text: String) {
    #if os(iOS)
    UIPasteboard.general.string = text
    #elseif os(macOS)
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
    #endif
}
