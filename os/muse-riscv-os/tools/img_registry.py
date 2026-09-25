#!/usr/bin/env python3
"""v2.3 host-side image registry stub for the guest `ctr pull` test.

Listens on 127.0.0.1:8091 (the guest reaches it as 10.0.2.2:8091 through
QEMU user-net, same mechanism as tools/http_server.py). Serves:

  /testimg/manifest    plain-text layer list (`#` comments allowed)
  /testimg/layer1.tar  ustar layer with hello.txt + bin/echo

v3.0: package routes for the guest `ctr install` test:

  /pkg/hello/manifest  package manifest (name:/version: + layers)
  /pkg/hello/hello.tar ustar layer with bin/hello + hello.txt

v3.1: + farewell (depends: hello=1.0) and loopy (depends: loopy,
self-cycle negative), same echo-ELF shape, other names.

v3.2: + fortune, built at startup from tools/pkgdemo/ via
tools/pkgbuild.py (the crates.io-pipeline demo: a real out-of-tree
guest crate, never through the workspace build). A build failure
leaves /pkg/fortune/* unregistered (404) and loud on stderr --
the suite then fails visibly, never silently.

The layer is packed at startup with stdlib `tarfile` in USTAR_FORMAT
(the guest untar only understands ustar regular/dir entries). bin/echo
(resp. bin/hello, same bytes) is the *guest* echo ELF read from the
host build tree -- deliberate: the suite runs the downloaded binary
to prove it is executable.
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

# v3.0: first package. bin/hello is the guest echo ELF under a
# collision-free name (/bin has no `hello`, so install/remove prove
# the link/unlink path instead of shadowing). hello.txt carries a
# marker line proving the store payload landed.
PKG_HELLO_MANIFEST = b"# pkg hello 1.0\nname: hello\nversion: 1.0\nhello.tar\n"
PKG_HELLO_TXT = b"pkg-hello-txt\n"

# v3.1: dependency packages. farewell depends on hello=1.0 (same echo
# ELF bytes, other names); loopy depends on itself (cycle negative).
PKG_FAREWELL_MANIFEST = (
    b"# pkg farewell 1.0\nname: farewell\nversion: 1.0\n"
    b"depends: hello=1.0\nfarewell.tar\n"
)
PKG_FAREWELL_TXT = b"farewell-txt\n"
PKG_LOOPY_MANIFEST = (
    b"# pkg loopy 1.0\nname: loopy\nversion: 1.0\n"
    b"depends: loopy\nloopy.tar\n"
)
PKG_LOOPY_TXT = b"loopy-txt\n"


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


def build_pkg_hello(echo: bytes) -> bytes:
    return build_pkg_bin(echo, "bin/hello", "hello.txt", PKG_HELLO_TXT)


def build_pkg_bin(echo: bytes, binname: str, txtname: str, txt: bytes) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as t:
        d = tarfile.TarInfo("bin")
        d.type = tarfile.DIRTYPE
        d.mode = 0o755
        d.mtime = 0
        t.addfile(d)
        h = tarfile.TarInfo(txtname)
        h.size = len(txt)
        h.mode = 0o644
        h.mtime = 0
        t.addfile(h, io.BytesIO(txt))
        e = tarfile.TarInfo(binname)
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
    with open(ECHO_ELF, "rb") as f:
        echo = f.read()
    if not echo:
        raise RuntimeError("empty echo ELF at %s (build user apps first)"
                           % ECHO_ELF)
    ROUTES[b"/pkg/hello/manifest"] = PKG_HELLO_MANIFEST
    ROUTES[b"/pkg/hello/hello.tar"] = build_pkg_hello(echo)
    print("img_registry: pkg hello 1.0 = manifest (%dB) + hello.tar (%dB)"
          % (len(PKG_HELLO_MANIFEST),
             len(ROUTES[b"/pkg/hello/hello.tar"])), flush=True)
    ROUTES[b"/pkg/farewell/manifest"] = PKG_FAREWELL_MANIFEST
    ROUTES[b"/pkg/farewell/farewell.tar"] = build_pkg_bin(
        echo, "bin/farewell", "farewell.txt", PKG_FAREWELL_TXT)
    print("img_registry: pkg farewell 1.0 = manifest (%dB) + farewell.tar (%dB)"
          % (len(PKG_FAREWELL_MANIFEST),
             len(ROUTES[b"/pkg/farewell/farewell.tar"])), flush=True)
    ROUTES[b"/pkg/loopy/manifest"] = PKG_LOOPY_MANIFEST
    ROUTES[b"/pkg/loopy/loopy.tar"] = build_pkg_bin(
        echo, "bin/loopy", "loopy.txt", PKG_LOOPY_TXT)
    print("img_registry: pkg loopy 1.0 = manifest (%dB) + loopy.tar (%dB)"
          % (len(PKG_LOOPY_MANIFEST),
             len(ROUTES[b"/pkg/loopy/loopy.tar"])), flush=True)
    # v3.2: fortune via the real pipeline (cargo build of tools/pkgdemo).
    try:
        import pkgbuild
        demo_dir = os.path.join(ROOT, "tools", "pkgdemo")
        fmanifest, ftar = pkgbuild.build_package(
            demo_dir, "fortune", "1.0", "fortune")
        ROUTES[b"/pkg/fortune/manifest"] = fmanifest
        ROUTES[b"/pkg/fortune/fortune.tar"] = ftar
        print("img_registry: pkg fortune 1.0 = manifest (%dB) + fortune.tar (%dB) [pipeline]"
              % (len(fmanifest), len(ftar)), flush=True)
    except Exception as e:
        print("img_registry: FORTUNE BUILD FAILED (%r); /pkg/fortune/* will 404"
              % (e,), flush=True)
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
