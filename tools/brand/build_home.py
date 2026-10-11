"""Multi-device P2P + AI-native repository cover, using the existing vector identity."""
from pathlib import Path
from common import PAL, Text, mark, svg_doc

ROOT = Path(__file__).resolve().parents[2]


def build():
    for theme in ("dark", "light"):
        for lang in ("en", "zh"):
            p, t = PAL[theme], Text(lang)
            zh = lang == "zh"
            o = []
            add = o.append

            def box(x, y, w, h, stroke=None, dash=False):
                add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="20" '
                    f'fill="{p["panel"]}" stroke="{stroke or p["line"]}" stroke-width="2"'
                    + (' stroke-dasharray="8 7"' if dash else '') + '/>')

            add(f'<rect width="1760" height="1040" rx="36" fill="{p["page"]}"/>')
            add(mark(74, 80, 50, p["agent"], p["human"]))
            add(t(178, 95, "ai-remote", 46, p["ink"], 650, cls="s"))
            add(t(1680, 91, "开源 · PRE-ALPHA" if zh else "OPEN SOURCE · PRE-ALPHA", 23, p["muted"], anchor="end"))
            add(t(84, 153 if zh else 173, "多端远程控制 / P2P / MCP" if zh else "MULTI-DEVICE / P2P / MCP", 26, p["agenttext"], 600))
            add(t(80, 283, "AI Native." if zh else "AI-native.", 106, p["ink"], 700, cls="s"))
            add(t(80, 396 if zh else 383, "你的设备，远程直连。" if zh else "Remote, across devices.", 70 if zh else 76, p["ink"], 700))
            add(t(84, 465 if zh else 447, "手机、平板、Mac、PC，与你的 AI。" if zh else "Your phone. Your tablet. Your computer. Your agent.", 30, p["muted"]))
            add(t(84, 511 if zh else 493, "端到端加密 · P2P 优先 · 控制权始终在你" if zh else "Encrypted end to end. P2P first. You keep control.", 30, p["muted"]))

            # Solid lines for desktop implementations; dashed lines for mobile WIP.
            add(f'<g fill="none" stroke="{p["agent"]}" stroke-width="3">'
                '<path d="M1230 277 V330 H1378 M1530 277 V330 H1378"/>'
                '<path d="M1240 420 V360 H1378 M1530 420 V360 H1378" stroke-dasharray="8 7"/></g>')
            for x, label in ((1120, "Mac"), (1430, "Windows")):
                box(x, 175, 210, 102)
                add(f'<rect x="{x+24}" y="195" width="50" height="35" rx="4" fill="none" stroke="{p["muted"]}" stroke-width="2"/>'
                    f'<path d="M{x+49} 230 V240 M{x+33} 240 H{x+65}" stroke="{p["muted"]}" stroke-width="2"/>')
                add(t(x+92, 231, label, 25, p["ink"], 600, cls="s"))
            box(1230, 308, 300, 76, p["agent"])
            add(t(1380, 357, "P2P / QUIC", 30, p["agenttext"], 650, anchor="middle", cls="s"))
            for x, label, tablet in ((1120, "iPhone", False), (1430, "iPad", True)):
                box(x, 420, 210, 94, dash=True)
                w = 49 if tablet else 30
                add(f'<rect x="{x+28}" y="438" width="{w}" height="54" rx="7" fill="none" stroke="{p["muted"]}" stroke-width="2"/>')
                add(t(x+93, 455, label, 25, p["ink"], 600, cls="s"))
                add(t(x+93, 487, "WIP", 22, p["muted"], cls="s"))

            columns = [
                (80, p["agenttext"], "01 / AI NATIVE", "让 AI 直接操作电脑" if zh else "Your agent, on your devices", "原生 MCP · 结构化 UI · 授权工具" if zh else "Native MCP · UI trees · permitted tools"),
                (624, p["agenttext"], "02 / P2P", "跨设备，加密直连" if zh else "Paired devices. Direct paths.", "QUIC · NAT 穿透 · 加密中继兜底" if zh else "QUIC · NAT traversal · relay fallback"),
                (1168, p["humantext"], "03 / HUMAN CONTROL", "看得见，随时接管" if zh else "Watch. Take over. Hand back.", "实时画面 · 活动记录 · 权限控制" if zh else "Live view · activity · access levels"),
            ]
            for x, color, number, title, sub in columns:
                box(x, 561, 512, 195)
                add(t(x+28, 608, number, 24, color, 600, cls="s"))
                add(t(x+28, 670, title, 33 if zh else 29, p["ink"], 650))
                add(t(x+28, 720, sub, 24 if zh else 22, p["muted"]))

            add(t(84, 807, "一个项目，连接你的设备。" if zh else "One project. Your devices, connected.", 27, p["ink"], 600))
            platforms = [
                ("macOS", "Mac", "已实现" if zh else "Implemented", False),
                ("Windows", "PC", "已实现" if zh else "Implemented", False),
                ("iOS", "iPhone", "WIP", True),
                ("iPadOS", "iPad", "WIP", True),
                ("Android", "手机 / 平板" if zh else "Phone / tablet", "WIP", True),
                ("Linux", "电脑" if zh else "Computer", "WIP", True),
            ]
            for i, (platform, device, status, wip) in enumerate(platforms):
                x = 80 + i * 272
                box(x, 839, 240, 135, dash=wip)
                add(t(x+20, 878, platform, 28, p["ink"], 650, cls="s"))
                add(t(x+20, 916, device, 23, p["muted"]))
                add(t(x+20, 953, status, 22, p["muted"] if wip else p["agenttext"], 600))
            add(t(84, 1013, "从源码构建 · 移动端与 Linux 开发中 · 各平台验证范围见下方" if zh else "Build from source · Mobile + Linux in progress · Platform evidence below", 23, p["muted"]))
            name = f"home-{theme}" + (".zh-CN" if zh else "") + ".svg"
            title = "ai-remote：AI Native 多端 P2P 远程控制" if zh else "ai-remote: AI-native remote control across devices over P2P"
            desc = "Mac and Windows implementations; iOS, iPadOS, Android and Linux WIP. Native MCP, encrypted QUIC, owner takeover."
            (ROOT / "assets/brand" / name).write_text(svg_doc(1760, 1040, title, "".join(o), desc=desc), encoding="utf-8")


if __name__ == "__main__":
    build()
