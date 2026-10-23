"""Portable behavior checks for Windows CI evidence, without starting any native process."""
import contextlib
import io
import importlib.util
import json
import os
import sys
from pathlib import Path
import tempfile
import unittest
from PIL import Image

SPEC = importlib.util.spec_from_file_location("windows_chat_ci", Path(__file__).with_name("desktop-chat-windows-ci.py"))
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)


class EvidenceTests(unittest.TestCase):
    def test_setup_exceptions_do_not_print_private_paths(self):
        def broken_setup():
            raise FileExistsError(r"C:\Users\Alice\private-auth-home")
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = ci.entry_point(broken_setup)
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(stdout.getvalue())["runner_error_stage"], "runner_setup_error")
        self.assertEqual(stderr.getvalue(), "")
        self.assertNotIn("Alice", stdout.getvalue())

    def test_log_counts_are_exact_and_raw_detail_is_not_exported(self):
        source = 'await check("a case (with parentheses)", true); await check("another", true);'
        raw = '\n'.join(['ok    a case (with parentheses)  (C:\\Users\\Alice\\auth.secret)',
                         'ok    dynamic Alice auth.secret  (private)', 'FAIL  another  (token=secret)',
                         'all passed', '  app exited with 0', '[smoke] isolated nonpersistent webview storage',
                         '[smoke] geometry {"window":{"x":-8000,"y":-8000,"width":1280,"height":800},"screens":[{"x":0,"y":0,"width":1920,"height":1080}]}'])
        parsed = ci.parse_log(raw, source)
        self.assertEqual((parsed['ok_count'], parsed['fail_count']), (2, 1))
        self.assertEqual(parsed['checks'][0]['name'], 'a case (with parentheses)')
        self.assertEqual(parsed['checks'][1]['name'], 'dynamic check')
        self.assertTrue(parsed['isolated_placement'])
        exported = json.dumps(parsed)
        for secret in ('Alice', 'auth.secret', 'token=secret', 'C:\\Users'):
            self.assertNotIn(secret, exported)

    def good(self):
        return dict(harness_exit=0, cleanup_ok=True, timed_out=False, runner_error_stage=None,
                    all_passed_count=1, ok_count=12, fail_count=0, failed_summary_counts=[],
                    app_exit_codes=[0], isolated_storage=True, isolated_placement=True, images=[{'valid': True}])

    def test_success_requires_harness_app_isolation_assertions_and_cleanup(self):
        self.assertTrue(ci.accepted(self.good()))
        for change in ({'harness_exit': 1}, {'app_exit_codes': [0, 0]}, {'app_exit_codes': [1]},
                       {'all_passed_count': 0}, {'all_passed_count': 2}, {'ok_count': 0}, {'fail_count': 1},
                       {'failed_summary_counts': [1]}, {'cleanup_ok': False}, {'timed_out': True},
                       {'isolated_storage': False}, {'isolated_placement': False}, {'images': []},
                       {'images': [{'valid': False}]}, {'runner_error_stage': 'read_process_exit'}, {'reported_check_counts': [99]}):
            with self.subTest(change=change):
                self.assertFalse(ci.accepted({**self.good(), **change}))

    def test_fixed_rounds_never_hide_a_failed_first_round(self):
        calls, report = [], {'runs': []}
        def run(suite, number):
            calls.append((suite[0], number))
            return {'suite': suite[0], 'round': number, 'passed': number == 2}
        list(ci.execute(run, report))
        self.assertEqual(len(calls), 12)
        self.assertEqual(calls, [(suite[0], number) for suite in ci.SUITES for number in (1, 2)])
        self.assertEqual(sum(r['passed'] for r in report['runs']), 6)
        self.assertFalse(all(r['passed'] for r in report['runs']))

    def test_client_origin_maps_only_the_observed_chat_rail(self):
        native = {'original_size': [1300, 840], 'client_box': [8, 31, 1292, 831]}
        layout = {'kind': 'stream_chat_rail', 'viewport': {'width': 1284, 'height': 800, 'dpr': 1},
                  'rect': {'x': 852, 'y': 0, 'width': 432, 'height': 800}}
        self.assertEqual(ci.crop_box(layout, native, (1300, 840)), [860, 31, 1292, 831])
        for bad in ({**layout, 'kind': 'desktop'}, {**layout, 'rect': {**layout['rect'], 'x': -1}},
                    {**layout, 'viewport': {**layout['viewport'], 'dpr': 2}},
                    {**layout, 'rect': {**layout['rect'], 'width': float('nan')}}):
            self.assertIsNone(ci.crop_box(bad, native, (1300, 840)))
        self.assertIsNone(ci.crop_box(layout, native, (1301, 840)))
        self.assertIsNone(ci.crop_box(None, native, (1300, 840)))

    def test_export_is_a_crop_with_hashes_and_never_copies_raw_logs_or_auth(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); raw = root/'raw'; out = root/'evidence'; raw.mkdir()
            image = Image.new('RGB', (800, 480), 'red')
            for x in range(408, 792):
                for y in range(24, 472):
                    image.putpixel((x, y), (0, x % 256, y % 256))
            image.save(raw/'case.png')
            (raw/'host.log').write_text('Alice token=secret')
            (raw/'auth.json').write_text('do not upload')
            layout = {'native': {'original_size': [800, 480], 'client_box': [8, 24, 792, 472]},
                      'shots': {'case': {'kind': 'stream_chat_rail', 'viewport': {'width': 784, 'height': 448, 'dpr': 1},
                                         'rect': {'x': 400, 'y': 0, 'width': 384, 'height': 448}, 'radius': 0}}}
            (raw/'ci-layout.json').write_text(json.dumps(layout))
            before = ci.digest(raw/'case.png')
            records = ci.collect_images(raw, out)
            self.assertEqual([p.name for p in out.iterdir()], ['case.png'])
            self.assertEqual(records[0]['crop_box'], [408, 24, 792, 472])
            self.assertEqual(records[0]['crop_size'], [384, 448])
            self.assertEqual(records[0]['original_sha256'], before)
            self.assertEqual(ci.digest(raw/'case.png'), before)
            with Image.open(out/'case.png') as cropped:
                self.assertEqual(cropped.getpixel((0, 0))[0], 0)
            self.assertNotIn('Alice', json.dumps(records))

    def test_missing_layout_and_flat_or_invalid_captures_are_explicit(self):
        with tempfile.TemporaryDirectory() as tmp:
            raw = Path(tmp)/'raw'; out = Path(tmp)/'out'; raw.mkdir()
            image = Image.new('RGB', (800, 480), 'black'); image.save(raw/'flat.png')
            image.save(raw/'wrong.png', format='JPEG')
            records = ci.collect_images(raw, out)
            self.assertTrue(all(not record['valid'] for record in records))
            self.assertFalse(out.exists())
            self.assertTrue(all(record['visual_qa'] == 'pending' for record in records))


class WindowsContainmentTests(unittest.TestCase):
    @unittest.skipUnless(os.name == 'nt', 'Win32 execution is checked on Windows')
    def test_job_runs_a_child_and_only_returns_typed_status(self):
        from desktop_chat_ci_windows import run_scoped
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            result = run_scoped([sys.executable, '-c', 'print("private probe output")'], cwd=root,
                                env=dict(os.environ), log=root/'private.log', timeout_seconds=10)
            self.assertEqual(result['returncode'], 0)
            self.assertTrue(result['cleanup_ok'])
            self.assertFalse(result['timed_out'])
            self.assertIsNone(result['error'])
            self.assertNotIn('private probe output', json.dumps(result))

    @unittest.skipUnless(os.name == 'nt', 'Win32 execution is checked on Windows')
    def test_timeout_stops_the_scoped_parent_and_descendant(self):
        from desktop_chat_ci_windows import run_scoped
        program = 'import subprocess,sys,time; subprocess.Popen([sys.executable,"-c","import time; time.sleep(120)"]); time.sleep(120)'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            result = run_scoped([sys.executable, '-c', program], cwd=root, env=dict(os.environ),
                                log=root/'private.log', timeout_seconds=1)
            self.assertTrue(result['timed_out'])
            self.assertTrue(result['cleanup_ok'])
            self.assertEqual(result['returncode'], 124)


if __name__ == '__main__':
    unittest.main()
