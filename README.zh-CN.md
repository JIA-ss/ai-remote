# ai-remote

<p align="right"><a href="README.md">English</a> · <b>中文</b></p>
<p align="center"><picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/brand/home-dark.zh-CN.svg">
<source media="(prefers-color-scheme: light)" srcset="assets/brand/home-light.zh-CN.svg">
<img src="assets/brand/home-light.zh-CN.svg" width="880" alt="ai-remote：让 AI 动手，控制权在你。Windows 与 macOS 的 MCP 远程控制；iPhone 控制端源码开发中。">
</picture></p>
<p align="center"><a href="#从源码开始">快速上手</a> · <a href="#接入你的智能体">MCP 接入</a> · <a href="#你的电脑你的规则">安全机制</a> · <a href="#目前能做什么">平台状态</a> · <a href="docs/architecture.zh-CN.md">架构</a></p>

**让 MCP 智能体远程操作你的电脑，也让你看得见、随时能接管。**
把 Codex、Claude Code、Cursor、VS Code 或其他 stdio MCP 客户端接入 ai-remote，即可操作已配对的 Windows 与 Mac：查看屏幕、发送鼠标键盘输入、列出窗口、运行授权的工具。连接经过加密；你可以暂停智能体、接管，再交还控制权。

> [!NOTE]
> **Pre-alpha，需要从源码构建。** Apple Silicon Mac 被控端和桌面应用已通过本机验收。Windows 已有实现。本仓库包含原生 iPhone 控制端源码，但 **iOS SDK 构建、模拟器安装和真机验收尚未验证**。目前没有签名发行包或 App Store 应用。[验收记录](docs/iphone-validation.md)

## 目前能做什么

| 端 | 状态 | 入口 |
| :-- | :-- | :-- |
| Windows 被控端 / CLI | 已实现；本轮 Mac 适配未重新做 Windows 运行验收 | [使用说明](docs/operations.md) |
| macOS 被控端 | 本机已验证配对、截图、输入和 H.264 屏幕流 | [Mac 配置与限制](docs/macos.md) |
| 桌面控制端 | 电脑列表、实时画面、活动、配对、接管与多 MCP 客户端配置 | [桌面应用](apps/desktop/README.md) |
| MCP 桥接 | 已验证真实 stdio 初始化、工具列表与已连接智能体记录 | [客户端配置](docs/mcp-clients.md) |
| iPhone 控制端 | 实验性 SwiftUI/UniFFI 源码；Mac 核心测试和原生解码回放通过 | [iPhone 构建与验收](apps/ios/README.md) |

本轮 Mac 验收使用 Apple Silicon 与 macOS 27。Intel Mac、较早系统仍需独立运行验收。开发版屏幕流已经工作，但当前帧率较低，不作为性能或延迟基准。

## 为什么使用 ai-remote

- **沿用现有智能体。** 一个 MCP 桥接访问你已配对的电脑，无需项目自带的托管 AI 模型。
- **按设备身份配对。** 一次性票据建立信任；QUIC 优先直连，需要时使用加密中继。
- **只授予需要的权限。** 每台设备分别设置 observe、control 或 admin。执行前检查权限、轮次、策略与审批。
- **看见过程，随时介入。** 桌面应用提供实时画面和活动记录；接管期间，智能体的控制调用会被阻止，交还后恢复。
- **通过已有 MCP 服务扩展。** 指定本地可执行文件与工具白名单挂载服务。无障碍元素树等能力由相应集成提供；核心尚不包含通用的原生元素树。

<p align="center"><picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/brand/how-it-works-dark.zh-CN.svg">
<source media="(prefers-color-scheme: light)" srcset="assets/brand/how-it-works-light.zh-CN.svg">
<img src="assets/brand/how-it-works-light.zh-CN.svg" width="880" alt="MCP 客户端连接 ai-remote mcp，再经加密 QUIC 访问已配对的电脑；被控端检查权限、审批并记录审计。">
</picture></p>

## 从源码开始

需要锁定的 Rust 1.95.0 工具链。桌面应用还需要 Node/npm 与对应系统的原生开发工具。

```sh
git clone https://github.com/JIA-ss/ai-remote.git
cd ai-remote
cargo build -p ai-remote
```

在 **macOS** 构建带 CLI 的开发应用：

```sh
tools/macos.sh build --debug --bundles app
open target/debug/bundle/macos/ai-remote.app
```

在**被控电脑**上运行 CLI（位于 `target/debug/`，或先加入 PATH）：

```sh
ai-remote host
# 在这台电脑的另一终端生成票据：
ai-remote pair-code --level control
```

在**控制电脑**兑换一次性票据：

```sh
ai-remote pair <ticket> --name office-pc
ai-remote call system_info --host office-pc
```

核对两端相同的三个单词安全码。Mac 被控端需要给实际运行的可执行文件授予屏幕录制与辅助功能权限，重启后运行 `ai-remote doctor`。锁屏或权限不足时，先恢复桌面和权限再操作。[详细说明](docs/operations.md)

## 接入你的智能体

桌面应用中打开 **设置 → 智能体**，选择客户端后复制安装指引。应用会提供真实可执行文件路径和当前配对配置目录，也支持 Mac 应用包内的 CLI。

CLI 已加入 PATH 时，可使用：

```sh
# Claude Code，适用于所有项目
claude mcp add --transport stdio --scope user ai-remote -- ai-remote mcp
# Codex
codex mcp add ai-remote -- ai-remote mcp
```

Cursor 使用 `~/.cursor/mcp.json` 中的 `mcpServers` JSON，VS Code 使用 MCP 配置中的 `servers` JSON。Codex 还可在 `~/.codex/config.toml` 中添加 `[mcp_servers.ai-remote]`。路径、转义和完整示例见 [MCP 客户端配置](docs/mcp-clients.md)。

## 你的电脑，你的规则

| 等级 | 允许的工具 |
| :-- | :-- |
| `observe` | 屏幕、窗口列表、系统信息与已允许的只读集成 |
| `control` | observe 加鼠标、键盘与剪贴板 |
| `admin` | control 加 shell 与后台任务；高风险调用先等待批准 |

高风险请求等待主人确认，未响应则拒绝。每次决策与结果写入被控端审计日志；撤销或取消配对即可移除设备访问权。

**远程允许签名当前需要 Windows Hello。** Mac 与 iPhone 控制端可以拒绝；Touch ID / Face ID 允许签名尚未实现。被控电脑桌面可用时，也可通过本机提示批准。[审批设计](docs/approvals.md)

## 常见问题

**iPhone 能控制电脑吗？** `apps/ios` 就是为此开发的原生 SwiftUI 控制端，复用 Rust 核心，包含配对、画面、触控与文字、接管和活动功能。目前仍需要 iOS SDK 构建和设备验收，Mac Swift 编译不能代替 iPhone 安装。

**智能体能通过它控制 iPhone 吗？** 不能。iOS 源码是控制端，不是 iPhone 被控端。

**需要端口转发或 ai-remote 云账号吗？** 正常配对不要求手动端口转发或项目云账号。网络限制仍可能使设备不可达，工具会返回错误，不保证所有网络都能连接。

**自带原生无障碍元素树或文件管理吗？** 核心提供截图、输入和窗口；额外能力由白名单挂载的 MCP 服务提供。Mac 原生元素树与文件传输仍在规划中。

**安装包在哪里？** 暂未发布签名版本，需要从源码构建。Windows、Intel Mac、iPhone 签名与 Mac 公证都有各自的验收要求。

## 开发与证据

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
node --test tools/mcp-install.test.mjs
```

[Mac 验收](docs/macos.md) · [iPhone 验收记录](docs/iphone-validation.md) · [工具参考](docs/tools.md) · [安全策略](SECURITY.md) · [参与贡献](CONTRIBUTING.md)

## 路线图

| 阶段 | 下一步 |
| :-- | :-- |
| M0–M2 | 持续验证 Windows/Mac，提升捕获性能，完成文件与无障碍能力，签名打包 |
| M3 | iOS SDK 编译，模拟器与真机验收；Face ID 签名、二维码配对与通知 |
| M4 | 可选自托管账号/中继服务与设备共享 |

## 许可证

[AGPL-3.0](LICENSE)。官方仓库：[JIA-ss/ai-remote](https://github.com/JIA-ss/ai-remote)。
