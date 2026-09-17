"""Opt-in real SSH server for native OpenSSH tests. Never forwards off loopback.

Requires asyncssh==2.21.1; keys are temporary and the server accepts no shell.
"""
import asyncio
import json
from pathlib import Path
import sys

import asyncssh

root = Path(sys.argv[1])


class Reply(asyncssh.SSHTCPSession):
    def connection_made(self, channel):
        self.channel = channel
        self.pending = b""

    def data_received(self, data, datatype):
        self.pending += data
        if b"\r\n\r\n" in self.pending:
            self.channel.write(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            self.channel.close()


class Gateway(asyncssh.SSHServer):
    def connection_requested(self, dest_host, dest_port, orig_host, orig_port):
        if dest_host == "game.granbluefantasy.jp" and dest_port == 80:
            return Reply()
        return False


async def main():
    host = asyncssh.generate_private_key("ssh-ed25519")
    server = await asyncssh.listen("127.0.0.1", 0, server_factory=Gateway,
                                  server_host_keys=[host],
                                  authorized_client_keys=str(root / "authorized.pub"))
    port = server.get_port()
    (root / "ssh" / "known_hosts").write_text(
        f"[127.0.0.1]:{port} " + host.export_public_key().decode(), encoding="utf-8")
    (root / "ready.json").write_text(json.dumps({"port": port}), encoding="utf-8")
    await asyncio.Future()


asyncio.run(main())
