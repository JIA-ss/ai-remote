import SwiftUI

/// The host's coding-agent chats: the session list, and one conversation shown turn by turn.
public struct PhoneChatView: View {
    @Bindable var chat: PhoneChatStore
    let close: () -> Void

    public init(chat: PhoneChatStore, close: @escaping () -> Void) {
        self.chat = chat
        self.close = close
    }

    public var body: some View {
        NavigationStack {
            Group {
                if let session = chat.current { conversation(session) } else { sessionList }
            }
            .navigationTitle(chat.current.map { chat.runtimeName.isEmpty ? $0.name : chat.runtimeName } ?? "智能体")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                if chat.current != nil {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("会话", systemImage: "chevron.left") { Task { await chat.leave() } }
                            .accessibilityIdentifier("chat-back")
                    }
                }
                ToolbarItem(placement: .confirmationAction) { Button("完成", action: close) }
                if chat.current == nil {
                    ToolbarItem(placement: .primaryAction) {
                        Button("新对话", systemImage: "plus") { chat.showNew = true }
                            .accessibilityIdentifier("chat-new")
                    }
                }
            }
            .sheet(isPresented: $chat.showNew) { newChat }
        }
        .task { if chat.current == nil { await chat.loadSessions() } }
        .alert("出错了", isPresented: Binding(get: { chat.error != nil && !chat.showNew }, set: { if !$0 { chat.error = nil } })) {
            Button("好", role: .cancel) {}
        } message: { Text(chat.error ?? "") }
    }

    // MARK: Sessions

    private var sessionList: some View {
        List {
            if chat.sessions.isEmpty && !chat.loading {
                VStack(spacing: 8) {
                    Image(systemName: "bubble.left.and.text.bubble.right").font(.largeTitle).foregroundStyle(.secondary)
                    Text("还没有智能体会话").font(.headline)
                    Text("点右上角的 + 在这台电脑上启动 Claude Code、Codex 等智能体；桌面端或命令行启动的会话也会出现在这里。")
                        .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity).padding(.vertical, 24).listRowBackground(Color.clear)
            }
            ForEach(chat.sessions) { session in
                Button { Task { await chat.open(session) } } label: { sessionRow(session) }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("chat-session-\(session.id)")
            }
            if chat.loading { ProgressView().frame(maxWidth: .infinity) }
        }
        .refreshable { await chat.loadSessions() }
    }

    private func sessionRow(_ s: PhoneChatSession) -> some View {
        HStack(spacing: 12) {
            Circle().fill(stateColor(s.state)).frame(width: 8, height: 8).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(s.name).font(.headline).lineLimit(1)
                    Text(s.folder).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                }
                Text("\(phoneChatStateName(s.state)) · \(s.turns) 轮 · 由 \(s.byDevice.isEmpty ? "未知设备" : s.byDevice) 启动")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
        }
        .contentShape(Rectangle())
        .padding(.vertical, 4)
    }

    private func stateColor(_ state: String) -> Color {
        switch state {
        case "running", "starting", "interrupting": .blue
        case "idle": .green
        default: .gray
        }
    }

    // MARK: New chat

    private var newChat: some View {
        NavigationStack {
            Form {
                if let blocked = chat.startBlocked {
                    Section { Label(blocked, systemImage: "exclamationmark.triangle").foregroundStyle(.orange) }
                }
                Section("智能体") {
                    if chat.runtimes.isEmpty { ProgressView() }
                    Picker("智能体", selection: $chat.newRuntime) {
                        ForEach(chat.runtimes) { runtime in
                            Text(runtime.name + (runtime.note.map { "（\($0)）" } ?? "")).tag(runtime.id)
                                .selectionDisabled(!runtime.usable)
                        }
                    }
                    #if os(iOS)
                    .pickerStyle(.inline)
                    #endif
                    .labelsHidden()
                    .accessibilityIdentifier("chat-new-runtime")
                }
                Section {
                    TextField("文件夹", text: $chat.newCwd)
                        .font(.body.monospaced()).autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never)
                        #endif
                        .accessibilityIdentifier("chat-new-cwd")
                } header: { Text("在电脑上的哪个文件夹工作") } footer: { Text("电脑上的绝对路径，~ 是用户主目录。") }
                Section {
                    TextField("第一条消息（可不填）", text: $chat.newPrompt, axis: .vertical).lineLimit(1...6)
                        .accessibilityIdentifier("chat-new-prompt")
                } footer: {
                    Text("智能体以电脑上登录用户的身份运行，执行命令和改文件前不会再问。电脑的主人可能要先在电脑上同意。")
                }
            }
            .navigationTitle("新对话")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("取消") { chat.showNew = false }.disabled(chat.starting) }
                ToolbarItem(placement: .confirmationAction) {
                    if chat.starting { ProgressView() } else {
                        Button("启动") { Task { await chat.start() } }
                            .disabled(!chat.canStart || chat.newRuntime.isEmpty || chat.newCwd.trimmingCharacters(in: .whitespaces).isEmpty)
                            .accessibilityIdentifier("chat-new-start")
                    }
                }
            }
            .safeAreaInset(edge: .bottom) {
                if chat.starting {
                    Label("正在启动；如果电脑要求确认，请在电脑上允许。", systemImage: "hourglass")
                        .font(.footnote).foregroundStyle(.secondary).padding()
                }
            }
            .alert("没能启动", isPresented: Binding(get: { chat.showNew && chat.error != nil }, set: { if !$0 { chat.error = nil } })) {
                Button("好", role: .cancel) {}
            } message: { Text(chat.error ?? "") }
        }
        .interactiveDismissDisabled(chat.starting)
        .task { await chat.loadRuntimes() }
    }

    // MARK: Conversation

    private func conversation(_ session: PhoneChatSession) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    Text(session.cwd).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(2)
                        .textSelection(.enabled)
                    ForEach(chat.log.notices.indices, id: \.self) { i in
                        Label(chat.log.notices[i], systemImage: "info.circle").font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(chat.log.turns) { turn in turnView(turn) }
                    Color.clear.frame(height: 1).id("chat-bottom")
                }
                .padding(16)
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: chat.log.seq) { _, _ in proxy.scrollTo("chat-bottom", anchor: .bottom) }
            .safeAreaInset(edge: .bottom, spacing: 0) { footer(session) }
        }
    }

    @ViewBuilder private func turnView(_ turn: PhoneChatTurn) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if let text = turn.userText {
                HStack {
                    Spacer(minLength: 40)
                    VStack(alignment: .trailing, spacing: 3) {
                        Text(text).textSelection(.enabled)
                            .padding(.horizontal, 12).padding(.vertical, 8)
                            .background(Color.blue.opacity(0.15), in: RoundedRectangle(cornerRadius: 14))
                        if !turn.userBy.isEmpty { Text(turn.userBy).font(.caption2).foregroundStyle(.secondary) }
                    }
                }
            }
            ForEach(turn.blocks) { block in blockView(block) }
            if let ending = turn.ending {
                Text(ending).font(.caption).foregroundStyle(turn.status == "completed" ? Color.secondary : Color.orange)
                    .accessibilityIdentifier("chat-turn-end-\(turn.n)")
            } else if turn.n == chat.log.turns.last?.n && chat.log.busy {
                HStack(spacing: 6) { ProgressView().controlSize(.small); Text("工作中…").font(.caption).foregroundStyle(.secondary) }
            }
        }
    }

    @ViewBuilder private func blockView(_ block: PhoneChatBlock) -> some View {
        switch block {
        case .text(_, let text):
            Text(text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
        case .thought(_, let text, let started, let ended):
            DisclosureGroup {
                Text(text).font(.footnote).foregroundStyle(.secondary).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } label: {
                Label(ended > started ? String(format: "思考 · %.0f 秒", max(1, (ended - started) / 1000)) : "思考",
                      systemImage: "brain").font(.footnote).foregroundStyle(.secondary)
            }
        case .tool(let tool):
            HStack(spacing: 8) {
                toolStatus(tool.status).frame(width: 16)
                Text(tool.title.isEmpty ? tool.kind : tool.title).font(.footnote.monospaced()).lineLimit(1).truncationMode(.middle)
            }
            .foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)
        case .question(_, let prompt):
            Label("智能体在问：\(prompt)", systemImage: "questionmark.bubble").font(.footnote)
        case .notice(let level, let text):
            Label(text, systemImage: level == "error" ? "exclamationmark.triangle" : "info.circle")
                .font(.caption).foregroundStyle(level == "info" ? Color.secondary : Color.orange)
        }
    }

    @ViewBuilder private func toolStatus(_ status: String) -> some View {
        switch status {
        case "completed": Image(systemName: "checkmark.circle").foregroundStyle(.green).accessibilityLabel("完成")
        case "failed": Image(systemName: "xmark.circle").foregroundStyle(.red).accessibilityLabel("失败")
        default: ProgressView().controlSize(.mini).accessibilityLabel("运行中")
        }
    }

    // MARK: Footer

    @ViewBuilder private func footer(_ session: PhoneChatSession) -> some View {
        VStack(spacing: 8) {
            HStack(spacing: 6) {
                Circle().fill(chat.connected ? stateColor(chat.log.state) : Color.orange).frame(width: 6, height: 6)
                Text(chat.connected ? phoneChatStateName(chat.log.state) : "未连接").font(.caption.weight(.medium))
                if !chat.log.queued.isEmpty { Text("· 排队 \(chat.log.queued.count) 条").font(.caption).foregroundStyle(.secondary) }
                Spacer()
            }
            .accessibilityIdentifier("chat-state")
            if let reason = chat.disconnected {
                HStack {
                    Text(reason).font(.footnote).foregroundStyle(.secondary).lineLimit(2)
                    Spacer()
                    Button("重新连接") { Task { await chat.resume() } }.buttonStyle(.bordered)
                }
            } else if chat.role == "ro" {
                // Older hosts give write tickets only to the device that started the session.
                HStack {
                    Text("由 \(session.byDevice.isEmpty ? "另一台设备" : session.byDevice) 启动，这台 iPhone 只能查看。")
                        .font(.footnote).foregroundStyle(.secondary)
                    Spacer()
                    if chat.log.busy && chat.connected {
                        Button("停止", role: .destructive) { Task { await chat.interrupt() } }.buttonStyle(.bordered)
                            .accessibilityIdentifier("chat-stop")
                    }
                }
            } else if chat.log.state == "ended" {
                Text("这个会话已结束。").font(.footnote).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
            } else if chat.canWrite {
                composer
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.bar)
    }

    @PhoneViewState private var composing = false

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 8) {
            PhoneChatField(text: $chat.draft, composing: $composing) { Task { await chat.send() } }
                .frame(minHeight: 36)
                .padding(.horizontal, 10).padding(.vertical, 4)
                .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 18))
                .accessibilityIdentifier("chat-input")
            if chat.log.busy {
                Button { Task { await chat.interrupt() } } label: {
                    Image(systemName: "stop.circle.fill").font(.title)
                }
                .tint(.red).disabled(chat.log.state == "interrupting")
                .accessibilityLabel("停止").accessibilityIdentifier("chat-stop")
            }
            Button { Task { await chat.send() } } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title)
            }
            // While the input method is composing, the text is not final: nothing is sent.
            .disabled(composing || chat.sending || chat.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel(chat.log.busy ? "排队发送" : "发送").accessibilityIdentifier("chat-send")
        }
    }
}

#if os(iOS)
import UIKit

/// A growing text field that reports input-method composition (marked text): Return sends only when
/// nothing is being composed, otherwise it belongs to the input method.
struct PhoneChatField: UIViewRepresentable {
    @Binding var text: String
    @Binding var composing: Bool
    let submit: () -> Void

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.font = .preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.backgroundColor = .clear
        view.isScrollEnabled = false
        view.returnKeyType = .send
        view.textContainerInset = UIEdgeInsets(top: 8, left: 0, bottom: 8, right: 0)
        view.delegate = context.coordinator
        view.accessibilityLabel = "给智能体的消息"
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        context.coordinator.parent = self
        // Never overwrite the field while the input method holds marked text.
        if view.text != text && view.markedTextRange == nil { view.text = text }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        let width = proposal.width ?? 280
        let fit = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        let line = uiView.font?.lineHeight ?? 20
        let maxHeight = line * 5 + 16
        uiView.isScrollEnabled = fit.height > maxHeight
        return CGSize(width: width, height: min(fit.height, maxHeight))
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: PhoneChatField
        init(_ parent: PhoneChatField) { self.parent = parent }

        func textViewDidChange(_ view: UITextView) {
            let marked = view.markedTextRange != nil
            if parent.composing != marked { parent.composing = marked }
            if !marked { parent.text = view.text }
        }

        func textViewDidChangeSelection(_ view: UITextView) {
            let marked = view.markedTextRange != nil
            if parent.composing != marked { parent.composing = marked }
        }

        func textView(_ view: UITextView, shouldChangeTextIn range: NSRange, replacementText replacement: String) -> Bool {
            guard replacement == "\n" else { return true }
            if view.markedTextRange != nil { return true }
            parent.submit()
            return false
        }
    }
}
#else
/// The same field off iOS (the macOS build checks the package): a plain multi-line text field.
struct PhoneChatField: View {
    @Binding var text: String
    @Binding var composing: Bool
    let submit: () -> Void
    var body: some View {
        TextField("给智能体的消息", text: $text, axis: .vertical).lineLimit(1...5).onSubmit(submit)
    }
}
#endif
