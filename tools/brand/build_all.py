"""Regenerate every asset in assets/brand/ and check it.

    python tools/brand/build_all.py

Checks: well-formed XML (SVG shown through <img> must be strict XML), a <title>, no external references,
no text below 20 units in the 1760-wide assets (10 px at GitHub's 880 px column), size under 60 KB,
and a reduced-motion rule in every animated file.
"""
import glob, os, re, sys
import xml.etree.ElementTree as ET
import build_brand, build_demo, build_diagram, build_home

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def main():
    build_brand.build()
    build_demo.build_all()
    build_diagram.build_all()
    build_home.build()
    bad = 0
    for f in sorted(glob.glob(os.path.join(ROOT, "assets", "brand", "*.svg"))):
        s = open(f, encoding="utf-8").read()
        name = os.path.basename(f)
        problems = []
        try:
            ET.fromstring(s.encode("utf-8"))
        except ET.ParseError as e:
            problems.append(f"XML: {e}")
        if "<title>" not in s:
            problems.append("no <title>")
        if re.search(r"https?://(?!www\.w3\.org/2000/svg)", s):
            problems.append("external reference")
        if "@keyframes" in s and "prefers-reduced-motion" not in s:
            problems.append("animated without reduced-motion rule")
        if 'viewBox="0 0 1760' in s:
            small = [float(x) for x in re.findall(r'font-size="([\d.]+)"', s) if float(x) < 20]
            if small:
                problems.append(f"text below 20 units: {sorted(set(small))}")
        kb = len(s.encode()) / 1024
        if kb > 60:
            problems.append(f"{kb:.1f} KB")
        bad += bool(problems)
        print(("BAD " if problems else "ok  ") + f"{name:34s} {kb:5.1f} KB", "; ".join(problems))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
