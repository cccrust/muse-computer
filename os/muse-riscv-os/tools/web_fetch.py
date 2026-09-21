#!/usr/bin/env python3
"""v1.5 host-side HTTP client for the guest webserver test.

Connects to 127.0.0.1:8080 (QEMU user-net forwards it to the guest's
10.0.2.15:80 via hostfwd), sends `GET /README HTTP/1.0`, and asserts the
response carries HTTP/1.0 200 plus the known README marker. Retries the
connect for up to ~30s (SLIRP first-packet setup + guest boot are slow).
Exit 0 on success, nonzero otherwise (test.sh hooks `|| PASS=0`).
"""
import socket
import sys
import time

HOST = "127.0.0.1"
PORT = 8080
MARKER = b"muse-riscv-os"
DEADLINE = 30.0


def fetch_once() -> bytes | None:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(5.0)
    try:
        s.connect((HOST, PORT))
        s.sendall(b"GET /README HTTP/1.0\r\n\r\n")
        chunks = []
        while True:
            try:
                b = s.recv(4096)
            except socket.timeout:
                break
            if not b:
                break
            chunks.append(b)
            if len(chunks) > 32:
                break
        return b"".join(chunks)
    except (ConnectionRefusedError, OSError):
        return None
    finally:
        s.close()


def main() -> int:
    t0 = time.time()
    last = b""
    while time.time() - t0 < DEADLINE:
        r = fetch_once()
        if r:
            last = r
            head, _, body = r.partition(b"\r\n\r\n")
            status = head.split(b"\r\n")[0] if head else b""
            if b"200" in status and MARKER in body:
                print(f"web_fetch: OK ({len(r)} bytes)", flush=True)
                return 0
            print(f"web_fetch: bad reply head={status!r}", flush=True)
            return 1
        time.sleep(1.0)
    print(f"web_fetch: no reply in {DEADLINE}s (last {len(last)} bytes)", flush=True)
    return 1


if __name__ == "__main__":
    sys.exit(main())
