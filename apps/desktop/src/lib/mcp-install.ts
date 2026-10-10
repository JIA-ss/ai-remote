// Client-specific setup, from one actual executable/profile supplied by the desktop backend.
export type McpClient = "claude" | "codex" | "cursor" | "vscode" | "other";
export interface McpLaunch { command: string; args: string[] }

export const MCP_CLIENTS: Array<[McpClient, string]> = [
  ["claude", "Claude Code"], ["codex", "Codex"], ["cursor", "Cursor"],
  ["vscode", "VS Code / GitHub Copilot"], ["other", ""],
];

/** POSIX shell or PowerShell, both with literal arguments (including spaces and apostrophes). */
export function shellArgument(value: string, windows: boolean): string {
  return windows ? `'${value.replaceAll("'", "''")}'` : `'${value.replaceAll("'", "'\"'\"'")}'`;
}

export function mcpInstall(client: McpClient, launch: McpLaunch, windows: boolean) {
  const argv = [launch.command, ...launch.args].map((a) => shellArgument(a, windows)).join(" ");
  switch (client) {
    case "claude":
      return { command: `claude mcp add --transport stdio --scope user ai-remote -- ${argv}` };
    case "codex":
      return {
        command: `codex mcp add ai-remote -- ${argv}`,
        config: `[mcp_servers.ai-remote]\ncommand = ${JSON.stringify(launch.command)}\nargs = ${JSON.stringify(launch.args)}`,
      };
    case "vscode":
      return { config: JSON.stringify({ servers: { "ai-remote": { type: "stdio", ...launch } } }, null, 2) };
    default:
      return { config: JSON.stringify({ mcpServers: { "ai-remote": launch } }, null, 2) };
  }
}
