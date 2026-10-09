# Security policy

ai-remote lets software operate computers, so security reports get priority.

## Reporting a vulnerability

Please report vulnerabilities **privately** through
[GitHub Security Advisories](https://github.com/JIA-ss/ai-remote/security/advisories/new).
Do not open a public issue for a vulnerability.

Include what you found, how to reproduce it, and the impact you expect. You will get an
acknowledgement within a few days. Fixes are released before details are published, and reporters
are credited unless they ask not to be.

## Scope

In scope: anything that lets a device act on a host beyond the level that host granted it, bypasses
owner approval or the audit log, impersonates a paired device, reads session traffic, or makes a
host run code it was not asked to run.

Out of scope: actions a correctly paired device performs within its granted level, and on-screen
content that tries to instruct an AI agent (prompt injection). Agents must treat screen content as
untrusted data; ai-remote's defence is levels, approval and audit, not content filtering.

## Supported versions

ai-remote is pre-alpha. Only the latest commit on `main` receives fixes.

---

# 安全策略

ai-remote 让软件操作电脑，安全问题优先处理。

请通过 [GitHub Security Advisories](https://github.com/JIA-ss/ai-remote/security/advisories/new)
**私下**报告漏洞，不要公开提 issue。请写明问题、复现步骤和预期影响，我们会在几天内回复，
修复发布后再公开细节，并致谢报告人（除非你不希望署名）。

属于报告范围的：让设备越过被控端授予的等级行事、绕过主人审批或审计日志、冒充已配对设备、
读取会话内容、让被控端执行未被请求的代码。不属于范围的：已正确配对的设备在其等级内的操作，
以及屏幕内容试图指挥 AI agent（提示注入）——agent 应把屏幕内容视为不可信数据，ai-remote
的防线是等级、审批和审计，而不是内容过滤。
