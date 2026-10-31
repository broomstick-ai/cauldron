#!/usr/bin/env python3
"""Read the live threads daemon's own `boot_id` off its chassis socket.

READ-ONLY AND STRUCTURALLY SO: it opens a websocket, reads the one `welcome`
frame the chassis sends unprompted, prints the boot id, and closes. It sends NO
method, no command verb, no subscribe -- there is nothing in this file that
could write to the daemon or its store.

Why the boot id and not just the pid: a pid can in principle be reused, and a
restart under the same supervisor could in principle land on it. The boot id is
minted fresh per boot by the daemon itself, so it is the field that betrays a
restart even when nothing else would.
"""
import socket, base64, os, struct, json, sys

HOST, PORT = "127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 3653
key = base64.b64encode(os.urandom(16)).decode()
s = socket.create_connection((HOST, PORT), timeout=6)
s.sendall((
    "GET /ws HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\n"
    "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
    "Sec-WebSocket-Version: 13\r\n\r\n" % (HOST, PORT, key)
).encode())
buf = b""
while b"\r\n\r\n" not in buf:
    buf += s.recv(4096)
head, rest = buf.split(b"\r\n\r\n", 1)
if b"101" not in head.split(b"\r\n")[0]:
    sys.exit(1)
while len(rest) < 2:
    rest += s.recv(65536)
n, off = rest[1] & 0x7F, 2
if n == 126:
    while len(rest) < 4:
        rest += s.recv(65536)
    n, off = struct.unpack(">H", rest[2:4])[0], 4
elif n == 127:
    while len(rest) < 10:
        rest += s.recv(65536)
    n, off = struct.unpack(">Q", rest[2:10])[0], 10
while len(rest) < off + n:
    rest += s.recv(65536)
s.close()
print(json.loads(rest[off:off + n]).get("boot_id", "UNREAD"))
