#!/usr/bin/env python3
"""Pinned real TLS API and 16-port loopback gateway; never uses live endpoints."""
from pathlib import Path
import argparse, hashlib, http.client, json, os, re, socket, ssl, subprocess, threading, time

p = argparse.ArgumentParser()
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--sha256', required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--scratch', type=Path, required=True)
a = p.parse_args()
os.umask(0o077)
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert sha(a.binary) == a.sha256
a.output.mkdir(parents=True, exist_ok=False)
package = Path(__file__).resolve().parents[1]
def save(name, value):
    (a.output/name).write_text(json.dumps(value, ensure_ascii=False, indent=2)+'\n')
listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(.1)
web_socket = socket.socket(); web_socket.bind(('127.0.0.1', 0)); web = web_socket.getsockname()[1]; web_socket.close()
origin = f'https://localhost:{web}'
cert, key = a.output/'cert.pem', a.output/'key.pem'
(a.output/'tls.cnf').write_text('[req]\ndistinguished_name=dn\n[dn]\nCN=localhost\n[v3]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyEncipherment,keyCertSign\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n')
with (a.output/'openssl.log').open('xb') as log:
    subprocess.run(['/usr/bin/openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=localhost', '-config', str(a.output/'tls.cnf'), '-extensions', 'v3', '-keyout', str(key), '-out', str(cert)], stdout=log, stderr=subprocess.STDOUT, check=True)
token = 'synthetic-physical-module-fixture-token-only'
(a.output/'token').write_text(token); os.chmod(key, 0o600); os.chmod(a.output/'token', 0o600)
ctx = ssl.create_default_context(cafile=str(cert))
verification = {'status': 'verified', 'verified_at_unix_ms': 1, 'evidence': 'Synthetic module gateway fixture only'}
modules = [{'id': name, 'address': address, 'profile': profile, 'verification': verification} for name, address, profile in [
    ('relay', '03', 'r8i8_din_pro_v2_7_7'), ('dimmer', '15', 'ad8_din_rs485_v2_2_1'), ('curtain', '22', 'c4_din_pro_v2_7_7')]]
config = {'schema_version': 2, 'revision': 1, 'manual_degraded_enabled': False, 'floors': ['Test'], 'rooms': [{'id': 'lab', 'name': 'Synthetic room', 'floor': 'Test'}],
          'gateways': [{'id': 'bus', 'endpoint': f'127.0.0.1:{listener.getsockname()[1]}', 'modules': modules}],
          'devices': [{'id': name, 'name': name, 'room': 'lab', 'module_id': module, 'channel': 1, 'enabled': True} for name, module in [('relay-light', 'relay'), ('dimmer-light', 'dimmer')]]}
save('devices.json', config)
save('automation.json', {'schema_version': 1, 'configuration_revision': 1, 'variables': [], 'rules': [], 'immediate_scenes': [], 'timed_scenes': []})
save('service.json', {'fixture_kind': 'synthetic-physical-modules-loopback', 'origin': origin, 'binary_sha256': a.sha256})
frames, errors, stop = [], [], threading.Event()
started = time.monotonic()
values = {3: [True]+[False]*7, 0x15: [0]*8}
def gateway():
    try:
        while not stop.is_set():
            try: connection, peer = listener.accept()
            except socket.timeout: continue
            assert peer[0] == '127.0.0.1'
            with connection:
                connection.settimeout(.1); pending = b''
                while not stop.is_set():
                    try: data = connection.recv(4096)
                    except socket.timeout: continue
                    if not data: break
                    pending += data
                    while len(pending) >= 5:
                        frame, pending = pending[:5], pending[5:]
                        assert frame[0] == 0xae and frame[-1] == 0xee
                        address, command, value = frame[1:4]
                        record = {'raw': frame.hex(), 'at_ms': round((time.monotonic()-started)*1000, 3)}
                        frames.append(record)
                        assert address in values, 'Unexpected motor/parameter address '+frame.hex()
                        switch = address == 3
                        if 0xa1 <= command <= 0xa8 or (not switch and command == 0xa0):
                            assert value == 0xf2
                            channels = range(1, 9) if command == 0xa0 else [command-0xa0]
                            record['kind'] = 'query'
                            replies = []
                            for channel in channels:
                                actual = (0xf1 if values[address][channel-1] else 0xf2) if switch else values[address][channel-1]
                                replies.append(bytes([0xae, address, 0xd0+channel, actual, 0xee]))
                            connection.sendall(b''.join(replies))
                        else:
                            # AD8 arbitrary percentages use the documented Bx
                            # ramp command; channel 01 is only off/full-on.
                            assert command == (1 if switch else 0xb1), frame.hex()
                            if switch:
                                assert value in [0xf1, 0xf2]; values[address][0] = value == 0xf1
                            else:
                                assert 0 <= value <= 100; values[address][0] = value
                            record['kind'] = 'relay' if switch else 'dimmer'
                            connection.sendall(bytes([0xae, address, 0xd1, value, 0xee]))
    except (ConnectionResetError, BrokenPipeError): pass
    except BaseException as error: errors.append(repr(error))
    finally: listener.close()
def get(path):
    connection = http.client.HTTPSConnection('localhost', web, context=ctx, timeout=4)
    try:
        connection.request('GET', path, headers={'Origin': origin, 'Authorization': 'Bearer '+token})
        response = connection.getresponse()
        return {'status': response.status, 'body': json.loads(response.read())}
    finally: connection.close()
args = [str(a.binary), 'serve-automation-tcp', str(a.output/'devices.json'), str(a.output/'automation.json'), str(a.output/'state'), 'forever', '--coexistence-confirmed', '--recoveries', '1', '--write-interval-ms', '20', '--web-listen', f'127.0.0.1:{web}', '--web-origin', origin, '--web-token', str(a.output/'token'), '--web-cert', str(cert), '--web-key', str(key), '--web-role', 'administrator']
result = {'passed': False, 'binary_sha256_before': sha(a.binary), 'all_endpoints_loopback': True, 'phone_or_production_operations': 0}
process = None; worker = threading.Thread(target=gateway, daemon=True)
try:
    worker.start()
    with (a.output/'server.log').open('xb') as log:
        process = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic()+20
        while True:
            assert process.poll() is None, 'CLI exited before fixture readiness'
            assert not errors, errors
            try:
                ready = get('/api/v1/modules')
                if ready['status'] == 200 and ready['body'].get('ready'): break
            except (OSError, ssl.SSLError, http.client.HTTPException): pass
            if time.monotonic() >= deadline: raise AssertionError('Synthetic module gateway startup timeout')
            time.sleep(.05)
        save('initial-ready.json', ready)
        env = {k: v for k, v in os.environ.items() if not k.startswith('HOMESENSE_')}
        env['HOMESENSE_PHYSICAL_MODULE_ACTUAL_FIXTURE'] = str(a.output.resolve())
        command = ['swift', 'test', '--build-system', 'native', '--package-path', str(package), '--scratch-path', str(a.scratch), '--filter', 'HomePhysicalModuleActualIntegrationTests']
        save('command.json', {'CLI_argv': args, 'test_argv': command, 'fixture_environment': {'HOMESENSE_PHYSICAL_MODULE_ACTUAL_FIXTURE': env['HOMESENSE_PHYSICAL_MODULE_ACTUAL_FIXTURE']}})
        with (a.output/'swift-test.log').open('xb') as log:
            test = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=120)
        # Read-only postmortem runs on success and failure, before shutdown.
        for name, path in [('after-native-devices.json', '/api/v1/devices'), ('after-native-modules.json', '/api/v1/modules')]:
            try: save(name, get(path))
            except Exception as error: save(name, {'read_error_type': type(error).__name__})
        assert test.returncode == 0, 'Native module test failed; original evidence retained'
        text = (a.output/'swift-test.log').read_text()
        assert re.search(r'Test run with 1 test in 1 suite passed', text) and not re.search(r'\b(skipped|disabled|ignored)\b', text)
        posts = json.loads((a.output/'native-module-posts.json').read_text())
        assert len(posts) == 3 and [post['path'] for post in posts] == ['/api/v1/module-queries', '/api/v1/module-commands', '/api/v1/module-commands']
        assert [frame['raw'] for frame in frames if frame['kind'] != 'query'] == ['ae0301f2ee', 'ae15b128ee']
        ramp = next(frame for frame in frames if frame['kind'] == 'dimmer')
        confirmations = []
        # Active feedback can satisfy the App before the scheduled query. Keep
        # observing the wire through the six-second fence instead of treating
        # that valid early push as a missing query.
        deadline = started + ramp['at_ms']/1000 + 8
        while time.monotonic() < deadline:
            assert process.poll() is None and not errors, errors
            confirmations = [frame for frame in frames if frame['raw'] == 'ae15a0f2ee' and frame['at_ms'] >= ramp['at_ms']]
            if confirmations: break
            time.sleep(.02)
        assert confirmations, 'AD8 final feedback must include a real post-ramp module query'
        query_delay = confirmations[0]['at_ms'] - ramp['at_ms']
        assert query_delay >= 6000, ('AD8 query before six-second ramp elapsed', query_delay)
        assert not errors, errors
        result.update(passed=True, executed_tests=1, executed_suites=1, native_POSTs=3, physical_writes=2, legal_query_frames=sum(frame['kind']=='query' for frame in frames), ordinary_feedback_budget_seconds=5, ad8_feedback_budget_seconds=8, ad8_query_delay_min_ms=6000, ad8_query_delay_observed_ms=round(query_delay,3))
finally:
    if process is not None and process.poll() is None:
        process.terminate()
        try: process.wait(timeout=8)
        except subprocess.TimeoutExpired: process.kill(); process.wait()
    stop.set(); worker.join(timeout=2)
    save('gateway-frames.json', frames); save('gateway-errors.json', errors)
    result['binary_sha256_after'] = sha(a.binary); result['binary_unchanged'] = result['binary_sha256_after'] == a.sha256
    save('result.json', result)
    print(json.dumps(result), flush=True)
