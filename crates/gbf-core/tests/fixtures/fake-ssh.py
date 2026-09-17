#!/usr/bin/env python3
"""Local-only SSH process double: SOCKS domain relay + controlled exit modes."""
import os
from pathlib import Path
import socket
import struct
import sys
import threading
import time

root = Path(sys.argv[sys.argv.index("-i") + 1]).parent
mode = (root / "mode").read_text() if (root / "mode").exists() else "normal"
(root / "pid").write_text(str(os.getpid()))
if mode in ("auth", "host"):
    print("Permission denied (publickey)." if mode == "auth" else "Host key verification failed.", file=sys.stderr)
    sys.exit(255)
if mode == "slow":
    time.sleep(60)
    sys.exit(1)
host, port = sys.argv[sys.argv.index("-D") + 1].split(":")
listener = socket.socket()
listener.bind((host, int(port)))
listener.listen()
(root / "port").write_text(port)


def exact(s, n):
    data = b""
    while len(data) < n:
        part = s.recv(n - len(data))
        if not part:
            raise EOFError()
        data += part
    return data


def client(s):
    try:
        s.settimeout(3)
        version, n = exact(s, 2)
        assert version == 5
        exact(s, n)
        s.sendall(b"\x05\x00")
        version, cmd, _, kind = exact(s, 4)
        assert version == 5 and cmd == 1 and kind == 3
        domain = exact(s, exact(s, 1)[0]).decode()
        port = struct.unpack("!H", exact(s, 2))[0]
        (root / "destination").write_text(f"{domain}:{port}")
        s.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
        request = b""
        while b"\r\n\r\n" not in request:
            request += exact(s, 1)
        with (root / "requests").open("a") as log:
            log.write(request.split(b"\r\n", 1)[0].decode() + "\n")
        s.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    except (OSError, EOFError, AssertionError):
        pass
    finally:
        s.close()


while True:
    s, _ = listener.accept()
    threading.Thread(target=client, args=(s,), daemon=True).start()
