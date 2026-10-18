#!/usr/bin/env python3
"""Pinned native HTTPS/WSS qualification on private loopback buses."""
from pathlib import Path
import argparse, copy, hashlib, http.client, json, os, re, socket, sqlite3, ssl, subprocess, threading, time

p = argparse.ArgumentParser()
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--sha256', required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--scratch', type=Path, required=True)
p.add_argument('--scenario', choices=['scene', 'motor', 'away', 'wall'], help='Run one explicitly selected development case; default runs all four.')
a = p.parse_args()
os.umask(0o077)
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert a.binary.is_absolute() and sha(a.binary) == a.sha256
a.output.mkdir(parents=True, exist_ok=False)
package = Path(__file__).resolve().parents[1]
repository = package.parents[1]
def save(path, value):
    temporary = path.with_suffix(path.suffix+'.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2)+'\n')
    temporary.replace(path)


class Fixture:
    def __init__(self, scenario):
        self.scenario = scenario
        self.root = a.output/scenario
        self.root.mkdir()
        source = repository/'config/test'/('native-app-keypad-scene-execute' if scenario in ['scene', 'wall'] else 'native-app-keypad-'+scenario)
        self.config = json.loads((source/'devices.template.json').read_text())
        self.automation = json.loads((source/'automation.json').read_text())
        self.frames, self.received, self.errors = [], [], []
        self.stop = threading.Event()
        self.socket = socket.socket(); self.socket.bind(('127.0.0.1', 0)); self.socket.listen(); self.socket.settimeout(.05)
        self.config['gateways'][0]['endpoint'] = f'127.0.0.1:{self.socket.getsockname()[1]}'
        self.values = {3: [scenario in ['scene', 'wall']]+[False]*7}
        if scenario == 'away':
            self.values[4] = [False]*8
            for number in range(3, 13):
                module = copy.deepcopy(self.config['gateways'][0]['modules'][0])
                module.update(id=f'relay-{number}', address=f'{number+2:02X}')
                self.config['gateways'][0]['modules'].append(module)
                self.values[number+2] = [False]*8
                ids = []
                for channel in range(1, 9):
                    device = f'relay-{number}:{channel}'; ids.append(device)
                    self.config['devices'].append({'id': device, 'name': 'Synthetic full-scope lamp', 'module_id': module['id'],
                        'channel': channel, 'enabled': True, 'catalog_visible': False})
                self.automation['away_off']['modules'].append({'module_id': module['id'], 'devices': ids,
                    'review_evidence': 'All eight synthetic relay outputs explicitly reviewed as lighting'})
            for key in self.config['keypads'][0]['keys'][:5]:
                key['local_indicator'] = {'kind': 'scene_selection', 'group': 'scene-selection', 'value': 1,
                    'selected_color': 'red', 'unselected_color': 'green', 'review_evidence': 'Synthetic reviewed source branch only'}
        if scenario in ['scene', 'wall']:
            module = copy.deepcopy(self.config['gateways'][0]['modules'][0]); module.update(id='other-panel-module', address='42')
            self.config['gateways'][0]['modules'].append(module)
            other = copy.deepcopy(self.config['keypads'][0]); other.update(id='other-panel', name='Synthetic linked panel', module_id=module['id'])
            for key in other['keys']:
                key.pop('action', None); key['led_endpoint'].update(id='other:'+str(key['number']), name='Synthetic linked LED')
            self.config['keypads'].append(other)
            scene = self.automation['immediate_scenes'][0]
            branch = scene['activate'][0]['action']
            for name, on in [('then_actions', False), ('else_actions', True)]:
                branch[name].append({'source': 'synthetic/linked/'+name,
                    'action': {'kind': 'command', 'device': 'other:1', 'command': {'kind': 'panel_led', 'on': on}}})
            self.config['devices'].append({'id': 'fan', 'name': 'Synthetic ventilation switch', 'module_id': 'switch-module', 'channel': 2, 'enabled': True})
            self.config['keypads'][0]['keys'][1]['action'] = {'kind': 'scene_execute', 'scene': 'fan-toggle', 'review_evidence': 'Synthetic fan proxy mapped through reviewed scene'}
            fan = copy.deepcopy(scene); fan['id'] = 'fan-toggle'
            action = fan['activate'][0]['action']; action['condition']['left']['device'] = 'fan'
            for name in ['then_actions', 'else_actions']:
                action[name] = [entry for entry in action[name] if entry['action']['kind'] != 'set']
                for entry in action[name]:
                    target = entry['action']['device']
                    entry['action']['device'] = {'light': 'fan', 'key:1': 'key:2', 'other:1': 'other:2'}[target]
            self.automation['immediate_scenes'].append(json.loads(json.dumps(fan).replace('conditional/', 'fan/')))
        if scenario == 'wall':
            # Owner admission requires all six keys explicitly mapped. These
            # generated scenes intentionally share one synthetic lamp while
            # each has its own exact originating and linked LED endpoints.
            for key in self.config['keypads'][0]['keys'][2:]:
                number = key['number']
                mapped = copy.deepcopy(scene); mapped['id'] = f'wall-key-{number}'
                branch = mapped['activate'][0]['action']
                for name in ['then_actions', 'else_actions']:
                    for entry in branch[name]:
                        action = entry['action']
                        if action.get('device') == 'key:1': action['device'] = f'key:{number}'
                        if action.get('device') == 'other:1': action['device'] = f'other:{number}'
                self.automation['immediate_scenes'].append(mapped)
                key['action'] = {'kind': 'scene_execute', 'scene': mapped['id'],
                    'review_evidence': 'Synthetic exact same-lamp action with per-key source and linked LED; never a physical installation'}
            self.config['keypads'][0]['event_control'] = {
                'owner': 'homesense', 'legacy_writer_isolated': True,
                'review_evidence': 'Private synthetic loopback bus; no legacy or physical gateway writer; exact conditional scene mapping reviewed'}
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0)); self.web = reservation.getsockname()[1]
        self.origin = f'https://localhost:{self.web}'
        self.token = 'synthetic-unified-keypad-fixture-token-only'
        (self.root/'token').write_text(self.token)
        self.cert, self.key = self.root/'cert.pem', self.root/'key.pem'
        with (self.root/'openssl.log').open('xb') as log:
            subprocess.run(['/usr/bin/openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=localhost',
                '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1', '-addext', 'extendedKeyUsage=serverAuth',
                '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign',
                '-keyout', str(self.key), '-out', str(self.cert)], stdout=log, stderr=subprocess.STDOUT, check=True)
        self.tls = ssl.create_default_context(cafile=str(self.cert))
        save(self.root/'devices.json', self.config); save(self.root/'automation.json', self.automation)
        save(self.root/'service.json', {'fixture_kind': 'synthetic-unified-keypad', 'scenario': scenario, 'origin': self.origin, 'binary_sha256': a.sha256})
        if scenario == 'away':
            save(self.root/'away-policy.json', {'review': 'Root accepted: do not delay twelve-module full-off for LED; source indication must not wait for ninety-six feedback channels.',
                'source_window_ms': 2000, 'postwrite_feedback': 'All twelve modules, including query responses, withheld until the native test releases them.',
                'motor_STOP_policy_separate': True, 'previous_strict_LED_before_full_off_failure_preserved': True})
        self.pending = []
        self.pending_away = set()
        self.away_proof_written = False
        self.write_number = 0
        self.injected = set()
        self.started = time.monotonic()
        self.process = None

    def flush(self):
        save(self.root/'gateway-frames.json', self.frames)
        save(self.root/'gateway-received.json', self.received)

    def inbound(self, connection):
        changed = False
        if self.scenario == 'away' and len(self.pending_away) == 12:
            with sqlite3.connect('file:'+str(self.root/'state/automation-tcp.sqlite')+'?mode=ro', uri=True) as db:
                modules = db.execute("SELECT command_id,status,updated_ms FROM command_journal WHERE device_id LIKE 'module-off-ticket:%' ORDER BY command_id").fetchall()
                cue = db.execute("SELECT command_id,status,updated_ms FROM command_journal WHERE device_id='key:6' ORDER BY command_id DESC LIMIT 1").fetchone()
            if len(modules) == 12 and all(row[1] == 'written' for row in modules) and cue and cue[1] == 'written':
                if not self.away_proof_written:
                    save(self.root/'away-written-proof.json', {'module_tickets': modules, 'source_LED_ticket': cue,
                        'sampled_at_ms': (time.monotonic()-self.started)*1000, 'feedback_released': False,
                        'policy': 'Away full-off is not delayed for LED; both source LED and all module writes must finish within the source window, without postwrite feedback.'})
                    self.away_proof_written = True
                if (self.root/'release-away-feedback').exists():
                    for address in sorted(self.pending_away):
                        self.values[address] = [False]*8
                        raw = b''.join(bytes([0xae, address, 0xd0+n, 0xf2, 0xee]) for n in range(1, 9))
                        connection.sendall(raw)
                        self.received.append({'kind': 'released_away_feedback', 'address': f'{address:02X}', 'channels': 8,
                            'raw': raw.hex(), 'at_ms': (time.monotonic()-self.started)*1000, 'journal_written_count': 12})
                    self.pending_away.clear(); changed = True
        for number in range(1, 6):
            path = self.root/f'inject-{number}'
            if path.exists() and number not in self.injected:
                data = bytes.fromhex(path.read_text()); assert data in [bytes.fromhex('ae41d1f1ee'), bytes.fromhex('ae41d1f2ee')]
                connection.sendall(data); self.injected.add(number)
                changed = True
                self.received.append({'kind': 'physical_button', 'raw': data.hex(), 'at_ms': (time.monotonic()-self.started)*1000})
        remaining = []
        for number, address, channel, value in self.pending:
            if (self.root/f'release-feedback-{number}').exists():
                device = 'light' if channel == 1 else 'fan'
                with sqlite3.connect('file:'+str(self.root/'state/automation-tcp.sqlite')+'?mode=ro', uri=True) as db:
                    witness = db.execute('SELECT command_id,status FROM command_journal WHERE device_id=? ORDER BY command_id DESC LIMIT 1', (device,)).fetchone()
                if witness is None or witness[1] != 'written':
                    remaining.append((number, address, channel, value)); continue
                self.values[address][channel-1] = value == 0xf1
                raw = bytes([0xae, address, 0xd0+channel, value, 0xee]); connection.sendall(raw)
                self.received.append({'kind': 'released_feedback', 'write': number, 'raw': raw.hex(), 'at_ms': (time.monotonic()-self.started)*1000,
                    'journal_command_id': witness[0], 'journal_status': witness[1]})
                changed = True
            else: remaining.append((number, address, channel, value))
        if changed: self.flush()
        self.pending = remaining

    def gateway(self):
        try:
            while not self.stop.is_set():
                try: connection, peer = self.socket.accept()
                except socket.timeout: continue
                assert peer[0] == '127.0.0.1'
                with connection:
                    connection.settimeout(.02); buffer = b''
                    while not self.stop.is_set():
                        self.inbound(connection)
                        try: data = connection.recv(4096)
                        except socket.timeout: continue
                        if not data: break
                        buffer += data
                        while len(buffer) >= 5:
                            raw, buffer = buffer[:5], buffer[5:]
                            assert raw[0] == 0xae and raw[4] == 0xee, raw.hex()
                            address, command, value = raw[1:4]
                            item = {'raw': raw.hex(), 'at_ms': (time.monotonic()-self.started)*1000}
                            self.frames.append(item)
                            if address in self.values:
                                if 0xa1 <= command <= 0xa8:
                                    assert value == 0xf2; item['kind'] = 'query'; channel = command-0xa0
                                    if address in self.pending_away: item['withheld_after_write'] = True
                                    else: connection.sendall(bytes([0xae, address, 0xd0+channel, 0xf1 if self.values[address][channel-1] else 0xf2, 0xee]))
                                elif command == 0 and self.scenario == 'away':
                                    assert value == 0xf2; item['kind'] = 'module_off'; self.pending_away.add(address)
                                else:
                                    assert 1 <= command <= 8 and value in [0xf1, 0xf2], raw.hex()
                                    item['kind'] = 'switch'; self.write_number += 1; item['write'] = self.write_number
                                    if self.scenario in ['scene', 'wall']: self.pending.append((self.write_number, address, command, value))
                                    else:
                                        self.values[address][command-1] = value == 0xf1
                                        connection.sendall(bytes([0xae, address, 0xd0+command, value, 0xee]))
                            elif address == 0x22 and self.scenario == 'motor':
                                assert command == 1 and value in [0xf0, 0xf1, 0xf2]; item['kind'] = 'motor'
                            elif address in [0x41, 0x42]:
                                assert 1 <= command <= 6 and value in [0xf0, 0xf1, 0xf2]; item['kind'] = 'LED'
                            else: raise AssertionError('Unexpected frame '+raw.hex())
                            self.flush()
        except (ConnectionResetError, BrokenPipeError): pass
        except BaseException as error: self.errors.append(repr(error))
        finally: self.socket.close(); self.flush()

    def get(self, path):
        c = http.client.HTTPSConnection('localhost', self.web, context=self.tls, timeout=4)
        try:
            c.request('GET', path, headers={'Origin': self.origin, 'Authorization': 'Bearer '+self.token})
            r = c.getresponse(); return {'status': r.status, 'body': json.loads(r.read())}
        finally: c.close()

    def run(self, method):
        self.worker = threading.Thread(target=self.gateway, daemon=True); self.worker.start()
        command = [str(a.binary), 'serve-automation-tcp', str(self.root/'devices.json'), str(self.root/'automation.json'), str(self.root/'state'), 'forever',
            '--coexistence-confirmed', '--recoveries', '1', '--write-interval-ms', '20', '--web-listen', f'127.0.0.1:{self.web}',
            '--web-origin', self.origin, '--web-token', str(self.root/'token'), '--web-cert', str(self.cert), '--web-key', str(self.key), '--web-role', 'administrator']
        self.log = (self.root/'server.log').open('xb')
        self.process = subprocess.Popen(command, stdout=self.log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic()+14
            while True:
                assert self.process.poll() is None, 'Fixture CLI exited'
                try:
                    ready = self.get('/api/v1/modules')
                    if ready['status'] == 200 and ready['body'].get('ready'): break
                except (OSError, ssl.SSLError, http.client.HTTPException): pass
                if time.monotonic() >= deadline: raise AssertionError('Fixture readiness timeout')
                time.sleep(.05)
            save(self.root/'initial-ready.json', ready)
            environment = os.environ.copy(); environment['HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE'] = str(self.root.resolve())
            test = ['swift', 'test', '--build-system', 'native', '--package-path', str(package), '--scratch-path', str(a.scratch), '--filter', 'HomeUnifiedKeypadActualIntegrationTests/'+method]
            save(self.root/'command.json', {'CLI': command, 'test': test, 'fixture_environment': {'HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE': str(self.root.resolve())}})
            with (self.root/'swift-test.log').open('xb') as log:
                result = subprocess.run(test, env=environment, stdout=log, stderr=subprocess.STDOUT, timeout=90)
            for name, path in [('final-devices.json', '/api/v1/devices'), ('final-modules.json', '/api/v1/modules')]:
                save(self.root/name, self.get(path))
            with sqlite3.connect('file:'+str(self.root/'state/automation-tcp.sqlite')+'?mode=ro', uri=True) as db:
                db.row_factory = sqlite3.Row
                journal = [dict(row) for row in db.execute('SELECT boot_id,command_id,device_id,hex(intent) AS intent,status,updated_ms FROM command_journal ORDER BY command_id')]
                save(self.root/'journal.json', journal)
            text = (self.root/'swift-test.log').read_text()
            assert result.returncode == 0 and re.search(r'Test run with 1 test in 1 suite passed', text), 'Native actual failed; preserved raw evidence'
            assert not re.search(r'\b(skipped|disabled|ignored)\b', text)
            assert not self.errors, self.errors
            if self.scenario == 'wall':
                lighting = [row for row in journal if row['device_id'] == 'light']
                assert len(lighting) == 1 and lighting[0]['status'] == 'written', lighting
                assert json.loads((self.root/'native-posts.json').read_text()) == []
            if self.scenario == 'motor':
                motor = [row for row in journal if row['device_id'] == 'curtain']
                assert len(motor) == 5 and all(row['status'] == 'written' for row in motor), motor
            if self.scenario == 'away':
                modules = [row for row in journal if row['device_id'].startswith('module-off-ticket:')]
                assert len(modules) == 12 and all(row['status'] in ['written', 'observed'] for row in modules), modules
                outputs = [row['raw'] for row in self.frames if row.get('kind') == 'module_off']
                assert sorted(outputs) == [f'ae{address:02x}00f2ee' for address in range(3, 15)], outputs
            return {'scenario': self.scenario, 'passed': True, 'frames': len(self.frames), 'log_sha256': sha(self.root/'swift-test.log')}
        finally:
            if self.process.poll() is None:
                self.process.terminate()
                try: self.process.wait(timeout=8)
                except subprocess.TimeoutExpired: self.process.kill(); self.process.wait()
            self.stop.set(); self.worker.join(timeout=2); self.log.close()
            save(self.root/'gateway-errors.json', self.errors)


result = {'passed': False, 'binary_sha256': a.sha256, 'all_endpoints_loopback': True, 'production_or_phone_operations': 0,
          'selected_scenarios': [a.scenario] if a.scenario else ['scene', 'motor', 'away', 'wall'], 'cases': []}
try:
    for scenario, method in [('scene', 'nativeUnifiedSceneFanSourceOrderRetirementAndPassiveWSS'),
                             ('motor', 'nativeUnifiedMotorRepeatStopAndExplicitStopSurviveHeldHTTP'),
                             ('away', 'nativeUnifiedSelectionAndAwayKeepCompleteScope'),
                             ('wall', 'nativeOwnedWallGestureUpdatesCueWithoutClientPOST')]:
        if a.scenario and scenario != a.scenario: continue
        result['cases'].append(Fixture(scenario).run(method))
    result['passed'] = True
finally:
    result['binary_unchanged'] = sha(a.binary) == a.sha256
    save(a.output/'result.json', result)
    print(json.dumps(result), flush=True)
