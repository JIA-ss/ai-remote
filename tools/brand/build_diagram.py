"""Agent and human controllers share P2P transport to paired desktop hosts."""
from pathlib import Path
from common import PAL, Text, cursor, svg_doc

ROOT = Path(__file__).resolve().parents[2]


def build(theme, lang):
    p, t = PAL[theme], Text(lang)
    zh = lang == "zh"
    o = []
    add = o.append

    def box(x, y, w, h, stroke=None, dash=False):
        add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="18" fill="{p["panel"]}" '
            f'stroke="{stroke or p["line"]}" stroke-width="2"'
            + (' stroke-dasharray="8 7"' if dash else '') + '/>')

    add(t(40, 52, "AI 与你，连接同一组设备。" if zh else "Your agent and you. The same paired devices.", 36, p["ink"], 650))
    add(t(40, 97, "AI Native 工具 + 多端 P2P 远程控制" if zh else "AI-native tools + multi-device P2P remote control", 26, p["muted"]))

    # AI only talks MCP to the local bridge; phone/tablet apps use the shared controller.
    box(40, 140, 400, 192, p["agent"])
    add(f'<g transform="translate(88 178)">{cursor(40, p["agent"], mirror=True)}</g>')
    add(t(120, 210, "AI Native / MCP", 30, p["agenttext"], 650, cls="s"))
    add(t(72, 264, "Codex · Claude Code", 27, p["ink"], cls="s"))
    add(t(72, 302, "Cursor · VS Code · 自建 Agent" if zh else "Cursor · VS Code · your agent", 25, p["muted"]))
    box(40, 386, 400, 240, p["humanline"])
    add(t(72, 431, "你的控制端" if zh else "Your controller apps", 30, p["humantext"], 650))
    add(t(72, 480, "Mac · Windows", 27, p["ink"], cls="s"))
    add(t(72, 524, "iOS / iPhone · WIP", 26, p["muted"], cls="s"))
    add(t(72, 565, "iPadOS / iPad · WIP", 26, p["muted"], cls="s"))
    add(t(72, 606, "Android · WIP", 26, p["muted"], cls="s"))
    add(f'<path d="M440 235 H524 M440 506 H524" fill="none" stroke="{p["muted"]}" stroke-width="3"/>')
    add(t(482, 214, "MCP", 20, p["agenttext"], anchor="middle", cls="s"))

    box(524, 140, 350, 486)
    add(t(556, 206, "ai-remote", 36, p["ink"], 650, cls="s"))
    add(t(556, 257, "MCP 桥接" if zh else "Local MCP bridge", 27, p["agenttext"]))
    add(t(556, 299, "+ 共享控制核心" if zh else "+ shared controller", 27, p["humantext"]))
    add(f'<path d="M556 337 H842" stroke="{p["line"]}" stroke-width="2"/>')
    for y, s in [(383, "按设备身份配对" if zh else "Pair by device identity"),
                 (428, "权限 · 轮次 · 接管" if zh else "Levels · turns · takeover"),
                 (473, "截图 + 结构化 UI" if zh else "Screens + structured UI"),
                 (518, "鼠标键盘 + 授权工具" if zh else "Input + permitted tools")]:
        add(t(556, y, s, 26, p["muted"]))
    add(t(556, 591, "你与 AI 共享控制通道" if zh else "For you and your agent", 25, p["ink"], 600))

    # Direct P2P path is preferred, never promised on every network.
    add(f'<path d="M874 310 H1230 M874 320 H1230" stroke="{p["agent"]}" stroke-width="3"/>')
    add(t(1052, 251, "P2P / QUIC", 32, p["agenttext"], 650, anchor="middle", cls="s"))
    add(t(1052, 292, "端到端加密" if zh else "End-to-end encrypted", 24, p["muted"], anchor="middle"))
    add(t(1052, 362, "NAT 穿透 · 优先直连" if zh else "NAT traversal · direct first", 23, p["muted"], anchor="middle"))
    add(f'<path d="M900 320 V466 H952 M1152 466 H1204 V320" fill="none" stroke="{p["muted"]}" stroke-width="2" stroke-dasharray="8 7"/>')
    box(952, 430, 200, 74, dash=True)
    add(t(1052, 477, "加密中继" if zh else "Encrypted relay", 24, p["ink"], anchor="middle"))
    add(t(1052, 548, "无法直连时兜底" if zh else "Fallback when needed", 24, p["muted"], anchor="middle"))

    add(t(1258, 118, "已配对的被控电脑" if zh else "Paired host computers", 26, p["muted"]))
    for y, name, sub, dash in [
        (140, "macOS / Mac", "已实现 · 原生 AX" if zh else "Implemented · native AX", False),
        (312, "Windows / PC", "已实现 · 原生 UIA" if zh else "Implemented · native UIA", False),
        (484, "Linux / WIP", "桌面支持开发中" if zh else "Desktop support in progress", True),
    ]:
        box(1230, y, 490, 142, dash=dash)
        add(t(1262, y+54, name, 33, p["ink"], 650, cls="s"))
        add(t(1262, y+104, sub, 26, p["muted"]))

    box(40, 674, 1680, 86, p["humanline"])
    add(t(74, 728, "控制权在你：设置权限、查看活动、接管与交还；高风险操作等待批准。" if zh else "You stay in control: set access, watch activity, take over, hand back, and approve risky steps.", 28, p["ink"]))
    title = "多端 P2P 远程控制与 AI Native 架构" if zh else "Multi-device P2P remote control and AI-native architecture"
    desc = "AI agents use the local MCP bridge; human apps use the shared controller. Both reach paired Mac and Windows hosts via encrypted QUIC, direct when possible with relay fallback. iOS, iPadOS, Android and Linux are WIP."
    name = f"how-it-works-{theme}" + (".zh-CN" if zh else "") + ".svg"
    path = ROOT / "assets/brand" / name
    path.write_text(svg_doc(1760, 800, title, "".join(o), desc=desc), encoding="utf-8")
    return str(path)


def build_all():
    return [build(theme, lang) for lang in ("en", "zh") for theme in ("dark", "light")]


if __name__ == "__main__":
    for path in build_all():
        print(path)
