#!/usr/bin/env python3
"""v2.3 host-side image registry stub for the guest `ctr pull` test.

Listens on 127.0.0.1:8091 (the guest reaches it as 10.0.2.2:8091 through
QEMU user-net, same mechanism as tools/http_server.py). Serves:

  /testimg/manifest    plain-text layer list (`#` comments allowed)
  /testimg/layer1.tar  ustar layer with hello.txt + bin/echo

The layer is packed at startup with stdlib `tarfile` in USTAR_FORMAT
(the guest untar only understands ustar regular/dir entries). bin/echo
is the *guest* echo ELF read from the host build tree -- deliberate:
the suite runs the downloaded binary to prove it is executable.
Everything else is 404. HTTP/1.0, closes after each reply (matches what
the guest stack implements). Started/stopped by test.sh around run1.
"""
import io
import os
import socket
import tarfile

ADDR = ("127.0.0.1", 8091)
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)  # os/muse-riscv-os/
ECHO_ELF = os.path.join(ROOT, "target", "riscv64gc-unknown-none-elf",
                        "release", "echo")

MANIFEST = b"# test image: one layer\nlayer1.tar\n"
HELLO = b"hello from image layer1\n"


def build_layer1() -> bytes:
    with open(ECHO_ELF, "rb") as f:
        echo = f.read()
    if not echo:
        raise RuntimeError("empty echo ELF at %s (build user apps first)"
                           % ECHO_ELF)
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as t:
        d = tarfile.TarInfo("bin")
        d.type = tarfile.DIRTYPE
        d.mode = 0o755
        d.mtime = 0
        t.addfile(d)
        h = tarfile.TarInfo("hello.txt")
        h.size = len(HELLO)
        h.mode = 0o644
        h.mtime = 0
        t.addfile(h, io.BytesIO(HELLO))
        e = tarfile.TarInfo("bin/echo")
        e.size = len(echo)
        e.mode = 0o755
        e.mtime = 0
        t.addfile(e, io.BytesIO(echo))
    return buf.getvalue()


ROUTES = {}


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
        body = None
        if len(parts) >= 2 and parts[0] == b"GET":
            body = ROUTES.get(parts[1])
        if body is None:
            conn.sendall(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n")
        else:
            head = (
                b"HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\n"
                b"Content-Length: " + str(len(body)).encode() + b"\r\n"
                b"Connection: close\r\n\r\n"
            )
            conn.sendall(head + body)
    except OSError:
        pass
    finally:
        try:
            conn.close()
        except OSError:
            pass


def main() -> None:
    ROUTES[b"/testimg/manifest"] = MANIFEST
    ROUTES[b"/testimg/layer1.tar"] = build_layer1()
    print("img_registry: testimg = manifest (%dB) + layer1.tar (%dB)"
          % (len(MANIFEST), len(ROUTES[b"/testimg/layer1.tar"])), flush=True)
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(ADDR)
    s.listen(8)
    print("img_registry: listening on %s:%d" % ADDR, flush=True)
    while True:
        conn, _ = s.accept()
        handle(conn)


if __name__ == "__main__":
    main()
