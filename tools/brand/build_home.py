"""Repository cover: two operators, one computer. Code-native artwork; no private screen content."""
from pathlib import Path
from common import PAL, Text, mark, svg_doc

ROOT = Path(__file__).resolve().parents[2]

def build():
    for theme in ("dark", "light"):
        for lang in ("en", "zh"):
            p, t = PAL[theme], Text(lang)
            zh = lang == "zh"
            body = f'<rect width="1760" height="900" rx="36" fill="{p["page"]}"/>'
            body += mark(74, 80, 66, p["agent"], p["human"])
            body += t(178, 116, "ai-remote", 46, p["ink"], 650, cls="s")
            body += t(1640, 111, "开源 · PRE-ALPHA" if zh else "OPEN SOURCE · PRE-ALPHA", 23, p["muted"], anchor="end", mono=True)
            body += t(80, 274, "让 AI 动手。" if zh else "Your agent works.", 102, p["ink"], 700)
            body += t(80, 400, "控制权，在你。" if zh else "You keep the controls.", 102, p["ink"], 700)
            body += t(84, 465, "MCP 远程操作 · 加密配对 · 随时接管" if zh else "Remote MCP. Encrypted pairing. Take over any time.", 34, p["muted"])
            body += mark(330, 1302, 169, p["agent"], p["human"])
            body += f'<rect x="80" y="540" width="1600" height="270" rx="24" fill="{p["panel"]}" stroke="{p["line"]}" stroke-width="2"/>'
            body += f'<path d="M616 572 V778 M1160 572 V778" stroke="{p["line"]}" stroke-width="2"/>'
            columns = [
                (116, p["agent"], "01  CONNECT", "连接你的智能体" if zh else "Connect your agent", "Codex · Claude · Cursor · VS Code"),
                (652, p["muted"], "02  PAIR", "一次配对，按权限操作" if zh else "Pair once. Set the level.", "observe → control → admin"),
                (1196, p["human"], "03  TAKE OVER", "你随时接管与交还" if zh else "Your turn. Any time.", "暂停 · 接管 · 交还" if zh else "Pause · take over · hand back"),
            ]
            for x, color, number, title, sub in columns:
                body += t(x, 610, number, 25, color, mono=True)
                body += t(x, 686, title, 35 if zh else 30, p["ink"], 600)
                body += t(x, 740, sub, 22 if not zh else 25, p["muted"], mono=not zh)
            body += t(82, 865, "Windows + macOS · iPhone 控制端源码开发中" if zh else "Windows + macOS · iPhone controller source in development", 26, p["muted"])
            name = f"home-{theme}" + (".zh-CN" if zh else "") + ".svg"
            (ROOT / "assets/brand" / name).write_text(svg_doc(1760,900,"ai-remote: MCP remote control with owner takeover",body,desc="Blue represents the agent; amber represents the owner. Phone controller remains experimental."))

if __name__ == "__main__": build()
