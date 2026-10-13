# Connect an MCP client / 连接 MCP 客户端

In the desktop app, open **Settings → Agents → MCP client**. Choose Claude Code, Codex, Cursor,
VS Code / GitHub Copilot or another stdio client, then copy the command or configuration shown.
The app uses its actual CLI executable and passes `--home <this profile> mcp`, so the client shares
the app's identity and paired computers even when the CLI is bundled inside a Mac app or its
path contains spaces. Mac commands are quoted for a POSIX shell; Windows commands for PowerShell.

| Client | Setup shown in the app | Official reference |
| :-- | :-- | :-- |
| Claude Code | `claude mcp add --transport stdio --scope user ai-remote -- …` | [Claude Code MCP](https://code.claude.com/docs/en/mcp) |
| Codex | `codex mcp add ai-remote -- …`, or a `[mcp_servers.ai-remote]` TOML entry in `~/.codex/config.toml` | [Codex MCP](https://developers.openai.com/codex/mcp/) |
| Cursor | `mcpServers` JSON in `~/.cursor/mcp.json`, or `.cursor/mcp.json` for one project | [Cursor MCP](https://cursor.com/help/customization/mcp) |
| VS Code / GitHub Copilot | `servers` JSON with `type: "stdio"`; open **MCP: Open User Configuration**, or use `.vscode/mcp.json` | [VS Code MCP](https://code.visualstudio.com/docs/agent-customization/mcp-servers) |
| Other | `mcpServers` JSON where supported, or enter the shown `command` and `args` in the client's local stdio settings | Follow that client's configuration format |

Once connected, the agent should also get the working method in
[skills/ai-remote/SKILL.md](../skills/ai-remote/SKILL.md): session start, looking and acting,
commands, files, and what levels, approvals and refusals mean. Claude Code loads it from
`~/.claude/skills/ai-remote/`; other agents take its text as rules.

Add or update the ai-remote entry while retaining other server settings. Reconnect/reload the
client's server after configuring it; its initialized MCP session then appears in **Connected
agents**. Tools become available after a host is paired. The app displays setup instructions;
it does not silently edit another client's configuration files.

在桌面应用打开 **设置 → 智能体 → MCP 客户端**，选择客户端后复制对应命令或配置。应用提供
实际的 CLI 路径，并通过 `--home` 绑定当前配对目录；无需先把打包应用中的 CLI 添加到 PATH。
Claude Code、Codex、Cursor 和 VS Code 的配置格式与保存位置分别显示，命令会处理 Mac 路径
中的空格，也有 Windows PowerShell 引用方式。

合并 JSON/TOML 时保留已有的其他服务器。完成配置后，在客户端重新连接或重新加载 MCP；
初始化成功的会话会显示在上方“已连接的智能体”中，配对电脑后可使用其工具。这里提供安装
说明与复制功能，不会直接修改其他客户端的配置文件。

Validation: `node --test tools/mcp-install.test.mjs` checks literal shell arguments and JSON/TOML
schemas. After a macOS debug bundle, `python3 tools/mcp-smoke.py` checks the actual stdio initialize,
tools/list and connected-client record with an isolated profile. This does not claim a runtime
acceptance run inside every third-party client.
