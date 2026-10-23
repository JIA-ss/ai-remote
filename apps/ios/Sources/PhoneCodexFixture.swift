#if DEBUG
import Foundation

// Simulator UI acceptance only (`-ui-chat-fixture -ui-chat-codex`): a Codex session shaped like the
// desktop's demo (apps/desktop/src/lib/demo.ts `codexDemoLog` and `demoCodexReply`), as a Codex host
// sends it — remarks and answers say their phase, commands carry the command without its shell and
// Codex's reading of it, edits name their files with unified diffs, finished commands their exit code
// and run time.

extension PhoneChatStore {
    static let codexFixtureCwd = "D:\\work\\report-tool"

    private static func cmd(_ command: String, _ actions: [JSONValue]) -> JSONValue {
        .object(["command": .string(command), "cwd": .string(codexFixtureCwd), "actions": .array(actions)])
    }
    private static func read(_ rel: String) -> JSONValue {
        .object(["type": .string("read"), "command": .string("sed -n '1,120p' \(rel)"), "name": .string(phoneBaseName(rel)),
                 "path": .string(codexFixtureCwd + "\\" + rel.replacingOccurrences(of: "/", with: "\\"))])
    }
    private static func unknown(_ command: String) -> JSONValue { .object(["type": .string("unknown"), "command": .string(command)]) }
    private static func out(_ text: String) -> JSONValue { .array([.object(["type": .string("text"), "text": .string(text)])]) }

    private static let exportOld = """
        import calendar
        from datetime import date


        def quarter_range(year: int, quarter: int) -> tuple[date, date]:
            start = date(year, quarter * 3 - 2, 1)
            end = date(year, quarter * 3, 31)
            return start, end


        def export(year: int, quarter: int, out: str) -> None:
            start, end = quarter_range(year, quarter)
            rows = load_rows(start, end)
            render_pdf(rows, out)
        """
    private static var exportNew: String {
        exportOld
            .replacingOccurrences(of: "    end = date(year, quarter * 3, 31)", with: "    last = calendar.monthrange(year, quarter * 3)[1]\n    end = date(year, quarter * 3, last)")
            .replacingOccurrences(of: "    render_pdf(rows, out)", with: "    render_pdf(rows, out, title=f\"Q{quarter} {year}\")")
    }
    private static let exportDiff = """
        @@ -4,9 +4,10 @@ from datetime import date

         def quarter_range(year: int, quarter: int) -> tuple[date, date]:
             start = date(year, quarter * 3 - 2, 1)
        -    end = date(year, quarter * 3, 31)
        +    last = calendar.monthrange(year, quarter * 3)[1]
        +    end = date(year, quarter * 3, last)
             return start, end


         def export(year: int, quarter: int, out: str) -> None:
             start, end = quarter_range(year, quarter)
             rows = load_rows(start, end)
        -    render_pdf(rows, out)
        +    render_pdf(rows, out, title=f"Q{quarter} {year}")

        """
    private static let testNew = """
        import pytest
        from datetime import date

        from export import quarter_range


        @pytest.mark.parametrize("quarter, last", [(1, 31), (2, 30), (3, 30), (4, 31)])
        def test_quarter_end(quarter, last):
            assert quarter_range(2026, quarter)[1] == date(2026, quarter * 3, last)

        """
    private static let answer1 = """
        Fixed — `quarter_range` always used **day 31** as the quarter's last day, which does not exist for June and September, so Q2 and Q3 raised `ValueError`.

        **What changed**
        - `export.py`: the end date now comes from `calendar.monthrange`, and the PDF title names the quarter.
        - `tests/test_quarters.py`: one case per quarter of 2026.

        ```powershell
        python -m pytest -q
        ```

        All 16 tests pass.
        """

    /// The session's two finished turns, numbered from 0 like a host's events (each with its own time).
    static func codexFixtureLog(now: Double) -> [[String: JSONValue]] {
        let w = codexFixtureCwd
        func t(_ v: String) -> JSONValue { .string(v) }
        let turn1: [[String: JSONValue]] = [
            ["t": t("state"), "state": t("running"), "queued": .array([])],
            ["t": t("user"), "input_id": t("cu1"), "by": .object(["device": t("home-laptop")]), "text": t("The Q3 export crashes with a date error. Find the cause, fix it and add a test.")],
            ["t": t("turn_start")],
            ["t": t("thought"), "id": t("cr1"), "text": t("**Locating the date math**\n\nThe error mentions a day out of range, so the quarter end is probably built with a fixed day."), "delta": .bool(false)],
            ["t": t("message"), "id": t("cm1"), "text": t("I'll find where the quarter dates are computed, then reproduce the crash."), "delta": .bool(false), "phase": t("commentary")],
            ["t": t("tool_call"), "call_id": t("cc1"), "title": t("Shell rg -n \"quarter_range|date\\(\" -g '*.py'"), "kind": t("execute"), "status": t("in_progress"),
             "input": cmd("rg -n \"quarter_range|date\\(\" -g '*.py'", [.object(["type": t("search"), "command": t("rg -n ..."), "query": t("quarter_range|date\\("), "path": .null])])],
            ["t": t("tool_update"), "call_id": t("cc1"), "status": t("completed"), "content": out("export.py:5:def quarter_range(year: int, quarter: int) -> tuple[date, date]:\nexport.py:6:    start = date(year, quarter * 3 - 2, 1)\nexport.py:7:    end = date(year, quarter * 3, 31)\nexport.py:12:    start, end = quarter_range(year, quarter)"),
             "exit_code": .number(0), "duration_ms": .number(140)],
            ["t": t("tool_call"), "call_id": t("cc2"), "title": t("Shell sed -n '1,120p' export.py"), "kind": t("execute"), "status": t("in_progress"), "input": cmd("sed -n '1,120p' export.py", [read("export.py")])],
            ["t": t("tool_update"), "call_id": t("cc2"), "status": t("completed"), "content": out(exportOld), "exit_code": .number(0), "duration_ms": .number(60)],
            ["t": t("tool_call"), "call_id": t("cc3"), "title": t("Shell ls tests"), "kind": t("execute"), "status": t("in_progress"),
             "input": cmd("ls tests", [.object(["type": t("listFiles"), "command": t("ls tests"), "path": t("tests")])])],
            ["t": t("tool_update"), "call_id": t("cc3"), "status": t("completed"), "content": out("test_export.py\ntest_rows.py"), "exit_code": .number(0), "duration_ms": .number(40)],
            ["t": t("tool_call"), "call_id": t("cc4"), "title": t("Shell python export.py --quarter Q3"), "kind": t("execute"), "status": t("in_progress"),
             "input": cmd("python export.py --quarter Q3", [unknown("python export.py --quarter Q3")])],
            ["t": t("tool_update"), "call_id": t("cc4"), "status": t("failed"), "content": .array([
                .object(["type": t("text"), "text": t("Traceback (most recent call last):\n  File \"D:\\work\\report-tool\\export.py\", line 7, in quarter_range\n    end = date(year, quarter * 3, 31)\nValueError: day is out of range for month\n")]),
                .object(["type": t("text"), "text": t("(exit code 1)")])]), "exit_code": .number(1), "duration_ms": .number(1340)],
            ["t": t("plan"), "entries": .array([
                .object(["content": t("Reproduce the Q3 crash"), "status": t("completed")]), .object(["content": t("Use each month's real last day"), "status": t("in_progress")]),
                .object(["content": t("Add a test per quarter"), "status": t("pending")]), .object(["content": t("Run the suite"), "status": t("pending")])])],
            ["t": t("message"), "id": t("cm2"), "text": t("Reproduced: the end date is always day 31. I'll take the real last day from `calendar.monthrange` and cover every quarter in a test."), "delta": .bool(false), "phase": t("commentary")],
            ["t": t("tool_call"), "call_id": t("cf1"), "title": t("edit \(w)\\export.py, \(w)\\tests\\test_quarters.py"), "kind": t("edit"), "status": t("in_progress"),
             "input": .object(["files": .array([
                .object(["path": t("\(w)\\export.py"), "kind": t("update"), "diff": t(exportDiff)]),
                .object(["path": t("\(w)\\tests\\test_quarters.py"), "kind": t("add"), "diff": t(testNew)])])]),
             "content": .array([
                .object(["type": t("diff"), "path": t("\(w)\\export.py"), "old": t(exportOld), "new": t(exportNew)]),
                .object(["type": t("diff"), "path": t("\(w)\\tests\\test_quarters.py"), "old": .null, "new": t(testNew)])]),
             "locations": .array([t("\(w)\\export.py"), t("\(w)\\tests\\test_quarters.py")])],
            ["t": t("tool_update"), "call_id": t("cf1"), "status": t("completed")],
            ["t": t("tool_call"), "call_id": t("cc5"), "title": t("Shell python -m pytest -q"), "kind": t("execute"), "status": t("in_progress"),
             "input": cmd("python -m pytest -q", [unknown("python -m pytest -q")])],
            ["t": t("tool_update"), "call_id": t("cc5"), "status": t("completed"), "content": out("................                                                   [100%]\n16 passed in 1.84s\n"),
             "exit_code": .number(0), "duration_ms": .number(2410)],
            ["t": t("plan"), "entries": .array([
                .object(["content": t("Reproduce the Q3 crash"), "status": t("completed")]), .object(["content": t("Use each month's real last day"), "status": t("completed")]),
                .object(["content": t("Add a test per quarter"), "status": t("completed")]), .object(["content": t("Run the suite"), "status": t("completed")])])],
            ["t": t("message"), "id": t("cm3"), "text": t(answer1), "delta": .bool(false), "phase": t("final_answer")],
            ["t": t("usage"), "input_tokens": .number(24_100), "output_tokens": .number(1_120), "context_window": .number(258_000), "context_used": .number(38_900)],
            ["t": t("turn_end"), "status": t("completed"), "final_text": t(answer1), "duration_ms": .number(83_400)],
            ["t": t("state"), "state": t("idle"), "queued": .array([])],
        ]
        let turn2: [[String: JSONValue]] = [
            ["t": t("state"), "state": t("running"), "queued": .array([])],
            ["t": t("user"), "input_id": t("cu2"), "by": .object(["device": t("home-laptop")]), "text": t("Does monthrange handle leap years?")],
            ["t": t("turn_start")],
            ["t": t("thought"), "id": t("cr2"), "text": t("**Recalling monthrange semantics**"), "delta": .bool(false)],
            ["t": t("message"), "id": t("cm4"), "text": t("Yes. `calendar.monthrange(year, month)` returns the weekday of the 1st and the number of days in that month, and it knows leap years: `monthrange(2024, 2)` is `(3, 29)`, `monthrange(2026, 2)` is `(6, 28)`. Quarters never end in February, so the export is not affected either way."),
             "delta": .bool(false), "phase": t("final_answer")],
            ["t": t("turn_end"), "status": t("completed"), "duration_ms": .number(6_100)],
            ["t": t("state"), "state": t("idle"), "queued": .array([])],
        ]
        let t0 = now - 4 * 60_000
        let numbered = turn1.map { ($0, $0["t"]?.string == "state" || $0["t"]?.string == "user" ? 0 : 1) }
            + turn2.map { ($0, $0["t"]?.string == "state" || $0["t"]?.string == "user" ? 1 : 2) }
        return numbered.enumerated().map { i, pair in
            var e = pair.0
            e["turn"] = .number(Double(pair.1))
            e["ts"] = .number(t0 + Double(i) * 3300)
            return e
        }
    }

    /// A Codex turn as a Codex host streams it, up to a command that keeps running (until Stop): thinking,
    /// a remark, a search and a read, a plan update, a web search, then the tests.
    func fixtureCodexTurn(_ n: Double) {
        let s = Self.self
        func t(_ v: String) -> JSONValue { .string(v) }
        let k = Int(n)
        let events: [[String: JSONValue]] = [
            ["t": t("thought"), "id": t("r\(k)"), "text": t("**Checking February handling**\n\nLeap years change February's length; the report also has a monthly mode that may build month ends by hand.")],
            ["t": t("message"), "id": t("m\(k)"), "text": t("I'll look for other places that build month ends by hand before changing anything."), "phase": t("commentary")],
            ["t": t("tool_call"), "call_id": t("a\(k)"), "title": t("Shell rg -n \"monthrange|, 31\\)|, 30\\)\" -g '*.py'"), "kind": t("execute"), "status": t("in_progress"),
             "input": s.cmd("rg -n \"monthrange|, 31\\)|, 30\\)\" -g '*.py'", [.object(["type": t("search"), "command": t("rg"), "query": t("monthrange|, 31\\)|, 30\\)"), "path": .null])])],
            ["t": t("tool_update"), "call_id": t("a\(k)"), "status": t("completed"), "content": s.out("export.py:8:    last = calendar.monthrange(year, quarter * 3)[1]\nreport/monthly.py:14:    end = date(year, month, 30)"),
             "exit_code": .number(0), "duration_ms": .number(700)],
            ["t": t("tool_call"), "call_id": t("b\(k)"), "title": t("Shell sed -n '1,80p' report/monthly.py"), "kind": t("execute"), "status": t("in_progress"),
             "input": s.cmd("sed -n '1,80p' report/monthly.py", [s.read("report/monthly.py")])],
            ["t": t("tool_update"), "call_id": t("b\(k)"), "status": t("completed"), "exit_code": .number(0), "duration_ms": .number(500),
             "content": s.out("from datetime import date\n\n\ndef month_range(year: int, month: int) -> tuple[date, date]:\n    start = date(year, month, 1)\n    end = date(year, month, 30)\n    return start, end\n")],
            ["t": t("plan"), "entries": .array([
                .object(["content": t("Find hand-built month ends"), "status": t("completed")]), .object(["content": t("Fix report/monthly.py"), "status": t("in_progress")]),
                .object(["content": t("Test February in 2024 and 2026"), "status": t("pending")])])],
            ["t": t("tool_call"), "call_id": t("w\(k)"), "title": t("search python calendar.monthrange leap year"), "kind": t("fetch"), "status": t("in_progress"),
             "input": .object(["query": t("python calendar.monthrange leap year")])],
            ["t": t("tool_update"), "call_id": t("w\(k)"), "status": t("completed"), "content": s.out("searched python calendar.monthrange leap year")],
            ["t": t("tool_call"), "call_id": t("c\(k)"), "title": t("Shell python -m pytest -q tests/test_monthly.py"), "kind": t("execute"), "status": t("in_progress"),
             "input": s.cmd("python -m pytest -q tests/test_monthly.py", [s.unknown("python -m pytest -q tests/test_monthly.py")])],
            ["t": t("tool_update"), "call_id": t("c\(k)"), "content": s.out("F.")],
        ]
        for var e in events {
            e["turn"] = .number(n)
            fixtureEvent(e)
        }
    }
}
#endif
