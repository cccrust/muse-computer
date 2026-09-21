#!/usr/bin/env python3
"""v1.3 UDP echo for the guest udpping test.

Listens on 127.0.0.1:7777 and reflects every datagram. QEMU user-net
(SLIRP) maps the guest's 10.0.2.2 to host loopback, so the guest's
sendto(10.0.0.2... 10.0.2.2:7777) lands here. Started/stopped by test.sh
around the run1 boot; also handy for manual `udpping` runs.
"""
import socket
import sys

ADDR = ("127.0.0.1", 7777)


def main() -> int:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(ADDR)
    print(f"udp_echo: listening on {ADDR[0]}:{ADDR[1]}", flush=True)
    while True:
        data, peer = s.recvfrom(2048)
        s.sendto(data, peer)


if __name__ == "__main__":
    sys.exit(main())
