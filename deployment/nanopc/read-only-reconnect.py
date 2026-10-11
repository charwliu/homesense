#!/usr/bin/env python3
"""Field diagnostic: query ONLY study 03/4 and inject EOF on our own proxy.

This uses a fresh unpaired loopback HAP instance. It does not simulate a real
gateway outage, change the gateway, or disconnect any other client's socket.
"""
import json
import pathlib
import select
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

release = pathlib.Path(sys.argv[1]).resolve()
mode = sys.argv[3] if len(sys.argv) > 3 else "eof"
assert mode in ("eof", "query-timeout"), "mode must be eof or query-timeout"
generations = 3 if mode == "query-timeout" else 2
root = pathlib.Path(tempfile.mkdtemp(prefix="readonly-reconnect-", dir=sys.argv[2]))
document = json.loads((release / "config/study-switch.live.json").read_text())
assert len(document["gateways"]) == len(document["devices"]) == 1
assert document["gateways"][0]["endpoint"] == "192.168.0.10:50007"
device = document["devices"][0]
assert (device["address"], device["channel"], device["enabled"]) == ("03", 4, True)
query = bytes.fromhex("AE03A4F2EE")
valid_replies = (bytes.fromhex("AE03D4F1EE"), bytes.fromhex("AE03D4F2EE"))
proxy = socket.socket()
proxy.bind(("127.0.0.1", 0))
proxy.listen(2)
proxy.settimeout(15)
document["gateways"][0]["endpoint"] = "127.0.0.1:%d" % proxy.getsockname()[1]
config = root / "proxy-config.json"
config.write_text(json.dumps(document, ensure_ascii=False))
with socket.socket() as reservation:
    reservation.bind(("127.0.0.1", 0))
    hap_port = reservation.getsockname()[1]
events = []
errors = []
finished = threading.Event()


def record(kind, **fields):
    events.append(dict(time_ns=time.time_ns(), kind=kind, **fields))


def run_proxy():
    try:
        for generation in range(1, generations + 1):
            client, _ = proxy.accept()
            client.settimeout(2)
            with client, socket.create_connection(("192.168.0.10", 50007), 2) as upstream:
                record("connected", generation=generation)
                pending = b""
                observed = b""
                deadline = time.monotonic() + 14
                while not finished.is_set() and time.monotonic() < deadline:
                    readable, _, _ = select.select([client, upstream], [], [], .2)
                    if client in readable:
                        data = client.recv(512)
                        if not data:
                            assert not pending, "truncated query"
                            break
                        pending += data
                        while len(pending) >= 5:
                            frame, pending = pending[:5], pending[5:]
                            # Validate every outbound frame BEFORE forwarding.
                            assert frame == query, "non-query rejected: " + frame.hex()
                            upstream.sendall(frame)
                            record("query", generation=generation, hex=frame.hex())
                    if upstream in readable:
                        data = upstream.recv(512)
                        assert data, "real gateway disconnected unexpectedly"
                        record("feedback", generation=generation, hex=data.hex())
                        if mode == "query-timeout" and generation == 2:
                            record("suppressed_feedback", generation=generation)
                            continue
                        client.sendall(data)
                        observed = (observed + data)[-1024:]
                        if generation == 1 and any(reply in observed for reply in valid_replies):
                            # Let startup consume the feedback before injecting EOF.
                            time.sleep(.2)
                            record("injected_eof", generation=generation)
                            break
    except Exception as error:
        errors.append(repr(error))


thread = threading.Thread(target=run_proxy, daemon=True)
thread.start()
with (root / "service.log").open("wb") as log:
    process = subprocess.Popen([
        str(release / "homesensed"), "serve-live", str(config), str(root / "state"),
        f"127.0.0.1:{hap_port}", "10", "--coexistence-confirmed",
    ], stdout=log, stderr=log)
    try:
        code = process.wait(timeout=20)
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        finished.set()
        thread.join(timeout=3)
        proxy.close()
        (root / "bus-events.json").write_text(json.dumps(events, indent=2) + "\n")
assert not thread.is_alive() and not errors, errors
assert code == 0, root / "service.log"
assert sum(e["kind"] == "injected_eof" for e in events) == 1
assert sum(e["kind"] == "connected" for e in events) == generations
for generation in range(1, generations + 1):
    assert any(e["kind"] == "query" and e["generation"] == generation for e in events)
    received = b"".join(bytes.fromhex(e["hex"]) for e in events
                        if e["kind"] == "feedback" and e["generation"] == generation)
    assert any(reply in received for reply in valid_replies)
text = (root / "service.log").read_text()
assert "gateway_recovery_synchronized" in text
if mode == "query-timeout":
    assert any(e["kind"] == "suppressed_feedback" for e in events)
    assert "gateway_recovery_query_failed epoch=3" in text
    assert "gateway_recovery_synchronized epoch=3" not in text
    assert "gateway_recovery_synchronized epoch=4" in text
with sqlite3.connect(root / "state/runtime.sqlite") as db:
    count = db.execute("SELECT count(*) FROM command_journal").fetchone()[0]
assert count == 0
print(json.dumps(dict(result="passed", mode=mode, state_directory=str(root), connections=generations,
                      injected_eof=1, command_journal_rows=count,
                      limitation="proxy injected EOF; not a real gateway outage")))
