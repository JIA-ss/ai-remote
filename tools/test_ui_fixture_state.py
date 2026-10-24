"""State-file races must not invalidate completed native UI checks."""

import json
from pathlib import Path
import unittest
from unittest.mock import patch

from ui_fixture_state import read_fixture_state


class FixtureStateTests(unittest.TestCase):
    def setUp(self):
        self.path = Path("disposable-fixture.json")
        self.value = {"pid": 42, "clicks": 2, "text": "中文 literal {ENTER}", "checked": True}
        self.complete = json.dumps(self.value, ensure_ascii=False)

    def test_truncated_then_complete_state_is_read_fresh(self):
        for incomplete in ("", '{"pid":', '{"text":"中文'):
            with self.subTest(incomplete=incomplete):
                with patch.object(Path, "read_text", side_effect=[incomplete, self.complete]) as read:
                    self.assertEqual(read_fixture_state(self.path), self.value)
                    self.assertEqual(read.call_count, 2)

    def test_incomplete_utf8_and_missing_file_can_finish_writing(self):
        incomplete = UnicodeDecodeError("utf-8", b"\xe4", 0, 1, "unexpected end of data")
        for error in (incomplete, FileNotFoundError("fixture is being replaced")):
            with self.subTest(error=type(error).__name__):
                with patch.object(Path, "read_text", side_effect=[error, self.complete]):
                    self.assertEqual(read_fixture_state(self.path), self.value)

    def test_persistently_invalid_state_still_fails_within_the_bound(self):
        with patch.object(Path, "read_text", return_value="{"):
            with self.assertRaisesRegex(TimeoutError, "fixture state remained incomplete"):
                read_fixture_state(self.path, timeout=0.02, poll_interval=0.001)

    def test_a_later_read_never_reuses_an_earlier_state(self):
        later = {**self.value, "clicks": 3}
        with patch.object(Path, "read_text", side_effect=[self.complete, json.dumps(later)]):
            self.assertEqual(read_fixture_state(self.path), self.value)
            self.assertEqual(read_fixture_state(self.path), later)


if __name__ == "__main__":
    unittest.main()
