#!/usr/bin/env python3
"""Exercise the ARM live entry with a loopback-only gateway and isolated state."""
import json, pathlib, socket, sqlite3, subprocess, sys, tempfile, threading, time, urllib.request, urllib.error
release = pathlib.Path(sys.argv[1]).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='live-smoke-', dir=sys.argv[2]))
bus = socket.socket()
bus.bind(('127.0.0.1', 0))
bus.listen(2)
bus.settimeout(10)
with socket.socket() as reservation:
    reservation.bind(('127.0.0.1', 0))
    port = reservation.getsockname()[1]
document = json.loads((release / 'config/simulation-home.json').read_text())
document['gateways'][0]['endpoint'] = f'127.0.0.1:{bus.getsockname()[1]}'
document['gateways'][0]['modules'][0]['verification'] = {'status': 'verified', 'evidence': 'isolated loopback fixture only; not field evidence', 'verified_at_unix_ms': 1}
config = root / 'fixture.json'
config.write_text(json.dumps(document))
errors = []
queries = []
def gateway():
    try:
        for _ in range(2):
            with bus.accept()[0] as connection:
                connection.settimeout(5)
                frame = b''
                while len(frame) < 5:
                    chunk = connection.recv(5-len(frame))
                    assert chunk, 'EOF before query'
                    frame += chunk
                assert frame == bytes.fromhex('AE15A1F2EE'), frame.hex()
                queries.append(frame.hex())
                connection.sendall(bytes.fromhex('AE15D12AEE'))
                assert connection.recv(5) == b'', 'unexpected control on unpaired service'
    except Exception as error:
        errors.append(repr(error))
thread = threading.Thread(target=gateway, daemon=True)
thread.start()
previous = None
for round_number in range(2):
    with (root / f'run-{round_number}.log').open('wb') as log:
        process = subprocess.Popen([str(release/'homesensed'), 'serve-live', str(config), str(root/'state'), f'127.0.0.1:{port}', '1', '--coexistence-confirmed'], stdout=log, stderr=log)
        try:
            status = None
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and process.poll() is None:
                try:
                    urllib.request.build_opener(urllib.request.ProxyHandler({})).open(f'http://127.0.0.1:{port}/accessories', timeout=.2)
                    raise AssertionError('unauthenticated request succeeded')
                except urllib.error.HTTPError as error:
                    status = error.code
                    break
                except urllib.error.URLError:
                    time.sleep(.02)
            assert status == 401, (status, log.name)
            assert len(queries) == round_number + 1
            assert process.wait(timeout=5) == 0, log.name
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
    with sqlite3.connect(root/'state/identity/identity.sqlite') as db:
        identity = db.execute('SELECT key,value FROM entries ORDER BY key').fetchall()
    if previous is not None:
        assert previous == identity, 'identity changed across restart'
    previous = identity
thread.join(timeout=5)
bus.close()
assert not thread.is_alive() and not errors, errors
print(json.dumps({'live_smoke': 'passed', 'restarts': 2, 'query_before_hap': True, 'unpaired_http': 401, 'identity_stable': True, 'state_directory': str(root)}))
