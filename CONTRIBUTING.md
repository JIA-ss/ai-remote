# Contributing

ai-remote is public and pre-alpha. Bug reports, reproducible platform results and focused pull
requests are welcome. Discuss larger changes in an issue first; security reports belong in the
[private reporting channel](SECURITY.md).

## Development

Use the toolchain pinned in `rust-toolchain.toml`. The transport dependency snapshot is included
under `vendor/mcpmesh`; no private repository token is needed.

```sh
cd apps/desktop
npm ci
npm run build
cd ../..
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
node --test tools/mcp-install.test.mjs
```

See [Mac development](docs/macos.md), [iPhone development](apps/ios/README.md) and
[operations](docs/operations.md) for prerequisites and isolated host/controller profiles.
The standard GitHub Actions runners compile/test Mac, Windows and the iOS targets. CI build
results do not replace platform permission, physical input or real-device acceptance.

- Reuse maintained dependencies and explain any new one. Preserve third-party license notices.
- Keep mcpmesh types behind `ar-net`.
- Host calls must pass the gate, turn checks, approval and audit. Do not add a bypass.
- Include the actual platform and evidence scope in your PR. Do not claim device acceptance from
  a cross-compile or prerecorded screen. Keep private tickets, keys and screen captures out of Git.
- Describe what failed and how the user can recover in error messages.

## Licensing

Project code is [AGPL-3.0](LICENSE); vendored dependencies retain their own licenses. There is no
automated CLA flow configured. Any future distribution or contributor agreement change must be
documented explicitly; this repository does not promise an App Store release.

# 参与贡献

项目已公开，仍处于 pre-alpha。欢迎报告可复现问题、补充各平台验收和提交范围明确的 PR；较大
改动先在 issue 讨论。安全问题请按[安全策略](SECURITY.md)私下报告。

开发命令见上方。优先复用成熟依赖；mcpmesh 类型留在 `ar-net`；被控端调用必须经过权限、轮次、
审批与审计。写清实际测试平台和证据范围，不用交叉编译或录屏回放代替真机验收。票据、密钥与
私人截图不能提交。项目采用 AGPL-3.0，第三方源码保留原许可证；当前没有自动 CLA 签署流程，
未来分发或贡献协议变化需要另行明确记录。
