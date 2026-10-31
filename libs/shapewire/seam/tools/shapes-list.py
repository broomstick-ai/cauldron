#!/usr/bin/env python3
"""Ask the live daemon for `shapes/list` and print the payload.

**READ-ONLY, AND STRUCTURALLY SO.** It sends exactly one method name,
`shapes/list`, which `shapewire::server::CHASSIS_METHODS` answers out of the
chassis itself. There is no command verb in this file, no subscribe, and no
write of any kind.

**Dependency-free on purpose.** `vendor-chassis.sh` runs this to build
`inventory.json`'s `shapes` section, and the vendor path must work on a machine
with nothing installed but python — a vendor step that needs `pip install` is a
vendor step somebody skips and then hand-edits `inventory.json`, which §7.4 says
is the one thing that must not happen. So the websocket handshake is written out
here (RFC 6455 is a GET, a base64 nonce and a masked text frame) rather than
imported.

    ./shapes-list.py [port]        # default 3653, the chassis door

Exits non-zero and says why if the daemon is not there, rather than printing an
empty list — an inventory with no shapes in it would make every rule 2.6 check
vacuously green.
"""

import base64
import json
import os
import socket
import struct
import sys


def handshake(sock, host, port):
    nonce = base64.b64encode(os.urandom(16)).decode()
    req = (
        f"GET /ws HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        f"Upgrade: websocket\r\n"
        f"Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {nonce}\r\n"
        f"Sec-WebSocket-Version: 13\r\n\r\n"
    )
    sock.sendall(req.encode())
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            sys.exit("the daemon closed the socket during the handshake")
        buf += chunk
    head, rest = buf.split(b"\r\n\r\n", 1)
    if b"101" not in head.split(b"\r\n")[0]:
        sys.exit(f"the daemon refused the upgrade: {head.splitlines()[0]!r}")
    return rest


def send_text(sock, text):
    payload = text.encode()
    mask = os.urandom(4)
    masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    n = len(payload)
    if n < 126:
        header = struct.pack("!BB", 0x81, 0x80 | n)
    elif n < 1 << 16:
        header = struct.pack("!BBH", 0x81, 0x80 | 126, n)
    else:
        header = struct.pack("!BBQ", 0x81, 0x80 | 127, n)
    sock.sendall(header + mask + masked)


class Frames:
    """Text frames off the wire. Continuations are joined; control frames are
    skipped. Enough for one request and its answer, and no more."""

    def __init__(self, sock, carry):
        self.sock = sock
        self.buf = carry

    def _need(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("the daemon closed the socket")
            self.buf += chunk

    def next_text(self):
        while True:
            self._need(2)
            b0, b1 = self.buf[0], self.buf[1]
            opcode = b0 & 0x0F
            length = b1 & 0x7F
            off = 2
            if length == 126:
                self._need(4)
                length = struct.unpack("!H", self.buf[2:4])[0]
                off = 4
            elif length == 127:
                self._need(10)
                length = struct.unpack("!Q", self.buf[2:10])[0]
                off = 10
            self._need(off + length)
            payload = self.buf[off:off + length]
            self.buf = self.buf[off + length:]
            if opcode == 0x8:
                raise EOFError("the daemon sent a close frame")
            if opcode in (0x1, 0x0):
                return payload.decode("utf-8", "replace")
            # ping/pong and anything else: not our business.


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 3653
    host = "127.0.0.1"
    try:
        sock = socket.create_connection((host, port), timeout=10)
    except OSError as e:
        sys.exit(f"no daemon on {host}:{port} ({e}). `shapes/list` is the one "
                 f"authority on what the producer serves and there is no "
                 f"honest fallback: a hand-written inventory makes rule 2.6's "
                 f"check agree with whoever wrote it.")
    carry = handshake(sock, host, port)
    frames = Frames(sock, carry)

    welcome = json.loads(frames.next_text())
    if welcome.get("type") != "welcome":
        sys.exit(f"first frame was not a welcome: {welcome}")
    if welcome.get("auth_required"):
        sys.exit("this daemon wants a secret; this script deliberately holds none")

    send_text(sock, json.dumps(
        {"type": "request", "id": "1", "method": "shapes/list", "params": {}}))
    for _ in range(64):
        v = json.loads(frames.next_text())
        if v.get("type") == "response" and v.get("correlate") == "1":
            outcome = v.get("outcome", {})
            if outcome.get("type") != "success":
                sys.exit(f"`shapes/list` refused: {outcome}")
            payload = outcome.get("payload", {})
            payload["_boot_id"] = welcome.get("boot_id")
            payload["_daemon_version"] = welcome.get("daemon_version") or welcome.get("version")
            print(json.dumps(payload, indent=2, sort_keys=True))
            return
    sys.exit("`shapes/list` never answered")


if __name__ == "__main__":
    main()
