#!/usr/bin/env python3
"""v1.7 host-side HTTP server for the guest wget/curl tests.

Listens on 127.0.0.1:8090 (the guest reaches it as 10.0.2.2:8090 through
QEMU user-net, same mechanism as tools/udp_echo.py). Serves /test.txt with
fixed content (carrying the `muse-riscv-os` marker the guests assert);
everything else is 404. HTTP/1.0, closes after each reply (matches what
the guest stack implements). Started/stopped by test.sh around run1.
"""
import socket

ADDR = ("127.0.0.1", 8090)
BODY = b"muse-riscv-os guest-online test file\nsecond line here\n"


def handle(conn: socket.socket) -> None:
    try:
        req = b""
        conn.settimeout(5.0)
        while b"\r\n\r\n" not in req and len(req) < 2048:
            b = conn.recv(1024)
            if not b:
                break
            req += b
        line = req.split(b"\r\n")[0] if req else b""
        parts = line.split(b" ")
        if len(parts) >= 2 and parts[0] == b"GET" and parts[1] == b"/test.txt":
            body = BODY
            head = (
                b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n"
                b"Content-Length: " + str(len(body)).encode() + b"\r\n"
                b"Connection: close\r\n\r\n"
            )
            conn.sendall(head + body)
        else:
            conn.sendall(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n")
    except OSError:
        pass
    finally:
        try:
            conn.close()
        except OSError:
            pass


def main() -> None:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(ADDR)
    s.listen(8)
    print(f"http_server: listening on {ADDR[0]}:{ADDR[1]}", flush=True)
    while True:
        conn, _ = s.accept()
        handle(conn)


if __name__ == "__main__":
    main()
