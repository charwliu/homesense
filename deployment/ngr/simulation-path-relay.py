"""Bounded test relay: run on each router to verify its VLAN path to the simulator."""
import socket, select, sys, time, threading
local = sys.argv[1]
assert local in ('192.168.3.1', '192.168.4.1')
def handle(client):
    upstream = socket.socket()
    try:
        upstream.settimeout(5)
        upstream.bind((local, 0))
        upstream.connect(('192.168.3.2', 51828))
        end = time.monotonic() + 30
        while time.monotonic() < end:
            ready, _, _ = select.select([client, upstream], [], [], 5)
            if not ready: return
            for source in ready:
                data = source.recv(65536)
                if not data: return
                (upstream if source is client else client).sendall(data)
    finally:
        client.close(); upstream.close()
with socket.socket() as listener:
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((local, 51928)); listener.listen(4); listener.settimeout(1)
    print('Test relay ready on', local, flush=True)
    end = time.monotonic() + 180
    while time.monotonic() < end:
        try: client, peer = listener.accept()
        except socket.timeout: continue
        if peer[0] not in ('192.168.3.2', '192.168.4.2'):
            client.close(); continue
        threading.Thread(target=handle, args=(client,), daemon=True).start()
