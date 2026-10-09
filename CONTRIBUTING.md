# Contributing

Thanks for your interest. ai-remote is pre-alpha and not open to outside contributions yet; this
page describes how it will work once it is.

## Before you start

- Open an issue to discuss anything larger than a small fix, so effort is not wasted.
- Security problems go through the [security policy](SECURITY.md), never public issues.

## Contributor license agreement

Contributions will require signing a contributor license agreement (CLA). ai-remote is licensed
under the AGPL-3.0; the CLA lets the project also publish the iOS app through the App Store, which
the AGPL alone does not allow. The CLA will be handled automatically on your first pull request.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets
```

The toolchain is pinned in `rust-toolchain.toml`. To try a host and a controller on one machine, use
two profiles:

```bash
ai-remote --home ./tmp/host host
ai-remote --home ./tmp/ctl pair <ticket printed by the host>
ai-remote --home ./tmp/ctl call system_info
```

Guidelines:

- Reuse maintained crates instead of writing your own; explain in the PR when you cannot.
- Keep `mcpmesh` types inside `ar-net`.
- Host tools must go through the gate; never add a path that skips levels, approval or audit.
- Error messages should tell the reader (often an AI agent) what went wrong and what to do next.

---

# 参与贡献

ai-remote 目前处于预览阶段，暂不接受外部贡献；以下是开放后的规则。较大的改动请先开 issue 讨论；
安全问题请按[安全策略](SECURITY.md)私下报告。贡献需要签署贡献者许可协议（CLA）：项目采用 AGPL-3.0，
CLA 让项目可以把 iOS App 发布到 App Store（单靠 AGPL 不行），首次提交 PR 时会自动引导签署。
开发约定：优先复用成熟的 crate；mcpmesh 的类型只出现在 `ar-net` 里；被控端工具必须经过闸门；
错误信息要让读者（往往是 AI agent）知道出了什么问题、下一步该怎么做。
