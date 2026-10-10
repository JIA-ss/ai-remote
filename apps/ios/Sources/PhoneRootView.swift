import SwiftUI

public struct PhoneRootView: View {
    @Bindable var store: PhoneStore
    public init(store: PhoneStore) { self.store = store }
    public var body: some View {
        NavigationStack {
            Group {
                if let host = store.selected { session(host) }
                else { computers }
            }
            .navigationTitle(store.selected?.name ?? "ai-remote")
            .toolbar {
                if store.selected != nil {
                    ToolbarItem(placement: .cancellationAction) { Button("电脑", systemImage: "chevron.left") { Task { await store.leaveSession() } } }
                    ToolbarItem(placement: .primaryAction) { Button("活动", systemImage: "list.bullet.rectangle") { store.showActivity = true } }
                } else {
                    ToolbarItem(placement: .primaryAction) { Button("配对电脑", systemImage: "plus") { store.pairSheet = true } }
                }
            }
            .sheet(isPresented: $store.pairSheet) { pairing }
            .sheet(isPresented: $store.showActivity) { activity }
            .modifier(PhoneErrorAlert(store: store, enabled: !store.pairSheet && !store.showActivity))
        }
        .tint(.blue)
    }
    private var computers: some View {
        List {
            if store.hosts.isEmpty {
                ContentUnavailableView("你的电脑，从这里开始", systemImage: "desktopcomputer", description: Text("在电脑上启动 ai-remote 被控端，生成配对票据，再用这台 iPhone 配对。"))
                Button("配对一台电脑") { store.pairSheet = true }
            }
            ForEach(store.hosts) { host in
                Button { Task { await store.connect(host) } } label: {
                    HStack(spacing: 14) {
                        Image(systemName: "desktopcomputer").font(.title2)
                        VStack(alignment: .leading, spacing: 5) {
                            Text(host.name).font(.headline).foregroundStyle(.primary)
                            Text(levelName(host.level)).font(.subheadline).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Image(systemName: "chevron.right").foregroundStyle(.tertiary)
                    }.padding(.vertical, 8)
                }
            }
            Section {
                Label("配对身份保存在这台 iPhone", systemImage: "lock.shield")
                Text("画面和操作通过加密连接传输。你可以随时接管或暂停智能体。退出控制页面会尝试交还控制。")
                    .font(.footnote).foregroundStyle(.secondary)
            }
        }
        .refreshable { do { try await store.refresh() } catch { store.error = error.localizedDescription } }
        .overlay { if store.loading { ProgressView("连接中…") } }
    }
    private func session(_ host: PhoneHost) -> some View {
        VStack(spacing: 0) {
            HStack {
                Circle().fill(store.ownsControls ? .orange : .blue).frame(width: 8, height: 8)
                Text(store.ownsControls ? "你正在操作" : store.driver == "paused" ? "智能体已暂停" : "正在观看")
                Spacer()
                if store.ownsControls { Button("交还") { Task { await store.setTurn("hand_back") } } }
                else { Button("接管") { Task { await store.setTurn("take_over") } }.disabled(!store.mayControl || !store.connected) }
            }.font(.subheadline.weight(.medium)).padding()
            screenView
            if !store.ownsControls {
                Text("接管后，可轻点、拖动和输入文字").font(.footnote).foregroundStyle(.secondary).padding()
            } else {
                HStack {
                    TextField("输入到电脑", text: $store.text).textFieldStyle(.roundedBorder).onSubmit { store.sendText() }
                    Button("发送", systemImage: "arrow.up.circle.fill") { store.sendText() }.labelStyle(.iconOnly).disabled(store.text.isEmpty)
                }.padding(.horizontal).padding(.top, 12)
                HStack {
                    ForEach(["Return", "Tab", "Escape", "Backspace"], id: \.self) { key in
                        Button(key) { store.input(tool: "keyboard", args: ["keys": .string(key)]) }.font(.caption)
                    }
                    Spacer()
                    Button("↑") { scroll("up") }; Button("↓") { scroll("down") }
                }.padding()
            }
            HStack {
                Button(store.driver == "paused" ? "继续智能体" : "暂停智能体", systemImage: store.driver == "paused" ? "play.fill" : "pause.fill") {
                    Task { await store.setTurn(store.driver == "paused" ? "resume" : "pause") }
                }
                Spacer()
                Text(levelName(store.level)).foregroundStyle(.secondary)
            }.font(.footnote).padding()
        }
    }
    private var screenView: some View {
        GeometryReader { area in
            if let picture = store.image {
                let ratio = CGFloat(picture.width) / CGFloat(picture.height)
                let width = min(area.size.width, area.size.height * ratio), height = width / ratio
                Image(decorative: picture, scale: 1).resizable().frame(width: width, height: height)
                    .contentShape(Rectangle())
                    .gesture(DragGesture(minimumDistance: 0).onEnded { gesture in
                        guard let screen = store.screen,
                              let start = screen.point(u: gesture.startLocation.x / width, v: gesture.startLocation.y / height),
                              let end = screen.point(u: gesture.location.x / width, v: gesture.location.y / height) else { return }
                        let drag = hypot(gesture.translation.width, gesture.translation.height) > 10
                        var args: [String: JSONValue] = ["action": .string(drag ? "drag" : "click"), "x": .number(start.0), "y": .number(start.1), "space": .string("screen")]
                        if drag { args["to_x"] = .number(end.0); args["to_y"] = .number(end.1) }
                        store.input(tool: "mouse", args: args)
                    }).position(x: area.size.width / 2, y: area.size.height / 2)
            } else {
                VStack(spacing: 16) { ProgressView(); Text("正在连接 \(store.selected?.name ?? "电脑")"); Text("第一帧到达后显示真实画面").font(.footnote).foregroundStyle(.secondary) }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }.background(Color.black.opacity(0.04)).frame(maxHeight: .infinity)
    }
    private func scroll(_ direction: String) {
        guard let screen = store.screen else { return }
        store.input(tool: "mouse", args: ["action": .string("scroll"), "direction": .string(direction), "amount": .number(3), "x": .number(screen.x + screen.width/2), "y": .number(screen.y + screen.height/2), "space": .string("screen")])
    }
    private var pairing: some View {
        NavigationStack {
            Form {
                Section("在电脑上生成票据") { Text("启动 ai-remote host，再运行 ai-remote pair-code --level control。").font(.callout); Text("可以通过通用剪贴板把票据复制到 iPhone。").font(.footnote).foregroundStyle(.secondary) }
                Section("一次性配对票据") {
                    TextEditor(text: $store.pairingCode).frame(minHeight: 130).font(.system(.footnote, design: .monospaced)).autocorrectionDisabled()
                        .ticketCapitalizationDisabled().accessibilityIdentifier("pairing-ticket")
                    Button("配对") { Task { await store.pair() } }.disabled(store.loading || store.pairingCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty).accessibilityIdentifier("pair-submit")
                }
                if let code = store.safetyCode { Section("核对安全码") { Text(code).font(.system(.title3, design: .monospaced)); Text("请确认电脑上显示的是相同的三个单词。配对票据使用一次后失效。").font(.footnote) } }
            }.navigationTitle("配对电脑").toolbar { ToolbarItem(placement: .cancellationAction) { Button("完成") { store.pairSheet = false } } }
        }.modifier(PhoneErrorAlert(store: store))
    }
    private var activity: some View {
        NavigationStack {
            List {
                if !store.pending.isEmpty {
                    Section("待批准") {
                        ForEach(store.pending, id: \.text) { request in
                            VStack(alignment: .leading, spacing: 8) {
                                Text(request["tool"].string ?? "请求").font(.headline)
                                Text(request["intent"].string ?? request["args_preview"].string ?? "")
                                Button("拒绝", role: .destructive) { Task { await store.deny(request["id"].string ?? "") } }
                            }
                        }
                        Text("当前 iPhone 版本尚未实现 Face ID 审批签名。允许操作请在被控电脑确认。").font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Section("最近活动") {
                    ForEach(Array(store.activity.reversed().enumerated()), id: \.offset) { _, event in
                        VStack(alignment: .leading, spacing: 4) { Text(event["tool"].string ?? event["kind"].string ?? "活动").font(.headline); Text(event["intent"].string ?? event["outcome"].string ?? event["decision"].string ?? event["task"].string ?? "").font(.footnote).foregroundStyle(.secondary) }
                    }
                }
            }.navigationTitle("活动与审批").toolbar { ToolbarItem(placement: .cancellationAction) { Button("完成") { store.showActivity = false } } }.task { await store.refreshPending() }
        }.modifier(PhoneErrorAlert(store: store))
    }
    private func levelName(_ level: String?) -> String { switch level { case "admin": "管理权限"; case "control": "控制权限"; case "observe": "仅观看"; default: "等待权限信息" } }
}

// Present errors from the visible sheet, so failed pairing/approval calls stay actionable.
private struct PhoneErrorAlert: ViewModifier {
    var store: PhoneStore
    var enabled = true
    func body(content: Content) -> some View {
        content.alert("需要关注", isPresented: Binding(get: { enabled && store.error != nil }, set: { if !$0 { store.error = nil } })) {
            Button("好", role: .cancel) { store.error = nil }
        } message: { Text(store.error ?? "") }
    }
}

private extension View {
    @ViewBuilder func ticketCapitalizationDisabled() -> some View {
        #if os(iOS)
        self.textInputAutocapitalization(.never)
        #else
        self
        #endif
    }
}
