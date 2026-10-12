#!/usr/bin/env python3
"""Concurrent real HTTP cookie sessions against synthetic devices; no field access."""
import concurrent.futures
import http.client
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time

repo = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='homesense-sessions-') as directory:
    root = pathlib.Path(directory)
    secret = secrets.token_urlsafe(32)
    token = root / 'token'
    token.write_text(secret)
    token.chmod(0o600)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'

    def request(method, path, session=None, body=None, bearer=None):
        headers = {'Origin': origin, 'Content-Type': 'application/json'}
        if session:
            headers.update({'Cookie': session[0], 'X-CSRF-Token': session[1]})
        if bearer:
            headers['Authorization'] = 'Bearer ' + bearer
        connection = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
        try:
            connection.request(method, path, None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            raw = response.read()
            return response.status, dict(response.getheaders()), json.loads(raw) if raw else None
        finally:
            connection.close()

    def login(value):
        code, headers, body = request('POST', '/api/v1/session', bearer=value)
        assert code == 200, code
        return headers['set-cookie'].split(';')[0], body['csrf']

    command = [str(binary), 'serve-web-simulation', str(repo/'config/simulation.json'),
               str(repo/'config/automation.simulation.json'), str(root/'state'), '60',
               f'127.0.0.1:{port}', str(token), '--administrator']
    with (root/'server.log').open('w') as log:
        process = subprocess.Popen(command, stdout=log, stderr=log)
        try:
            for _ in range(250):
                assert process.poll() is None, 'server exited'
                try:
                    first = login(secret)
                    break
                except (ConnectionError, OSError):
                    time.sleep(.02)
            else:
                raise AssertionError('listener startup timeout')
            second = login(secret)
            assert first != second
            code, _, issued = request('POST', '/api/v1/credentials', first,
                {'role': 'Viewer', 'devices': ['simulation:light'], 'scenes': []})
            assert code == 201, code
            target = issued['id']
            old_member_session = login(issued['secret'])
            views = [request('GET', '/api/v1/credentials', session)[2] for session in (first, second)]
            expected = [next(item['grant'] for item in view['credentials'] if item['id'] == target) for view in views]
            assert expected[0] == expected[1]
            barrier = threading.Barrier(2)

            def update(index):
                barrier.wait(timeout=5)
                return request('PATCH', '/api/v1/credentials/' + target, (first, second)[index],
                    {'expected': expected[index], 'role': ('Member', 'Viewer')[index],
                     'devices': (['simulation:light'], [])[index], 'scenes': []})

            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(update, range(2)))
            assert sorted(result[0] for result in results) == [200, 409], results
            loser = next(result for result in results if result[0] == 409)
            assert loser[2]['error'] == 'credential_changed', loser
            assert request('GET', '/api/v1/devices', old_member_session)[0] == 401
            winner = next(i for i, result in enumerate(results) if result[0] == 200)
            current = request('GET', '/api/v1/credentials', first)[2]
            grant = next(item['grant'] for item in current['credentials'] if item['id'] == target)
            assert grant['role'] == ('Member', 'Viewer')[winner]
            assert grant['devices'] == (['simulation:light'], [])[winner]
            renewed = login(issued['secret'])
            code, _, snapshot = request('GET', '/api/v1/devices', renewed)
            assert code == 200
            assert [device['id'] for device in snapshot['devices']] == grant['devices']
            assert request('DELETE', '/api/v1/session', first)[0] == 200
            assert request('GET', '/api/v1/credentials', first)[0] == 401
            assert request('GET', '/api/v1/credentials', second)[0] == 200
            print(json.dumps({'concurrent_cookie_updates': 'one committed, one stale rejected',
                'old_target_session': 'revoked', 'renewed_target_session': 'accepted',
                'logout_isolation': 'passed', 'devices': 'synthetic loopback only'}))
        finally:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
