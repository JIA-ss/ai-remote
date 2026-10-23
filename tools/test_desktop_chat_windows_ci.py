"""Portable behavior checks for Windows CI evidence, without starting any native process."""
import ast
import contextlib
import io
import importlib.util
import json
import os
import sys
import subprocess
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

    def ci_source_adapter(self):
        tree = ast.parse(Path(__file__).with_name('desktop-chat-e2e.py').read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'ci_driver_source')
        namespace = {}
        exec(compile(ast.Module(body=[function], type_ignores=[]), 'ci_driver_source', 'exec'), namespace)
        return namespace['ci_driver_source']

    def test_ci_adapter_requires_one_known_connection_and_supports_all_six_drivers(self):
        adapt = self.ci_source_adapter()
        for _, filename, _ in ci.SUITES:
            original = Path(__file__).with_name(filename).read_text()
            rewritten = adapt(original)
            self.assertIn('const invoke = (cmd, args) => window.__smokeCiInvoke(cmd, args);', rewritten)
            self.assertIn('const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);', original)
        for unsupported in ('', 'const invoke = (cmd,args) => anotherBackend(cmd,args);',
                            'const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);' * 2):
            with self.assertRaises(ValueError):
                adapt(unsupported)

    def test_ci_done_with_readonly_tauri_keeps_native_forwarding_and_final_evidence(self):
        harness = Path(__file__).with_name('desktop-chat-e2e.py').read_text()
        hook = next(node.value.value for node in ast.parse(harness).body if isinstance(node, ast.Assign)
                    and any(isinstance(target, ast.Name) and target.id == 'SMOKE_CI_CAPTURE_LAYOUT' for target in node.targets))
        adapter = self.ci_source_adapter()('const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);')
        script = """
const vm = require('node:vm');
const events = [], internals = {}, payload = {session:'fixture',nested:{value:3}}, options = {request:'sentinel'};
let forwarded;
const nativeResult = {answer:17};
const originalInvoke = async function(command,args,...rest) {
  events.push({command,...args});
  if(command === 'call_tool') forwarded = {bound:this===internals,payload:args===payload,options:rest[0]===options};
  return nativeResult;
};
// Match Cargo.lock's Tauri 2.12.2 core.js: value-only means nonwritable/nonconfigurable.
Object.defineProperty(internals,'invoke',{value:originalInvoke});
const window = {__smokePrepareShot:async(name)=>events.push({prepare:name}),__TAURI_INTERNALS__:internals};
const devices = {getBoundingClientRect:()=>({x:0,y:0,width:1280,height:800})};
const document = {querySelector:(selector)=>selector === '.win[data-route="devices"]' ? devices : null};
const context = {window,document,innerWidth:1280,innerHeight:800,devicePixelRatio:1,
  getComputedStyle:()=>({visibility:'visible',borderTopLeftRadius:'0',borderTopRightRadius:'0',borderBottomLeftRadius:'0',borderBottomRightRadius:'0'})};
vm.createContext(context); vm.runInContext(HOOK,context); vm.runInContext(ADAPTER+';globalThis.driverInvoke=invoke;',context);
(async()=>{
 const businessResult = await window.__smokeCiInvoke('call_tool',payload,options);
 await context.driverInvoke('log_from_page',{level:'e2e',message:'ok    first'});
 await context.driverInvoke('log_from_page',{level:'e2e',message:'FAIL  second'});
 const result = await context.driverInvoke('log_from_page',{level:'done',message:'1'});
 const descriptor=Object.getOwnPropertyDescriptor(internals,'invoke');
 process.stdout.write(JSON.stringify({events,forwarded,resultUnchanged:result===nativeResult&&businessResult===nativeResult,
   nativeUnchanged:internals.invoke===originalInvoke,readonly:descriptor.writable===false&&descriptor.configurable===false}));
})().catch(()=>process.exit(1));
""".replace('HOOK', json.dumps(hook)).replace('ADAPTER', json.dumps(adapter))
        process = subprocess.run(['node', '-'], input=script, text=True, capture_output=True, check=True)
        evidence = json.loads(process.stdout)
        self.assertTrue(evidence['nativeUnchanged'])
        self.assertTrue(evidence['readonly'])
        self.assertTrue(evidence['resultUnchanged'])
        self.assertEqual(evidence['forwarded'], {'bound': True, 'payload': True, 'options': True})
        final = evidence['events'][3:]
        self.assertEqual(final[0], {'prepare': 'zz-final'})
        self.assertEqual(json.loads(final[1]['message'])['kind'], 'devices_ui')
        self.assertEqual(json.loads(final[1]['message'])['name'], 'zz-final')
        self.assertEqual(final[2]['message'], '2 checks')
        self.assertEqual(final[3]['level'], 'done')

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
                    app_exit_codes=[0], isolated_storage=True, isolated_placement=True, reported_check_counts=[12],
                    images=[{'valid': True, 'crop_status': 'saved_test_ui_only'}])

    def motion_line(self, payload):
        return '     (live tool normal paint ' + json.dumps(payload) + ')'

    def test_motion_preferences_export_only_two_typed_values(self):
        for system in (True, False):
            for app in (None, 'true', 'false'):
                payload = {'systemReduce': system, 'appReduce': app,
                           'line': {'text': 'private-command', 'color': 'private-color'},
                           'parts': [{'text': r'C:\Users\Alice\auth.secret'}]}
                parsed = ci.parse_log(self.motion_line(payload), '')
                self.assertEqual(parsed['motion_preferences'], [{'systemReduce': system, 'appReduce': app}])
                for raw in ('private-command', 'private-color', 'Alice', 'auth.secret', 'parts'):
                    self.assertNotIn(raw, json.dumps(parsed))

    def test_motion_preferences_retain_invalid_records_without_raw_values(self):
        invalid = [{'systemReduce': value, 'appReduce': None} for value in (None, 0, 1, 'true', 'false', [], {})]
        invalid += [{'systemReduce': False}, {'systemReduce': True, 'appReduce': 'private-root'},
                    {'systemReduce': True, 'appReduce': 1}, {'systemReduce': True, 'appReduce': True}]
        for payload in invalid:
            with self.subTest(payload=payload):
                parsed = ci.parse_log(self.motion_line(payload), '')
                self.assertEqual(parsed['motion_preferences'], [None])
                self.assertNotIn('private-root', json.dumps(parsed))
        for line in ('     (live tool normal paint {bad})',
                     '     (live tool normal paint {"systemReduce":true,"appReduce":null}) trailing'):
            self.assertEqual(ci.parse_log(line, '')['motion_preferences'], [None])

    def test_chat_requires_exactly_one_valid_normal_motion_record(self):
        valid = {'systemReduce': False, 'appReduce': None}
        self.assertEqual(ci.parse_log('', '')['motion_preferences'], [])
        good_chat = {**self.good(), 'suite': 'chat', 'motion_preferences': [valid]}
        self.assertTrue(ci.accepted(good_chat))
        for preferences in (None, [], [None], [valid, valid], [valid, None],
                            [{'systemReduce': 1, 'appReduce': None}],
                            [{'systemReduce': False, 'appReduce': 'private-root'}]):
            with self.subTest(preferences=preferences):
                self.assertFalse(ci.accepted({**good_chat, 'motion_preferences': preferences}))
        self.assertFalse(ci.accepted({**self.good(), 'suite': 'chat'}))
        duplicate = ci.parse_log(self.motion_line(valid) + '\n' + self.motion_line(valid), '')
        self.assertFalse(ci.accepted({**self.good(), 'suite': 'chat', **duplicate}))
        for suite in ('render', 'offline', 'preview', 'codex', 'workspace'):
            self.assertTrue(ci.accepted({**self.good(), 'suite': suite}), suite)

    def test_success_requires_harness_app_isolation_assertions_and_cleanup(self):
        self.assertTrue(ci.accepted(self.good()))
        for change in ({'harness_exit': 1}, {'app_exit_codes': [0, 0]}, {'app_exit_codes': [1]},
                       {'all_passed_count': 0}, {'all_passed_count': 2}, {'ok_count': 0}, {'fail_count': 1},
                       {'failed_summary_counts': [1]}, {'cleanup_ok': False}, {'timed_out': True},
                       {'isolated_storage': False}, {'isolated_placement': False}, {'images': []},
                       {'images': [{'valid': False}]}, {'images': [{'valid': True, 'crop_status': 'omitted_no_reliable_layout'}]}, {'reported_check_counts': []}, {'runner_error_stage': 'read_process_exit'}, {'reported_check_counts': [99]}):
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

    def live_workspace_layout(self, mode='visible', inert=False):
        tree = ast.parse(Path(__file__).with_name('desktop-chat-e2e.py').read_text())
        hook = next(node.value.value for node in tree.body if isinstance(node, ast.Assign)
                    and any(isinstance(target, ast.Name) and target.id == 'SMOKE_CI_CAPTURE_LAYOUT'
                            for target in node.targets))
        script = """
const vm = require('node:vm');
const scene = SCENE, events = [], internals = {};
const node = (rect, visibility='visible') => ({getBoundingClientRect:()=>rect,visibility});
const whole = node({x:0,y:0,width:784,height:448});
const video = node({x:0,y:0,width:400,height:448});
const pane = node(scene.mode === 'zero' ? {x:400,y:0,width:0,height:0}
  : {x:400,y:0,width:384,height:448}, scene.mode === 'hidden' ? 'hidden' : 'visible');
pane.inert = scene.inert;
// The embedded workspace is a child of the pane; the video is its sibling, never its parent.
const embedded = node({x:400,y:36,width:384,height:412});
whole.children = [video,pane]; pane.children = [embedded];
const choices = new Map([
 ['.nt-live-workspace',whole], ['.nt-live-workspace > .nt-workspace-pane',pane],
 ['.nt-rail',pane], ['.nt-rail:not(.nt-workspace-pane)',null],
 ['.ws',embedded], ['.ws:not([data-embedded="true"])',null],
]);
Object.defineProperty(internals,'invoke',{value:async(command,args)=>events.push({command,...args})});
const window = {__TAURI_INTERNALS__:internals,__smokePrepareShot:async()=>{}};
const context = {window,document:{querySelector:(selector)=>choices.get(selector)??null},
 innerWidth:784,innerHeight:448,devicePixelRatio:1,
 getComputedStyle:(el)=>({visibility:el.visibility,borderTopLeftRadius:'0',borderTopRightRadius:'0',
   borderBottomLeftRadius:'0',borderBottomRightRadius:'0'})};
vm.createContext(context); vm.runInContext(HOOK,context);
window.__smokePrepareShot('case').then(()=>process.stdout.write(
 events.find(event=>event.level==='ci-capture-layout').message)).catch(()=>process.exit(1));
""".replace('SCENE', json.dumps({'mode': mode, 'inert': inert})).replace('HOOK', json.dumps(hook))
        result = subprocess.run(['node', '-'], input=script, text=True, capture_output=True, check=True)
        return json.loads(result.stdout)

    def test_live_workspace_exports_the_complete_pane_without_video_sibling_pixels(self):
        for inert in (False, True):
            with self.subTest(visible_inert=inert), tempfile.TemporaryDirectory() as tmp:
                raw = Path(tmp)/'raw'; out = Path(tmp)/'out'; raw.mkdir()
                layout = self.live_workspace_layout(inert=inert)
                self.assertEqual(layout['kind'], 'stream_workspace_ui')
                self.assertEqual(layout['rect'], {'x': 400, 'y': 0, 'width': 384, 'height': 448})
                # Sensitive video/caption pixels are red; only the entire test pane has red=0.
                image = Image.new('RGB', (800, 480), (255, 0, 0))
                gradient = Image.linear_gradient('L').resize((384, 448))
                safe = Image.merge('RGB', (Image.new('L', gradient.size), gradient, gradient.transpose(Image.Transpose.FLIP_TOP_BOTTOM)))
                image.paste(safe, (408, 24)); image.save(raw/'case.png')
                (raw/'ci-layout.json').write_text(json.dumps({
                    'native': {'original_size': [800, 480], 'client_box': [8, 24, 792, 472]},
                    'shots': {'case': layout}}))
                before = ci.digest(raw/'case.png')
                record, = ci.collect_images(raw, out)
                self.assertEqual(record['crop_status'], 'saved_test_ui_only')
                self.assertEqual(record['crop_box'], [408, 24, 792, 472])
                self.assertEqual(record['crop_size'], [384, 448], 'include sidebar and approval space, not only chat')
                with Image.open(out/'case.png') as cropped:
                    self.assertEqual(cropped.getchannel('R').tobytes().count(b'\xff'), 0,
                                     'no synthetic video/caption pixel may be exported')
                    self.assertEqual(cropped.getchannel('R').getextrema(), (0, 0))
                self.assertEqual(ci.digest(raw/'case.png'), before)

    def test_unavailable_live_pane_never_falls_back_to_video_parent_or_embedded_workspace(self):
        for mode in ('hidden', 'zero'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                raw = Path(tmp)/'raw'; out = Path(tmp)/'out'; raw.mkdir()
                layout = self.live_workspace_layout(mode=mode)
                self.assertEqual(layout['kind'], 'unavailable')
                self.assertIsNone(layout['rect'], 'visible parent/embedded geometry is not a safe fallback')
                image = Image.merge('RGB', (Image.new('L', (800, 480), 255),
                    Image.linear_gradient('L').resize((800, 480)), Image.new('L', (800, 480))))
                image.save(raw/'case.png')
                (raw/'ci-layout.json').write_text(json.dumps({
                    'native': {'original_size': [800, 480], 'client_box': [8, 24, 792, 472]},
                    'shots': {'case': layout}}))
                record, = ci.collect_images(raw, out)
                self.assertEqual(record['crop_status'], 'omitted_no_reliable_layout')
                self.assertNotIn('crop_file', record)
                self.assertFalse(out.exists())

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
