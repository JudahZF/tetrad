#!/usr/bin/env python3
"""SSH ProxyCommand for the Quad Cortex.

The QC's OpenSSH 6.7p1 drops connections when a modern client sends its
identification banner in one write. This proxy sends the client's first line
one byte per TCP segment, then relays both directions unchanged.

Part of Tetrad: scripts/qc-ssh-proxy.py. `scripts/install.sh --proxy` uses it
(docs/install.md, "SSH problems"). For plain ssh, in ~/.ssh/config:
    Host qc
        HostName <unit-ip>
        Port <port>
        User root
        IdentityFile ~/.ssh/<your-key>
        ProxyCommand python3 /path/to/tetrad/scripts/qc-ssh-proxy.py %h %p
"""

import os
import select
import socket
import sys
import time


def main() -> None:
    host, port = sys.argv[1], int(sys.argv[2])
    sock = socket.create_connection((host, port), timeout=10)
    sock.settimeout(None)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

    stdin, stdout = sys.stdin.fileno(), sys.stdout.fileno()
    banner_sent = False
    pending = b""

    while True:
        readable, _, _ = select.select([sock, stdin], [], [])
        if sock in readable:
            data = sock.recv(65536)
            if not data:
                return
            os.write(stdout, data)
        if stdin in readable:
            data = os.read(stdin, 65536)
            if not data:
                sock.shutdown(socket.SHUT_WR)
                return
            if banner_sent:
                sock.sendall(data)
                continue
            pending += data
            if b"\n" not in pending:
                continue
            line, _, rest = pending.partition(b"\n")
            for byte in line + b"\n":
                sock.send(bytes([byte]))
                time.sleep(0.002)
            if rest:
                sock.sendall(rest)
            banner_sent = True


if __name__ == "__main__":
    main()
