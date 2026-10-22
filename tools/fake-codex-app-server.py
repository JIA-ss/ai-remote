#!/usr/bin/env python3
"""A stand-in for `codex app-server` (JSON-RPC over stdio, the protocol of codex-cli 0.16x), for the
desktop chat e2e (tools/desktop-chat-e2e.py, tools/desktop-chat-codex-e2e.js). It answers the requests
the host's Codex adapter sends (initialize, thread/start and /resume, skills/list, turn/start, /steer,
/interrupt, thread/backgroundTerminals/list) and plays a scripted turn chosen by a word in the message:

  EXPLORE  reasoning, a remark, a search, a read and a listing, a failing command, an edit of two files
           (one new) that it really writes, a passing command, a web search, plan updates, the answer
  ASK      asks the user (item/tool/requestUserInput) and answers with the choice
  SLOW     runs `sleep 30` until Stop; a message sent meanwhile (turn/steer) is acknowledged
  STEERME  runs a command until a message is sent meanwhile (up to 20 s), takes it, and answers
  FAIL     the provider refuses: an `error` notification, then the turn fails
  MANY     eight files changed, a long command output and a long diff
  NOPHASE  messages without a phase, as older Codex versions send them
  COMPACT  the conversation is compacted, then a short answer
  anything else: a short answer

`--version` and `login status` answer like codex-cli, so the host's detection accepts it.
"""
import json
import os
import sys
import threading
import time

if "--version" in sys.argv:
    print("codex-cli 0.163.0")
    sys.exit(0)
if "login" in sys.argv:
    print("Logged in using ChatGPT")
    sys.exit(0)

lock = threading.Lock()
THREAD = "th-fake-0001"
state = {"turn": None, "n": 0, "cwd": os.getcwd(), "next_id": 1000}
interrupted = threading.Event()
steers = []
answers = {}
answered = threading.Event()


def send(v):
    with lock:
        sys.stdout.write(json.dumps(v) + "\n")
        sys.stdout.flush()


def note(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": {"threadId": THREAD, **params}})


def result(rid, value):
    send({"jsonrpc": "2.0", "id": rid, "result": value})


def item_started(item):
    note("item/started", {"turnId": state["turn"], "item": item})


def item_completed(item):
    note("item/completed", {"turnId": state["turn"], "item": item})


def pause(s):
    """Waits, unless the turn is stopped; says whether to go on."""
    return not interrupted.wait(s)


def message(mid, text, phase="final_answer", chunk=24, delay=0.03):
    started = {"type": "agentMessage", "id": mid, "text": "", "phase": phase, "memoryCitation": None}
    if phase is None:
        started["phase"] = None
    item_started(started)
    for i in range(0, len(text), chunk):
        note("item/agentMessage/delta", {"itemId": mid, "delta": text[i:i + chunk]})
        if not pause(delay):
            return False
    item_completed({**started, "text": text})
    return True


def reasoning(rid, summary):
    item_started({"type": "reasoning", "id": rid, "summary": [], "content": []})
    for i in range(0, len(summary), 20):
        note("item/reasoning/summaryTextDelta", {"itemId": rid, "delta": summary[i:i + 20], "summaryIndex": 0})
        if not pause(0.02):
            return False
    item_completed({"type": "reasoning", "id": rid, "summary": [summary], "content": []})
    return True


def command(cid, script, actions, output, exit_code=0, seconds=0.3, duration_ms=None):
    """A shell command as Codex runs it: inside `/bin/zsh -lc`, its output streamed, then its end."""
    shell = f"/bin/zsh -lc {json.dumps(script) if chr(39) in script else chr(39) + script + chr(39)}"
    base = {"type": "commandExecution", "id": cid, "command": shell, "cwd": state["cwd"], "processId": "p" + cid,
            "source": "agent", "status": "inProgress", "commandActions": actions, "aggregatedOutput": None,
            "exitCode": None, "durationMs": None}
    item_started(base)
    lines = output.splitlines(True)
    step = max(1, len(lines) // 4)
    for i in range(0, len(lines), step):
        note("item/commandExecution/outputDelta", {"itemId": cid, "delta": "".join(lines[i:i + step])})
        if not pause(seconds / 4):
            return False
    item_completed({**base, "status": "completed" if exit_code == 0 else "failed", "aggregatedOutput": output,
                    "exitCode": exit_code, "durationMs": duration_ms if duration_ms is not None else int(seconds * 1000)})
    return True


def plan(*steps):
    note("turn/plan/updated", {"turnId": state["turn"], "explanation": None,
                               "plan": [{"step": s, "status": st} for s, st in steps]})


def usage():
    note("thread/tokenUsage/updated", {"turnId": state["turn"], "tokenUsage": {
        "total": {"totalTokens": 52000, "inputTokens": 50000, "cachedInputTokens": 30000, "outputTokens": 2000, "reasoningOutputTokens": 400},
        "last": {"totalTokens": 41000, "inputTokens": 40000, "cachedInputTokens": 30000, "outputTokens": 1000, "reasoningOutputTokens": 300},
        "modelContextWindow": 258000}})


def end(status="completed", error=None):
    note("turn/completed", {"turn": {"id": state["turn"], "items": [], "status": status, "error": error}})
    state["turn"] = None


def unified(path, old, new):
    import difflib
    return "".join(difflib.unified_diff(old.splitlines(True), new.splitlines(True), "a/" + path, "b/" + path, n=3))


def file_change(fid, changes):
    """Writes the files, then reports the change as Codex does (unified diffs; a new file's diff is its content)."""
    items = []
    for rel, new in changes:
        path = os.path.join(state["cwd"], rel)
        old = open(path, encoding="utf-8").read() if os.path.exists(path) else None
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "w", encoding="utf-8").write(new)
        if old is None:
            items.append({"path": path, "kind": {"type": "add"}, "diff": new})
        else:
            body = unified(rel, old, new)
            body = body[body.index("@@"):] if "@@" in body else body
            items.append({"path": path, "kind": {"type": "update", "move_path": None}, "diff": body})
    base = {"type": "fileChange", "id": fid, "changes": items, "status": "inProgress"}
    item_started(base)
    if not pause(0.3):
        return False
    item_completed({**base, "status": "completed"})
    return True


OLD = "import calendar\nfrom datetime import date\n\n\ndef quarter_end(year, q):\n    return date(year, q * 3, 31)\n"
NEW = ("import calendar\nfrom datetime import date\n\n\ndef quarter_end(year, q):\n"
       "    last = calendar.monthrange(year, q * 3)[1]\n    return date(year, q * 3, last)\n")
TEST = ("import pytest\nfrom datetime import date\n\nfrom export import quarter_end\n\n\n"
        "@pytest.mark.parametrize(\"q, last\", [(1, 31), (2, 30), (3, 30), (4, 31)])\n"
        "def test_quarter_end(q, last):\n    assert quarter_end(2026, q) == date(2026, q * 3, last)\n")
ANSWER = """Fixed: `quarter_end` always used **day 31**, which June and September do not have.

| Quarter | Before | Now |
|:--|:--|:--|
| Q2 | `ValueError` | ok |
| Q3 | `ValueError` | ok |

```sh
python3 -m pytest -q
```
All 5 tests pass."""


def explore(n):
    k = lambda x: f"{x}{n}"
    cwd = state["cwd"]
    if not reasoning(k("r"), "**Locating the date math**\n\nThe error says the day is out of range, so the quarter end is probably built with a fixed day."):
        return end("interrupted")
    if not message(k("m"), "I'll find where the quarter end is computed, then reproduce the crash.", "commentary"):
        return end("interrupted")
    plan(("Reproduce the Q3 crash", "inProgress"), ("Use each month's real last day", "pending"), ("Add a test per quarter", "pending"))
    steps = [
        (k("c"), "rg -n 'quarter_end|date\\(' -g '*.py'", [{"type": "search", "command": "rg -n 'quarter_end|date\\(' -g '*.py'", "query": "quarter_end|date\\(", "path": None}],
         "export.py:5:def quarter_end(year, q):\nexport.py:6:    return date(year, q * 3, 31)\n", 0),
        (k("d"), "sed -n '1,80p' export.py", [{"type": "read", "command": "sed -n '1,80p' export.py", "name": "export.py", "path": os.path.join(cwd, "export.py")}],
         open(os.path.join(cwd, "export.py"), encoding="utf-8").read() if os.path.exists(os.path.join(cwd, "export.py")) else OLD, 0),
        (k("e"), "ls", [{"type": "listFiles", "command": "ls", "path": None}], "export.py\n", 0),
    ]
    for cid, script, actions, out, code in steps:
        if not command(cid, script, actions, out, code, 0.25):
            return end("interrupted")
    if not command(k("f"), "python3 export.py --quarter Q3", [{"type": "unknown", "command": "python3 export.py --quarter Q3"}],
                   "Traceback (most recent call last):\n  File \"export.py\", line 6, in quarter_end\n    return date(year, q * 3, 31)\nValueError: day is out of range for month\n",
                   1, 1.2):
        return end("interrupted")
    plan(("Reproduce the Q3 crash", "completed"), ("Use each month's real last day", "inProgress"), ("Add a test per quarter", "pending"))
    if not message(k("m2"), "Reproduced: the end date is always day 31. I'll take the real last day from `calendar.monthrange` and cover every quarter in a test.", "commentary"):
        return end("interrupted")
    path = os.path.join(cwd, "export.py")
    old = open(path, encoding="utf-8").read() if os.path.exists(path) else OLD
    bad = "    return date(year, q * 3, 31)\n"
    new = old.replace(bad, "    last = calendar.monthrange(year, q * 3)[1]\n    return date(year, q * 3, last)\n") if bad in old else NEW
    if not file_change(k("p"), [("export.py", new), (os.path.join("tests", "test_quarters.py"), TEST)]):
        return end("interrupted")
    if not command(k("g"), "python3 -m pytest -q", [{"type": "unknown", "command": "python3 -m pytest -q"}],
                   ".....                                                                [100%]\n5 passed in 0.04s\n", 0, 0.6, 2300):
        return end("interrupted")
    item_started({"type": "webSearch", "id": k("w"), "query": "python calendar monthrange", "action": None})
    if not pause(0.4):
        return end("interrupted")
    item_completed({"type": "webSearch", "id": k("w"), "query": "python calendar monthrange", "action": {"type": "search", "query": "python calendar monthrange", "queries": None}})
    plan(("Reproduce the Q3 crash", "completed"), ("Use each month's real last day", "completed"), ("Add a test per quarter", "completed"))
    if not message(k("a"), ANSWER, "final_answer", 16, 0.04):
        return end("interrupted")
    usage()
    end()


def ask(n):
    if not message(f"m{n}", "Before I change anything I need one decision.", "commentary"):
        return end("interrupted")
    answered.clear()
    answers.clear()
    rid = state["next_id"] = state["next_id"] + 1
    send({"jsonrpc": "2.0", "id": rid, "method": "item/tool/requestUserInput", "params": {
        "threadId": THREAD, "turnId": state["turn"], "itemId": f"q{n}",
        "questions": [{"id": "scope", "header": "Scope", "question": "Should I fix report/monthly.py too?", "isOther": True, "isSecret": False,
                       "options": [{"label": "Fix it too", "description": "Same bug, same fix"}, {"label": "Only export.py", "description": "Keep the change small"}]}]}})
    state["asking"] = rid
    while not answered.wait(0.2):
        if interrupted.is_set():
            return end("interrupted")
    choice = (answers.get("scope") or {}).get("answers", ["?"])[0]
    if not message(f"a{n}", f"Going with **{choice}**. Nothing else needed changing.", "final_answer"):
        return end("interrupted")
    end()


def slow(n):
    if not message(f"m{n}", "Counting slowly; send a message to steer me.", "commentary"):
        return end("interrupted")
    cid = f"s{n}"
    base = {"type": "commandExecution", "id": cid, "command": "/bin/zsh -lc 'sleep 30'", "cwd": state["cwd"], "processId": "p" + cid,
            "source": "agent", "status": "inProgress", "commandActions": [{"type": "unknown", "command": "sleep 30"}],
            "aggregatedOutput": None, "exitCode": None, "durationMs": None}
    item_started(base)
    seen = 0
    for _ in range(300):
        if interrupted.wait(0.1):
            # The command keeps running as a background terminal; the turn ends interrupted.
            return end("interrupted")
        while seen < len(steers):
            message(f"n{n}{seen}", f"Noted: {steers[seen]}", "commentary")
            seen += 1
    item_completed({**base, "status": "completed", "aggregatedOutput": "", "exitCode": 0, "durationMs": 30000})
    message(f"a{n}", "Done counting.", "final_answer")
    end()


def steerme(n):
    cid = f"w{n}"
    base = {"type": "commandExecution", "id": cid, "command": "/bin/zsh -lc 'npm run build'", "cwd": state["cwd"], "processId": "p" + cid,
            "source": "agent", "status": "inProgress", "commandActions": [{"type": "unknown", "command": "npm run build"}],
            "aggregatedOutput": None, "exitCode": None, "durationMs": None}
    item_started(base)
    for _ in range(200):
        if interrupted.wait(0.1):
            return end("interrupted")
        if steers:
            break
    item_completed({**base, "status": "completed", "aggregatedOutput": "built in 1.2s\n", "exitCode": 0, "durationMs": 1200})
    taken = steers[0] if steers else "nothing"
    if not message(f"a{n}", f"Built, and taken into account: {taken}.", "final_answer"):
        return end("interrupted")
    end()


def fail(n):
    if not reasoning(f"r{n}", "**Trying the request**"):
        return end("interrupted")
    note("error", {"turnId": state["turn"], "willRetry": False,
                   "error": {"message": json.dumps({"type": "error", "status": 503, "error": {"message": "The model is overloaded. Try again later."}})}})
    end("failed", None)


def many(n):
    if not message(f"m{n}", "Renaming the helpers in every report module.", "commentary"):
        return end("interrupted")
    big = "".join(f"line {i}: {'ok' if i % 7 else 'warn'}\n" for i in range(1, 301))
    if not command(f"b{n}", "python3 -c 'print(300 lines)'", [{"type": "unknown", "command": "python3 -c 'print(300 lines)'"}], big, 0, 0.4, 1500):
        return end("interrupted")
    files = []
    for i in range(1, 8):
        rel = os.path.join("report", f"mod{i}.py")
        files.append((rel, "".join(f"def helper_{j}():\n    return {j}\n" for j in range(1, 6 + i))))
    long = "".join(f"value_{j} = {j}\n" for j in range(1, 701))
    files.append(("constants.py", long))
    if not file_change(f"p{n}", files):
        return end("interrupted")
    if not message(f"a{n}", "Renamed the helpers in seven modules and generated `constants.py`.", "final_answer"):
        return end("interrupted")
    end()


def nophase(n):
    if not message(f"m{n}", "Looking at it.", None):
        return end("interrupted")
    if not command(f"c{n}", "cat README.md", [{"type": "read", "command": "cat README.md", "name": "README.md", "path": os.path.join(state["cwd"], "README.md")}], "# Report tool\n", 0, 0.2):
        return end("interrupted")
    if not message(f"a{n}", "The README describes the report tool.", None):
        return end("interrupted")
    end()


def compact(n):
    note("thread/compacted", {"turnId": state["turn"]})
    item_started({"type": "contextCompaction", "id": f"x{n}"})
    item_completed({"type": "contextCompaction", "id": f"x{n}"})
    if not message(f"a{n}", "Compacted; still here.", "final_answer"):
        return end("interrupted")
    end()


def short(n):
    if not reasoning(f"r{n}", "**Answering directly**"):
        return end("interrupted")
    if not message(f"a{n}", "Yes: `calendar.monthrange(2024, 2)` is `(3, 29)`, so leap years are handled.", "final_answer"):
        return end("interrupted")
    end()


SCRIPTS = [("EXPLORE", explore), ("ASK", ask), ("STEERME", steerme), ("SLOW", slow), ("FAIL", fail), ("MANY", many), ("NOPHASE", nophase), ("COMPACT", compact)]


def run_turn(text):
    state["n"] += 1
    n = state["n"]
    interrupted.clear()
    steers.clear()
    for word, fn in SCRIPTS:
        if word in text:
            return fn(n)
    return short(n)


def text_of(items):
    return " ".join(i.get("text", "") for i in items if i.get("type") == "text")


for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
    if method is None and rid is not None:
        # The answer to a question we asked.
        if rid == state.get("asking"):
            answers.update((msg.get("result") or {}).get("answers") or {})
            answered.set()
        continue
    if method == "initialize":
        result(rid, {"userAgent": "fake-codex/0.163.0", "platformFamily": "unix", "platformOs": sys.platform})
    elif method in ("thread/start", "thread/resume"):
        state["cwd"] = params.get("cwd") or state["cwd"]
        result(rid, {"thread": {"id": THREAD, "preview": "", "cwd": state["cwd"]}, "model": "gpt-fake", "cwd": state["cwd"]})
    elif method == "skills/list":
        result(rid, {"data": [{"cwd": state["cwd"], "skills": [{"name": "tidy", "path": "/skills/tidy/SKILL.md", "description": "Tidy imports and formatting", "enabled": True}], "errors": []}]})
    elif method == "turn/start":
        state["turn"] = f"tu-{state['n'] + 1}"
        result(rid, {"turn": {"id": state["turn"], "items": [], "status": "inProgress", "error": None}})
        threading.Thread(target=run_turn, args=(text_of(params.get("input") or []),), daemon=True).start()
    elif method == "turn/steer":
        if state["turn"] is None:
            send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32600, "message": "no active turn to steer"}})
        else:
            steers.append(text_of(params.get("input") or []))
            result(rid, {"turnId": state["turn"]})
    elif method == "turn/interrupt":
        interrupted.set()
        result(rid, {})
    elif method == "thread/backgroundTerminals/list":
        result(rid, {"data": [], "nextCursor": None})
    elif rid is not None:
        result(rid, {})
