#!/usr/bin/env python3
"""Run the deployed binary against an isolated simulation state directory."""
import json
import pathlib
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

release = pathlib.Path(sys.argv[1]).resolve()
state_root = pathlib.Path(sys.argv[2]).resolve()
state_root.mkdir(parents=True, exist_ok=True)
state = pathlib.Path(tempfile.mkdtemp(prefix="smoke-", dir=state_root))
binary = release / "homesensed"
subprocess.run([str(binary), "demo", "30"], check=True)
subprocess.run([str(binary), "check-config", str(release / "config/study-panel.draft.json")], check=True)
with socket.socket() as reservation:
    reservation.bind(("127.0.0.1", 0))
    port = reservation.getsockname()[1]
previous = None
for round_number in range(2):
    with (state / f"service-{round_number}.log").open("wb") as log:
        process = subprocess.Popen([str(binary), "serve-simulation", str(release / "config/simulation.json"), str(state), str(port), "2"], stdout=log, stderr=log)
        status = None
        try:
            deadline = time.monotonic() + 1.5
            while time.monotonic() < deadline and process.poll() is None:
                try:
                    urllib.request.build_opener(urllib.request.ProxyHandler({})).open(f"http://127.0.0.1:{port}/accessories", timeout=0.2)
                    raise AssertionError("unpaired read unexpectedly succeeded")
                except urllib.error.HTTPError as error:
                    status = error.code
                    break
                except urllib.error.URLError:
                    time.sleep(0.02)
            code = process.wait(timeout=5)
            assert code == 0, f"service exit {code}; inspect {log.name}"
            assert status == 401, f"expected unpaired 401, got {status}"
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
    with socket.socket() as released:
        released.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        released.bind(("127.0.0.1", port))
        released.listen(1)
    with sqlite3.connect(state / "identity/identity.sqlite") as db:
        identity = db.execute("SELECT key,value FROM entries WHERE key IN ('config','aids','graph_manifest') ORDER BY key").fetchall()
    if previous is not None:
        assert previous == identity, "identity or graph changed across restart"
    previous = identity
print(json.dumps({"simulation_smoke": "passed", "restarts": 2, "unpaired_http_status": 401, "identity_stable": True, "listener_released": True, "state_directory": str(state)}))
