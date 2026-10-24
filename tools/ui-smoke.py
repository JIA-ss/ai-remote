#!/usr/bin/env python3
"""Cross-platform real MCP -> QUIC -> AX/UIA acceptance on a disposable native window.

Build: cargo build -p ai-remote. Requires an unlocked desktop and host accessibility
permission on macOS. Never uses a model or screenshots for targeting. Exits 2 for a
preflight blocker, 1 for a regression; report.json always preserves the outcome.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time

from ui_fixture_state import read_fixture_state

sys.stdout.reconfigure(encoding="utf-8")
ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--cli', type=Path, default=ROOT / ('target/debug/ai-remote.exe' if os.name == 'nt' else 'target/debug/ai-remote'))
parser.add_argument('--out', type=Path, default=ROOT / 'target/ui-validation')
opts = parser.parse_args()
CLI = opts.cli.resolve()
OUT = opts.out.resolve()
OUT.mkdir(parents=True, exist_ok=True)
report = {'platform': sys.platform, 'model_calls': 0, 'targeting': 'native_elements', 'result': 'running'}


def quiet_profiles(*homes):
    for home in homes:
        for role in ('host', 'controller'):
            directory = home / role
            directory.mkdir(parents=True, exist_ok=True)
            (directory / 'ai-remote.toml').write_bytes(b'[network]\nquiet = true\n')


def command(home, *args):
    p = subprocess.run([str(CLI), '--home', str(home), '--json', *map(str,args)], capture_output=True, text=True, encoding="utf-8", timeout=30)
    if p.returncode:
        raise RuntimeError(f'command failed ({p.returncode}): {p.stderr[-800:]}')
    return json.loads(p.stdout)


def wait_for(fn, label, timeout=15):
    deadline = time.monotonic()+timeout
    while time.monotonic()<deadline:
        try:
            value = fn()
            if value: return value
        except (json.JSONDecodeError, FileNotFoundError, RuntimeError):
            pass
        time.sleep(.1)
    raise AssertionError(f'timed out: {label}')


class Bridge:
    def __init__(self, home):
        self.p = subprocess.Popen([str(CLI),'--home',str(home),'mcp'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,encoding="utf-8",bufsize=1)
        self.q = queue.Queue()
        self.counter = 0
        threading.Thread(target=self.read,daemon=True).start()
        self.request('initialize', {'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'structured-ui-smoke','version':'1'}})
        self.send({'jsonrpc':'2.0','method':'notifications/initialized'})
    def read(self):
        for line in self.p.stdout:
            try: self.q.put(json.loads(line))
            except json.JSONDecodeError: self.q.put({'error': 'invalid MCP JSON'})
    def send(self, v):
        self.p.stdin.write(json.dumps(v,ensure_ascii=False)+'\n');self.p.stdin.flush()
    def request(self, method, params):
        self.counter+=1
        self.send({'jsonrpc':'2.0','id':self.counter,'method':method,'params':params})
        deadline=time.monotonic()+30
        while time.monotonic()<deadline:
            v=self.q.get(timeout=max(.01,deadline-time.monotonic()))
            if v.get('id')==self.counter:
                assert 'error' not in v, v
                return v['result']
        raise AssertionError('MCP timed out')
    def call(self, tool, args=None, error=False):
        v=self.request('tools/call', {'name':tool,'arguments':{**(args or {}),'host':'ui-e2e'}})
        assert bool(v.get('isError'))==error, v.get('content')
        if error or tool=='screenshot': return v
        assert all(c['type'] != 'image' for c in v.get('content',[])) or tool=='screenshot', 'unexpected image in text-first UI result'
        return v.get('structuredContent') or v
    def close(self):
        if self.p.stdin: self.p.stdin.close()
        try:self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:self.p.terminate();self.p.wait(timeout=5)


def select_node(tree, label):
    nodes=[n for n in tree['elements'] if n['label']==label]
    assert len(nodes)==1, f'{label}: expected one observed element, got {len(nodes)}'
    return nodes[0]


exit_code=0
with tempfile.TemporaryDirectory(prefix='ar-ui-', dir='/tmp' if sys.platform=='darwin' else None) as temp:
    base=Path(temp);host_home=base/'host';client_home=base/'controller';state_file=base/'fixture.json'
    quiet_profiles(host_home, client_home)
    host=fixture=None;bridges=[]
    log=open(OUT/'host.log','w')
    try:
        host=subprocess.Popen([str(CLI),'--home',str(host_home),'host','--no-pair-code'],stdout=log,stderr=log)
        ticket=wait_for(lambda:command(host_home,'pair-code','--level','control').get('line'), 'isolated host ready',30)
        command(client_home,'pair',ticket,'--name','ui-e2e')
        bridge=Bridge(client_home);bridges.append(bridge)
        system=bridge.call('system_info')
        if not system['session']['desktop_usable']:
            report.update(result='blocked',reason=system['session'].get('problem','desktop unavailable'))
            exit_code=2
        else:
            doctor=bridge.call('doctor')
            network=next((check for check in doctor.get('checks',[]) if check.get('id')=='network'), None)
            assert network and network.get('grade')=='ok' and 'locked by quiet = true' in network.get('summary',''), 'isolated host must lock quiet network mode'
            report['network_quiet']={'configured_profiles':4, 'host_doctor_lock':True}
            if sys.platform=='darwin':
                checks=doctor.get('checks',[])
                if any(c.get('id')=='accessibility' and c.get('grade')=='fail' for c in checks):
                    report.update(result='blocked',reason='host Accessibility permission missing');exit_code=2
            if not exit_code:
                if sys.platform=='darwin':
                    executable=base/'fixture'
                    subprocess.run(['swiftc',str(ROOT/'tools/ui-fixture.swift'),'-o',str(executable)],check=True,capture_output=True,timeout=60)
                    fixture=subprocess.Popen([str(executable),str(state_file)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                elif os.name=='nt':
                    fixture=subprocess.Popen(['powershell.exe','-NoProfile','-STA','-File',str(ROOT/'tools/ui-fixture.ps1'),'-OutputPath',str(state_file)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                else: raise RuntimeError('native UI acceptance supports macOS and Windows')
                wait_for(lambda:state_file.exists() and json.loads(state_file.read_text(encoding="utf-8")).get('pid'),'fixture ready')
                pid=read_fixture_state(state_file)['pid']
                def state():return read_fixture_state(state_file)
                def tree(**args):return bridge.call('ui_tree',{'pid':pid,**args})
                def action(t,n,a,value=None,error=False):
                    args={'snapshot_id':t['snapshot_id'],'element_id':n['id'],'action':a}
                    if value is not None:args['value']=value
                    return bridge.call('ui_action',args,error=error)
                first=tree()
                (OUT/'initial-tree.json').write_text(json.dumps(first,ensure_ascii=False,indent=2),encoding="utf-8")
                assert not first['truncated'], first['warnings']
                assert first['source'] in ['macos_ax','windows_uia']
                assert 'snapshot_id='+first['snapshot_id'] in first['text']
                secure=select_node(first,'Password')
                assert secure['protected'] and secure['value'] is None and secure['actions']==[]
                assert 'fixture-secret-do-not-export' not in json.dumps(first)
                report['password_redaction']='passed'
                input_node=select_node(first,'Input')
                assert 'set_value' in input_node['actions']
                read=bridge.call('ui_read',{'snapshot_id':first['snapshot_id'],'element_id':input_node['id']})
                assert read['element']['value']==''
                second=Bridge(client_home);bridges.append(second)
                second.call('ui_read',{'snapshot_id':first['snapshot_id'],'element_id':input_node['id']},error=True)
                report['foreign_session']='refused'
                refreshed=tree()
                bridge.call('ui_read',{'snapshot_id':first['snapshot_id'],'element_id':input_node['id']},error=True)
                report['superseded_snapshot']='refused'
                literal='中文 literal {ENTER} $(not-a-command)'
                result=action(refreshed,select_node(refreshed,'Input'),'set_value',literal)
                assert result['status']=='dispatched' and not result['verified']
                wait_for(lambda:state()['text']==literal,'literal native text value')
                action(refreshed,select_node(refreshed,'Input'),'set_value','duplicate',error=True)
                assert state()['text']==literal
                report['literal_text_and_single_use']='passed'
                current=tree()
                assert select_node(current,'Input')['value']==literal
                action(current,select_node(current,'Apply'),'press')
                wait_for(lambda:state()['clicks']==1,'native button invocation')
                report['button_independent_verification']='passed'
                current=tree()
                action(current,select_node(current,'Disabled'),'press',error=True)
                report['disabled_control']='refused'
                current=tree()
                option=select_node(current,'Enabled option')
                action(current,option,'toggle' if 'toggle' in option['actions'] else 'press')
                wait_for(lambda:state()['checked'] is True,'native checkbox state')
                current=tree()
                assert select_node(current,'Enabled option')['checked'] is True
                report['checkbox_independent_verification']='passed'
                command(client_home,'take-over','--host','ui-e2e','--let-watch')
                command(client_home,'hand-back','--host','ui-e2e')
                action(current,select_node(current,'Apply'),'press',error=True)
                assert state()['clicks']==1
                current=tree()
                action(current,select_node(current,'Apply'),'press')
                wait_for(lambda:state()['clicks']==2,'fresh action after handback')
                report['takeover_invalidates_snapshot']='passed'
                small=tree(max_elements=1)
                assert small['truncated'] and small['observed_count']==1
                report['bounded_tree']='passed'
                filtered=tree(query='Apply')
                assert len(filtered['matched_ids'])==1
                assert select_node(filtered,'Apply')['id'] in filtered['matched_ids']
                report['text_query']='passed'
                (OUT/'final-tree.json').write_text(json.dumps(tree(),ensure_ascii=False,indent=2),encoding="utf-8")
                # Screenshot is evidence only; none of the target selection or actions used it.
                image=bridge.call('screenshot')
                pixels=next(c for c in image['content'] if c['type']=='image')
                (OUT/'acceptance.jpg').write_bytes(base64.b64decode(pixels['data']))
                audit=(host_home/'host/audit.jsonl').read_text(encoding='utf-8')
                assert literal not in audit, 'literal value leaked in audit'
                report['audit_value_redaction']='passed'
                report.update(result='passed',fixture=state(),observed_elements=first['observed_count'])
    except Exception as error:
        report.update(result='failed',reason=str(error));exit_code=1
    finally:
        for bridge in bridges:bridge.close()
        if fixture is not None:
            fixture.terminate()
            try:fixture.wait(timeout=5)
            except subprocess.TimeoutExpired:fixture.kill();fixture.wait(timeout=5)
        try:command(client_home,'stop-agent')
        except Exception:pass
        if host is not None:
            host.terminate()
            try:host.wait(timeout=10)
            except subprocess.TimeoutExpired:host.kill();host.wait(timeout=5)
        log.close()
        (OUT/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf-8")
print(json.dumps(report,ensure_ascii=False,indent=2))
sys.exit(exit_code)
