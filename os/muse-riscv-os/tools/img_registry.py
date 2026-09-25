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
PKG_HELLO_MANIFEST_1 = b"# pkg hello 1.0\nname: hello\nversion: 1.0\nhello.tar\n"
PKG_HELLO_TXT_1 = b"pkg-hello-txt\n"
# v3.3: hello 2.0 (same binary, new payload line -- the upgrade target).
PKG_HELLO_MANIFEST_2 = b"# pkg hello 2.0\nname: hello\nversion: 2.0\nhello.tar\n"
PKG_HELLO_TXT_2 = b"pkg-hello-txt-v2\n"
# v3.3: version index (one version per line); unpinned installs and
# upgrades resolve through it. Keep sorted ascending (guest takes max).
PKG_HELLO_INDEX = b"1.0\n2.0\n"
PKG_SINGLE_INDEX = b"1.0\n"

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

# v3.4: private package (401 without the bearer token) + selectively
# corrupted package (manifest hash is honest, tar has a flipped bit).
PKG_SECRET_MANIFEST = b"# pkg secret 1.0\nname: secret\nversion: 1.0\nsecret.tar\n"
PKG_SECRET_TXT = b"secret-txt\n"
PKG_TAMPERED_MANIFEST = (
    b"# pkg tampered 1.0\nname: tampered\nversion: 1.0\ntampered.tar\n"
)
PKG_TAMPERED_TXT = b"tampered-txt\n"


def with_sha(manifest: bytes, tars: list) -> bytes:
    """Append the v3.4 `sha256:` line (hash over concatenated layers)."""
    import hashlib
    h = hashlib.sha256()
    for t in tars:
        h.update(t)
    return manifest + b"sha256: " + h.hexdigest().encode() + b"\n"


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


def build_pkg_hello_1(echo: bytes) -> bytes:
    return build_pkg_bin(echo, "bin/hello", "hello.txt", PKG_HELLO_TXT_1)


def build_pkg_hello_2(echo: bytes) -> bytes:
    return build_pkg_bin(echo, "bin/hello", "hello.txt", PKG_HELLO_TXT_2)


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
        head, _, _ = req.partition(b"\r\n\r\n")
        lines = head.split(b"\r\n")
        parts = lines[0].split(b" ") if lines else []
        body = None
        if len(parts) >= 2 and parts[0] == b"GET":
            path = parts[1]
            # v3.4: private routes need `Authorization: Bearer test-token`.
            if path.startswith(b"/pkg/secret/"):
                auth = b""
                for ln in lines[1:]:
                    k, s, v = ln.partition(b":")
                    if s and k.strip().lower() == b"authorization":
                        auth = v.strip()
                        break
                if auth.lower() != b"bearer test-token":
                    conn.sendall(b"HTTP/1.0 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                    return
            body = ROUTES.get(path)
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
    # v3.4: every served manifest carries its sha256 line (integrity).
    h1 = build_pkg_hello_1(echo)
    h2 = build_pkg_hello_2(echo)
    ROUTES[b"/pkg/hello/1.0/manifest"] = with_sha(PKG_HELLO_MANIFEST_1, [h1])
    ROUTES[b"/pkg/hello/1.0/hello.tar"] = h1
    ROUTES[b"/pkg/hello/2.0/manifest"] = with_sha(PKG_HELLO_MANIFEST_2, [h2])
    ROUTES[b"/pkg/hello/2.0/hello.tar"] = h2
    ROUTES[b"/pkg/hello/index"] = PKG_HELLO_INDEX
    print("img_registry: pkg hello 1.0+2.0 versioned + index + sha", flush=True)
    # v3.3: every package is versioned (+ index); the legacy unversioned
    # /pkg/<name>/* routes are gone (guest resolves via index or pins).
    fw = build_pkg_bin(echo, "bin/farewell", "farewell.txt", PKG_FAREWELL_TXT)
    ROUTES[b"/pkg/farewell/1.0/manifest"] = with_sha(PKG_FAREWELL_MANIFEST, [fw])
    ROUTES[b"/pkg/farewell/1.0/farewell.tar"] = fw
    ROUTES[b"/pkg/farewell/index"] = PKG_SINGLE_INDEX
    print("img_registry: pkg farewell 1.0 versioned + index + sha", flush=True)
    lp = build_pkg_bin(echo, "bin/loopy", "loopy.txt", PKG_LOOPY_TXT)
    ROUTES[b"/pkg/loopy/1.0/manifest"] = with_sha(PKG_LOOPY_MANIFEST, [lp])
    ROUTES[b"/pkg/loopy/1.0/loopy.tar"] = lp
    ROUTES[b"/pkg/loopy/index"] = PKG_SINGLE_INDEX
    print("img_registry: pkg loopy 1.0 versioned + index + sha", flush=True)
    # v3.4: secret (401 without bearer) + tampered (honest manifest,
    # flipped tar byte).
    sc = build_pkg_bin(echo, "bin/secret", "secret.txt", PKG_SECRET_TXT)
    ROUTES[b"/pkg/secret/1.0/manifest"] = with_sha(PKG_SECRET_MANIFEST, [sc])
    ROUTES[b"/pkg/secret/1.0/secret.tar"] = sc
    ROUTES[b"/pkg/secret/index"] = PKG_SINGLE_INDEX
    print("img_registry: pkg secret 1.0 versioned + index + sha [private]", flush=True)
    tm = bytearray(build_pkg_bin(echo, "bin/tampered", "tampered.txt", PKG_TAMPERED_TXT))
    ROUTES[b"/pkg/tampered/1.0/manifest"] = with_sha(PKG_TAMPERED_MANIFEST, [bytes(tm)])
    # flip a payload byte (tampered.txt data block): headers stay valid
    # so the guest downloads fine and dies exactly on sha256 mismatch.
    tm[1025] ^= 1
    ROUTES[b"/pkg/tampered/1.0/tampered.tar"] = bytes(tm)
    ROUTES[b"/pkg/tampered/index"] = PKG_SINGLE_INDEX
    print("img_registry: pkg tampered 1.0 versioned + index + sha [corrupt tar]", flush=True)
    # v3.2: fortune via the real pipeline (cargo build of tools/pkgdemo).
    try:
        import pkgbuild
        demo_dir = os.path.join(ROOT, "tools", "pkgdemo")
        fmanifest, ftar = pkgbuild.build_package(
            demo_dir, "fortune", "1.0", "fortune")
        print("img_registry: pkg fortune 1.0 = manifest (%dB) + fortune.tar (%dB) [pipeline]"
              % (len(fmanifest), len(ftar)), flush=True)
    except Exception as e:
        print("img_registry: FORTUNE BUILD FAILED (%r); /pkg/fortune/* will 404"
              % (e,), flush=True)
        fmanifest, ftar = None, None
    if fmanifest is not None:
        # v3.3: fortune is versioned like the rest.
        ROUTES[b"/pkg/fortune/1.0/manifest"] = fmanifest
        ROUTES[b"/pkg/fortune/1.0/fortune.tar"] = ftar
        ROUTES[b"/pkg/fortune/index"] = PKG_SINGLE_INDEX
        print("img_registry: pkg fortune 1.0 versioned + index", flush=True)
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
