import SwiftUI
import PhotosUI

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
            // By project, as Orca and Codex list threads: the folder with the latest activity first.
            ForEach(phoneSessionsByFolder(chat.sessions), id: \.folder) { group in
                Section {
                    ForEach(group.sessions) { session in
                        Button { Task { await chat.open(session) } } label: { sessionRow(session) }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("chat-session-\(session.id)")
                    }
                } header: {
                    HStack(spacing: 8) {
                        Image(systemName: "folder").foregroundStyle(.secondary)
                        PhoneFolderLabel(path: group.folder, home: chat.home).textCase(nil)
                        Spacer()
                        Text("\(group.sessions.count)").font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("chat-project-\(phoneFolderName(group.folder))")
                }
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
                    Spacer(minLength: 4)
                    if s.lastActivity > 0 {
                        Text(phoneAgo(s.lastActivity, now: Date().timeIntervalSince1970 * 1000)).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Text("\(phoneChatStateName(s.state, turns: s.turns)) · \(s.turns) 轮 · 由 \(s.byDevice.isEmpty ? "未知设备" : s.byDevice) 启动")
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
                    NavigationLink {
                        PhoneFolderPicker(chat: chat, selection: $chat.newCwd)
                    } label: {
                        HStack {
                            Image(systemName: "folder").foregroundStyle(.secondary)
                            if chat.newCwd.isEmpty { Text("选择文件夹").foregroundStyle(.secondary) }
                            else { PhoneFolderLabel(path: chat.newCwd, home: chat.home) }
                        }
                    }
                    .accessibilityIdentifier("chat-new-cwd")
                } header: { Text("在电脑上的哪个文件夹工作") } footer: { Text("最近用过的文件夹在最前面，也可以浏览电脑上的文件夹或直接输入路径。") }
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
        .task {
            // The last folder used here, as the desktop does; the home folder the first time.
            if chat.newCwd == "~", let last = chat.recentFolders.first { chat.newCwd = last }
            await chat.loadRuntimes()
        }
    }

    // MARK: Conversation

    private func conversation(_ session: PhoneChatSession) -> some View {
        let stopped = chat.log.stoppedTurn?.n
        let readonly = !chat.canWrite
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    Text(session.cwd).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(2)
                        .textSelection(.enabled)
                    ForEach(Array(chat.log.notices.enumerated()), id: \.offset) { _, notice in
                        PhoneNoticeRow(level: notice.level, text: phoneNoticeText(code: notice.code, text: notice.text))
                    }
                    ForEach(chat.log.turns) { turn in
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(phoneTurnRows(turn)) { row in
                                PhoneChatRowView(row: row, readonly: readonly) { qid, text in Task { await chat.answer(qid, text: text) } }
                            }
                            if turn.status != "running" { PhoneTurnPreviews(chat: chat, files: phoneTurnFiles(turn.blocks)) }
                            PhoneTurnFoot(turn: turn, stopped: turn.n == stopped, readonly: readonly) { Task { await chat.continueStopped() } }
                            if turn.status == "running", turn.n == chat.log.turns.last?.n, chat.log.busy {
                                PhoneWorkingRow(label: workingLabel, since: chat.log.phase.since)
                            }
                        }
                    }
                    if chat.log.busy, chat.log.turns.last?.status != "running" {
                        PhoneWorkingRow(label: workingLabel, since: nil)
                    }
                    ForEach(chat.outgoing) { o in outgoingRow(o) }
                    Color.clear.frame(height: 1).id("chat-bottom")
                }
                .padding(16)
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: chat.log.seq) { _, _ in proxy.scrollTo("chat-bottom", anchor: .bottom) }
            .onChange(of: chat.outgoing.count) { _, _ in proxy.scrollTo("chat-bottom", anchor: .bottom) }
            .safeAreaInset(edge: .bottom, spacing: 0) { footer(session) }
        }
        .environment(\.phoneSentImages, chat.sentImages)
        .environment(\.phonePreviewOpen) { path in previewing = PhonePreviewTarget(path: path) }
        // A link in a reply that names a file the agent wrote opens its preview; web links open as usual.
        .environment(\.openURL, OpenURLAction { url in
            if let path = phoneFileTarget(url.absoluteString) { previewing = PhonePreviewTarget(path: path); return .handled }
            return .systemAction
        })
        .sheet(item: $previewing) { target in PhonePreviewSheet(chat: chat, path: target.path) { previewing = nil } }
    }

    @PhoneViewState private var previewing: PhonePreviewTarget?

    /// A message on its way: "发送中…", or why it did not arrive with 重新发送 / 丢弃.
    private func outgoingRow(_ o: PhoneOutgoing) -> some View {
        HStack {
            Spacer(minLength: 40)
            VStack(alignment: .trailing, spacing: 4) {
                PhoneAttachmentStrip(attachments: [], pictures: o.images)
                if !o.text.isEmpty {
                    Text(o.text).padding(.horizontal, 12).padding(.vertical, 8)
                        .background(Color.blue.opacity(o.state == "failed" ? 0.06 : 0.1), in: RoundedRectangle(cornerRadius: 14))
                        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(o.state == "failed" ? Color.orange.opacity(0.6) : .clear))
                }
                if o.state == "failed" {
                    HStack(spacing: 10) {
                        Label("未送达：\(phoneOutgoingError(o.error))", systemImage: "exclamationmark.circle").font(.caption).foregroundStyle(.orange)
                        Button("重新发送") { Task { await chat.retry(o) } }.font(.caption.weight(.medium)).disabled(!chat.canWrite)
                            .accessibilityIdentifier("chat-retry")
                        Button("丢弃") { chat.discard(o) }.font(.caption).foregroundStyle(.secondary)
                            .accessibilityIdentifier("chat-discard")
                    }
                    .buttonStyle(.borderless)
                } else {
                    Text("发送中…").font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat-outgoing")
    }

    /// What the agent is doing now, in words.
    private var workingLabel: String {
        switch chat.log.phase.what {
        case "starting": return "启动中…"
        case "stopping": return "正在停止…"
        case "asking": return "等你回答"
        case "writing": return "正在回复"
        case "tool":
            if case .tool(let x)? = chat.log.turns.last?.blocks.last {
                let label = phoneToolLabel(x)
                return "正在\(label.verb) \(label.target)".trimmingCharacters(in: .whitespaces)
            }
            return "工作中"
        default: return "思考中"
        }
    }

    // MARK: Footer

    @ViewBuilder private func footer(_ session: PhoneChatSession) -> some View {
        VStack(spacing: 8) {
            if !chat.log.queued.isEmpty {
                VStack(spacing: 6) {
                    ForEach(chat.log.queued) { q in queuedCard(q) }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("chat-queue")
            }
            HStack(spacing: 6) {
                Circle().fill(chat.connected ? stateColor(chat.log.state) : Color.orange).frame(width: 6, height: 6)
                Text(chat.connected ? phoneChatStateName(chat.log.state, turns: chat.log.turns.count) : "未连接").font(.caption.weight(.medium))
                Spacer()
                if let usage = chat.log.context { PhoneContextRing(usage: usage) }
            }
            // A container keeps its own id; on a plain stack it would overwrite the ring's.
            .accessibilityElement(children: .contain)
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
        .alert("编辑排队的消息", isPresented: Binding(get: { editing != nil }, set: { if !$0 { editing = nil } })) {
            TextField("消息", text: $editText)
            Button("取消", role: .cancel) { editing = nil }
            Button("保存") {
                if let q = editing, editText.trimmingCharacters(in: .whitespacesAndNewlines) != q.text {
                    Task { await chat.editQueued(q.id, text: editText) }
                }
                editing = nil
            }
        } message: { Text("改好后它会重新排到队尾。") }
    }

    @PhoneViewState private var composing = false
    @PhoneViewState private var editing: PhoneQueued?
    @PhoneViewState private var editText = ""

    /// A message waiting for the running turn to end: edit it or take it back.
    private func queuedCard(_ q: PhoneQueued) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "text.append").font(.footnote).foregroundStyle(.secondary)
            Text(q.text).font(.footnote).lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
            if chat.canWrite {
                Button { editText = q.text; editing = q } label: { Image(systemName: "pencil") }
                    .accessibilityLabel("编辑").accessibilityIdentifier("chat-queued-edit")
                Button { Task { await chat.dequeue(q.id) } } label: { Image(systemName: "trash") }
                    .accessibilityLabel("移出队列").accessibilityIdentifier("chat-queued-remove")
            }
        }
        .buttonStyle(.borderless)
        .padding(10)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
    }

    private var placeholder: String {
        switch chat.sendMode {
        case "queue": "这一轮结束后发给智能体"
        default: "发消息给 \(chat.runtimeName.isEmpty ? "智能体" : chat.runtimeName)"
        }
    }

    @PhoneViewState private var caret = 0
    @PhoneViewState private var setCaret: Int?
    @PhoneViewState private var menuActive = 0
    @PhoneViewState private var mentionFiles: [String] = []
    @PhoneViewState private var mentionError: String?
    @PhoneViewState private var pickingPhotos = false
    @PhoneViewState private var photoItems: [PhotosPickerItem] = []
    @PhoneViewState private var takingPhoto = false

    /// The `/` or `@` menu the caret is in, if any.
    private var autocomplete: PhoneAutocomplete? { composing ? nil : phoneAutocompleteAt(chat.draft, caret: caret) }

    private var composer: some View {
        let empty = chat.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && chat.attachments.isEmpty
        return VStack(alignment: .leading, spacing: 6) {
            if let ac = autocomplete { menu(ac) }
            if !chat.attachments.isEmpty { draftImages }
            HStack(alignment: .bottom, spacing: 6) {
                attachButton
                PhoneChatField(text: $chat.draft, composing: $composing, caret: $caret, setCaret: $setCaret) { Task { await chat.send() } }
                    .frame(minHeight: 36)
                    .accessibilityIdentifier("chat-input")
                    .overlay(alignment: .leading) {
                        if chat.draft.isEmpty {
                            Text(placeholder).foregroundStyle(.tertiary).padding(.leading, 5).lineLimit(1).allowsHitTesting(false)
                                .accessibilityHidden(true)
                        }
                    }
                if chat.log.busy {
                    Button { Task { await chat.interrupt() } } label: {
                        Image(systemName: "stop.circle.fill").font(.title2)
                    }
                    .tint(.red).disabled(chat.log.state == "interrupting")
                    .padding(.bottom, 4)
                    .accessibilityLabel("停止").accessibilityIdentifier("chat-stop")
                }
                if !chat.log.busy || !empty {
                    Button { Task { await chat.send() } } label: {
                        Image(systemName: "arrow.up.circle.fill").font(.title2)
                    }
                    // While the input method is composing, the text is not final: nothing is sent.
                    .disabled(composing || chat.sending || empty)
                    .padding(.bottom, 4)
                    .contextMenu {
                        if chat.log.busy {
                            Button("停止本轮并立即发送", systemImage: "bolt") { Task { await chat.send(interrupt: true) } }
                        }
                    }
                    .accessibilityLabel("发送")
                    .accessibilityIdentifier("chat-send")
                }
            }
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 20))
            if let note = chat.attachNote {
                Text(note).font(.caption).foregroundStyle(.orange).accessibilityIdentifier("chat-attach-note")
            }
        }
        .photosPicker(isPresented: $pickingPhotos, selection: $photoItems, maxSelectionCount: max(1, phoneMaxImages - chat.attachments.count),
                      matching: .images, preferredItemEncoding: .compatible)
        .onChange(of: photoItems) { _, items in
            guard !items.isEmpty else { return }
            photoItems = []
            Task {
                for item in items {
                    guard let data = try? await item.loadTransferable(type: Data.self) else { chat.attachNote = "读不出这张图片"; continue }
                    if let image = phonePrepareImage(data, name: "photo") { chat.attach(image) } else { chat.attachNote = "读不出这张图片" }
                }
            }
        }
        #if os(iOS)
        .fullScreenCover(isPresented: $takingPhoto) {
            PhoneCameraPicker { data in
                takingPhoto = false
                if let data, let image = phonePrepareImage(data, name: "camera") { chat.attach(image) }
            }
            .ignoresSafeArea()
        }
        #endif
        .task(id: autocomplete.map { $0.mode + ":" + $0.query }) {
            menuActive = 0
            guard let ac = autocomplete, ac.mode == "mention" else { return }
            try? await Task.sleep(for: .milliseconds(120))
            guard !Task.isCancelled else { return }
            do { mentionFiles = try await chat.files(ac.query); mentionError = nil }
            catch { mentionFiles = []; mentionError = error.localizedDescription }
        }
    }

    /// The + on the left: a photo from the library, or one taken now.
    private var attachButton: some View {
        Menu {
            Button("从相册选择", systemImage: "photo.on.rectangle") { pickingPhotos = true }
                .accessibilityIdentifier("chat-attach-library")
            #if os(iOS)
            Button("拍照", systemImage: "camera") { takingPhoto = true }
                .disabled(!UIImagePickerController.isSourceTypeAvailable(.camera))
                .accessibilityIdentifier("chat-attach-camera")
            #endif
        } label: {
            Image(systemName: "plus").font(.body.weight(.semibold)).frame(width: 30, height: 30)
                .background(Color.secondary.opacity(0.15), in: Circle())
        }
        .disabled(!chat.takesImages || chat.attachments.count >= phoneMaxImages)
        .padding(.bottom, 3)
        .accessibilityLabel(chat.takesImages ? "添加图片" : "这个智能体不收图片")
        .accessibilityIdentifier("chat-attach")
    }

    /// The images waiting to go with the message.
    private var draftImages: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(chat.attachments) { img in
                    ZStack(alignment: .topTrailing) {
                        if let cg = phoneDecodeImage(img.data, maxPixels: 200) {
                            Image(decorative: cg, scale: 1).resizable().scaledToFill().frame(width: 56, height: 56)
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                        }
                        Button { chat.detach(img.id) } label: {
                            Image(systemName: "xmark.circle.fill").symbolRenderingMode(.palette).foregroundStyle(.white, .black.opacity(0.6))
                        }
                        .buttonStyle(.plain).offset(x: 5, y: -5)
                        .accessibilityLabel("移除 \(img.name)").accessibilityIdentifier("chat-attachment-remove")
                    }
                    .padding(.top, 5).padding(.trailing, 5)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat-attachments")
    }

    /// The `/` commands and skills, or the `@` files, above the composer.
    @ViewBuilder private func menu(_ ac: PhoneAutocomplete) -> some View {
        let items: [(id: String, title: String, detail: String?, insert: String, section: String)] = ac.mode == "slash"
            ? phoneMatchCommands(chat.commands, ac.query).map { c in
                (c.id, c.kind == "skill" && !c.insert.hasPrefix("/") ? c.insert : "/" + c.name, c.description, c.insert, c.kind == "skill" ? "技能" : "命令")
            }
            : mentionFiles.map { f in (f, phoneBaseName(f), f == phoneBaseName(f) ? nil : f, phoneMentionText(f), "文件") }
        VStack(alignment: .leading, spacing: 0) {
            if items.isEmpty {
                Text(ac.mode == "slash" ? (chat.commands.isEmpty ? "这个智能体还没有报告它的命令" : "没有匹配的命令")
                     : (mentionError.map { "列不出文件：\($0)" } ?? "没有匹配的文件"))
                    .font(.footnote).foregroundStyle(.secondary).padding(10)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(items.prefix(50).enumerated()), id: \.element.id) { i, item in
                        if i == 0 || items[i - 1].section != item.section, ac.mode == "slash" {
                            Text(item.section).font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
                                .padding(.horizontal, 10).padding(.top, 8).padding(.bottom, 2)
                        }
                        Button { choose(ac, item.insert) } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(item.title).font(.subheadline.monospaced()).lineLimit(1)
                                if let detail = item.detail, !detail.isEmpty {
                                    Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 10).padding(.vertical, 6)
                            .background(i == menuActive ? Color.accentColor.opacity(0.12) : .clear)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("chat-ac-\(item.title)")
                    }
                }
            }
            // As tall as its rows, up to about five of them.
            .frame(height: min(220, CGFloat(min(items.count, 50)) * (ac.mode == "slash" ? 48 : 50) + (ac.mode == "slash" ? 26 * CGFloat(Set(items.map(\.section)).count) : 0)))
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.secondary.opacity(0.2)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(ac.mode == "slash" ? "chat-slash-menu" : "chat-mention-menu")
    }

    private func choose(_ ac: PhoneAutocomplete, _ insert: String) {
        let applied = phoneApplyChoice(chat.draft, ac, insert: insert)
        chat.draft = applied.text
        caret = applied.caret
        setCaret = applied.caret
    }
}

/// The file a preview sheet shows.
struct PhonePreviewTarget: Identifiable, Equatable {
    let path: String
    var id: String { path }
}

#if os(iOS)
import UIKit

/// A growing text field that reports input-method composition (marked text): Return sends only when
/// nothing is being composed, otherwise it belongs to the input method.
struct PhoneChatField: UIViewRepresentable {
    @Binding var text: String
    @Binding var composing: Bool
    /// Where the caret is (UTF-16 offset), for the `/` and `@` menus; `setCaret` moves it once.
    @Binding var caret: Int
    @Binding var setCaret: Int?
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
        if let at = setCaret, view.markedTextRange == nil {
            view.selectedRange = NSRange(location: min(at, (view.text as NSString).length), length: 0)
            DispatchQueue.main.async { setCaret = nil }
        }
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
            parent.caret = view.selectedRange.location
        }

        func textViewDidChangeSelection(_ view: UITextView) {
            let marked = view.markedTextRange != nil
            if parent.composing != marked { parent.composing = marked }
            let at = view.selectedRange.location
            if parent.caret != at { parent.caret = at }
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
    @Binding var caret: Int
    @Binding var setCaret: Int?
    let submit: () -> Void
    var body: some View {
        TextField("给智能体的消息", text: $text, axis: .vertical).lineLimit(1...5).onSubmit(submit)
            .onChange(of: text) { _, t in caret = (t as NSString).length }
    }
}
#endif

#if os(iOS)
/// The camera, for a photo to send (the photo library goes through PhotosPicker).
struct PhoneCameraPicker: UIViewControllerRepresentable {
    let done: (Data?) -> Void
    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ picker: UIImagePickerController, context: Context) {}
    func makeCoordinator() -> Coordinator { Coordinator(done) }
    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let done: (Data?) -> Void
        init(_ done: @escaping (Data?) -> Void) { self.done = done }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            done((info[.originalImage] as? UIImage)?.jpegData(compressionQuality: 0.9))
        }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { done(nil) }
    }
}
#endif
