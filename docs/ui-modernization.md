# 设备总览与液态玻璃界面

本轮参考 [网易 UU 远程](https://uuyc.163.com/) 的设备入口与连接反馈，以及 Apple 官方
Liquid Glass 指南和 Landmarks 示例源码。具体来源、实现证据和边界见
[调研记录](ui-reference-review.md)。UU 产品源码未获得，本轮没有借用其私有实现。

## 已实现

- 桌面默认进入设备总览。在线数量、待批准数量、权限、当前智能体和直连／中继路径来自现有状态；
  搜索同时过滤侧栏和卡片，离线设备不能直接观看。
- 浮动侧栏、标题栏与会话控制栏使用玻璃材质；设备内容和审批正文保持清晰。
  浅色／深色／跟随系统与减少透明度可保存，高对比、减少动态和不支持背景模糊时有回退。
- 原生导航按钮支持键盘操作。连接或重连后重新读取路径，保留未知状态；只显示仍连接的智能体。
  会话操作失败显示原因，离开控制界面时撤销尚未发送的缓冲输入。
- iPhone 使用原生设备卡片、适应大字体的布局与安全区域控制栏。iOS/macOS 26+
  使用 `glassEffect`，旧系统保留标准材质；暂停不依赖视频首帧，继续仍要求控制权限。
- 浏览器样例明确标为“界面预览”，样例工具目录与当前内置结构化 UI 工具一致。

## 验收与复现

```sh
cd apps/desktop
npm ci
npm run build
npm run dev
```

另一个终端从仓库根目录执行：

```sh
node --test tools/desktop-state.test.mjs tools/mcp-install.test.mjs
python3 tools/desktop-ui-smoke.py
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
tools/macos.sh build --debug --bundles app
tools/ios.sh mac-core
python3 tools/ui-smoke.py --out target/ui-modernization/native-mac
```

（2026-10-26 起 `tools/desktop-ui-smoke.py` 已改写成玻璃巡检：直接走 Camofox REST、自己起 Vite，结果默认写到
`target/glass-audit/`，不再使用 `general` session；下面是改写前那一轮的记录。）

本轮本机结果：桌面生产构建与原生 macOS `.app` 打包通过，206 个 Rust 测试通过，Clippy 通过；
前端 8 个测试通过（本机不具备 PowerShell 的 1 项跳过）。Camofox 在既有 `general` session、
headless 模式完成搜索、键盘导航、设备详情、接管／交还、外观保存与页面入口检查，并检查
960×600、1280×800、1728×1031 布局。报告和截图在 `target/ui-modernization/browser/`。
SwiftUI 源码通过 macOS SDK 编译及类型检查。

本次浏览器操作使用明确标识的样例后端，不能证明真实视频、鼠标键盘或端到端远控可用。
原生 macOS AX 验收预检报告 `the macOS screen is locked`，证据保存到
`target/ui-modernization/native-mac/report.json`；本机缺少完整 Xcode/iOS SDK，未完成
iPhone 模拟器或真机 UI 验收。没有发布新的签名或公证安装包。
