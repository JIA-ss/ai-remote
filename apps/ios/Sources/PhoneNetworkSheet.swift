import SwiftUI

/// Settings > Network on the phone: the desktop app's page, for a device that only ever connects
/// out (it has no address of its own to give anyone).
struct PhoneNetworkSheet: View {
    @Bindable var store: PhoneStore
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("这台设备 怎样连接你的电脑。连接始终是端到端加密的。")
                        .font(.subheadline).foregroundStyle(.secondary)
                }
                Section("连接方式") {
                    modeRow(lan: false, title: "自动", detail: "在家、公司、外面都能连上。能直连就直连，连不上时经加密中继转发（中继看不到内容）。")
                    modeRow(lan: true, title: "仅限局域网", detail: "只和同一个网络（同一个 Wi‑Fi 或公司网络）里的电脑直连，不经过任何外部服务器。离开这个网络就连不上。电脑上也要选“仅限局域网”。")
                }
                Section {
                    Toggle(isOn: Binding(get: { network?.lanDiscovery ?? false },
                                         set: { on in Task { await store.changeNetwork(lanDiscovery: on) } })) {
                        option("在局域网里自动发现电脑", "电脑换了 IP 也能自动找到；在“仅限局域网”下，配对码也要靠它。只查找、不广播这台设备，但开着时大约每秒向整个局域网发一次查询，在公共网络里建议关闭。")
                    }
                    .disabled(disabled).accessibilityIdentifier("network-discovery")
                    if network?.lan != true {
                        Toggle(isOn: Binding(get: { network?.portMapping ?? false },
                                             set: { on in Task { await store.changeNetwork(portMapping: on) } })) {
                            option("让路由器开放端口（UPnP）", "家用路由器支持时，直连更容易成功。成功之前会每 20 到 26 秒询问一次路由器，公司网络里可能被当作可疑行为。")
                        }
                        .disabled(disabled).accessibilityIdentifier("network-upnp")
                    }
                } header: { Text("可选功能") } footer: {
                    Text(network?.locked == true
                         ? "已锁定为关闭：这台设备 的 ai-remote.toml 设置了 quiet = true。"
                         : "默认关闭，需要时再打开：它们会在你的连接之外往网络上发包。")
                }
                if network?.lan == true {
                    Section {
                        if store.hosts.isEmpty {
                            Text("还没有配对的电脑").foregroundStyle(.secondary)
                        }
                        ForEach(store.hosts) { host in PhoneHostAddressRow(store: store, host: host, port: network?.lanPort ?? 47800) }
                    } header: { Text("要连接的电脑") } footer: {
                        Text(network?.lanDiscovery == true
                             ? "按这里的地址去连；对方换了地址时，会在这个网络里自动重新找到。"
                             : "按这里的地址去连。配对时自动记下，连接期间自动更新；只有对方换了 IP 才需要修改。不写端口时用 \(String(network?.lanPort ?? 47800))。")
                    }
                }
                if store.applyingNetwork {
                    Section { ProgressView("正在按新设置重新连接…").frame(maxWidth: .infinity) }
                }
            }
            .navigationTitle("网络")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } } }
            .task { await store.loadNetwork() }
        }.modifier(PhoneErrorAlert(store: store))
    }

    private var network: PhoneNetwork? { store.network }
    private var disabled: Bool { network == nil || network?.locked == true || store.applyingNetwork }

    private func modeRow(lan: Bool, title: String, detail: String) -> some View {
        let chosen = network.map { $0.lan == lan } ?? false
        return Button {
            guard !chosen else { return }
            Task { await store.changeNetwork(mode: lan ? "lan" : "auto") }
        } label: {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: chosen ? "checkmark.circle.fill" : "circle")
                    .font(.title3).foregroundStyle(chosen ? Color.blue : Color.secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(title).font(.body.weight(.medium)).foregroundStyle(.primary)
                        if !lan {
                            Text("推荐").font(.caption2.weight(.semibold)).foregroundStyle(.blue)
                                .padding(.horizontal, 6).padding(.vertical, 2)
                                .background(.blue.opacity(0.12), in: Capsule())
                        }
                    }
                    Text(detail).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.leading)
                }
            }.padding(.vertical, 4).contentShape(Rectangle())
        }
        .buttonStyle(.plain).disabled(network == nil || store.applyingNetwork)
        .accessibilityAddTraits(chosen ? [.isSelected] : [])
        .accessibilityIdentifier(lan ? "network-mode-lan" : "network-mode-auto")
    }

    private func option(_ title: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
            Text(detail).font(.footnote).foregroundStyle(.secondary)
        }.padding(.vertical, 2)
    }
}

/// Where this phone reaches one paired computer, and a way to change it.
private struct PhoneHostAddressRow: View {
    var store: PhoneStore
    let host: PhoneHost
    let port: Int
    @State private var editing = false
    @State private var value = ""
    @State private var problem: String?
    @State private var busy = false
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Image(systemName: "desktopcomputer").foregroundStyle(.blue).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text(host.name).font(.body.weight(.medium))
                    Text(shown).font(.footnote.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                        .accessibilityIdentifier("network-address-\(host.name)")
                }
                Spacer(minLength: 4)
                if !editing {
                    Button("修改") { value = ""; problem = nil; editing = true; focused = true }
                        .buttonStyle(.bordered).buttonBorderShape(.capsule)
                        .accessibilityIdentifier("network-edit-\(host.name)")
                }
            }
            if editing {
                TextField("192.168.1.20", text: $value)
                    .font(.body.monospaced()).textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled().ticketCapitalizationDisabled().addressKeyboard()
                    .focused($focused).submitLabel(.done).onSubmit { save() }
                    .onChange(of: value) { _, _ in problem = nil }
                    .accessibilityLabel("\(host.name) 的地址").accessibilityIdentifier("network-address-field")
                if let problem { Text(problem).font(.footnote).foregroundStyle(.red) }
                HStack {
                    Button("保存") { save() }.buttonStyle(.borderedProminent).disabled(busy)
                        .accessibilityIdentifier("network-address-save")
                    Button("取消") { editing = false; problem = nil }.buttonStyle(.bordered)
                    if busy { ProgressView().padding(.leading, 4) }
                }.buttonBorderShape(.capsule)
            }
        }.padding(.vertical, 4)
    }

    private var shown: String {
        host.addresses.isEmpty ? "还没有地址" : (PhoneAddress.shareable(host.addresses).isEmpty ? host.addresses : PhoneAddress.shareable(host.addresses)).joined(separator: ", ")
    }

    private func save() {
        let text = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard PhoneAddress.validInput(text) else {
            problem = "请填 IP 地址，比如 192.168.1.20 或 192.168.1.20:\(port)。"
            return
        }
        busy = true
        Task {
            defer { busy = false }
            do { _ = try await store.setAddress(text, for: host); editing = false }
            catch { problem = error.localizedDescription }
        }
    }
}

private extension View {
    @ViewBuilder func addressKeyboard() -> some View {
        #if os(iOS)
        self.keyboardType(.numbersAndPunctuation)
        #else
        self
        #endif
    }
}
