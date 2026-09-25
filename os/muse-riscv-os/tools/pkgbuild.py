#!/usr/bin/env python3
"""v3.2 host-side package builder: guest crate dir -> installable package.

A "package" is a manifest + one ustar layer (the only shapes the guest
untar understands: ustar regular/dir entries). This tool:

  1. cross-builds a guest binary crate for riscv64gc-unknown-none-elf
     (the crate must already be guest-compatible: no_std + user-lib,
     with a build.rs pinning user-linker.ld -- see tools/pkgdemo/),
  2. packs `bin/<bin>` (+ optional extra files) into a ustar layer,
  3. writes the manifest (`name:`/`version:`/optional `depends:`/layer).

Usage:
  pkgbuild.py --crate DIR --pkg NAME --version VER --bin BIN --out DIR
              [--add arcpath=hostpath ...] [--depends "a b=1.0"]
  pkgbuild.py --src crates NAME VER --out DIR   # fetch-only check

`--src crates` downloads the .crate file from static.crates.io
(stdlib urllib, no login) and unpacks it; it proves the fetch path.
End-to-end crates.io->guest still awaits the first published guest
crate (which needs user-lib on crates.io first -- see _doc/v3.2.md).

Importable: build_package(...) returns (manifest_bytes, tar_bytes)
and optionally writes them to out_dir (used by img_registry.py to
serve /pkg/<name>/* without committing binary blobs).
"""

import io
import os
import subprocess
import sys
import tarfile

TARGET = "riscv64gc-unknown-none-elf"
CRATES_URL = "https://static.crates.io/crates/{name}/{name}-{version}.crate"


def run(cmd, cwd):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError("cmd %r failed in %s:\n%s" % (cmd, cwd, p.stderr[-2000:]))
    return p


def fetch_crate(name, version, dest_parent):
    """Download + unpack a .crate file; returns the crate source dir."""
    import urllib.request
    url = CRATES_URL.format(name=name, version=version)
    dest = os.path.join(dest_parent, "%s-%s" % (name, version))
    print("pkgbuild: fetching %s" % url, flush=True)
    with urllib.request.urlopen(url, timeout=60) as r:
        blob = r.read()
    if not blob:
        raise RuntimeError("empty download: %s" % url)
    with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as t:
        t.extractall(dest_parent)
    if not os.path.isdir(dest):
        raise RuntimeError("unpack did not produce %s" % dest)
    print("pkgbuild: unpacked %s (%dB)" % (dest, len(blob)), flush=True)
    return dest


def build_elf(crate_dir, binary):
    run(["cargo", "build", "--release", "--target", TARGET], cwd=crate_dir)
    elf = os.path.join(crate_dir, "target", TARGET, "release", binary)
    with open(elf, "rb") as f:
        data = f.read()
    if not data:
        raise RuntimeError("empty ELF at %s (build the crate first?)" % elf)
    return data


def build_layer(binary, elf_bytes, extra):
    """extra: list of (archive_path, host_path). Deterministic output."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as t:
        d = tarfile.TarInfo("bin")
        d.type = tarfile.DIRTYPE
        d.mode = 0o755
        d.mtime = 0
        t.addfile(d)
        seen_dirs = {"bin"}
        for arcpath, hostpath in extra:
            parts = arcpath.split("/")[:-1]
            for i in range(1, len(parts) + 1):
                dd = "/".join(parts[:i])
                if dd and dd not in seen_dirs:
                    di = tarfile.TarInfo(dd)
                    di.type = tarfile.DIRTYPE
                    di.mode = 0o755
                    di.mtime = 0
                    t.addfile(di)
                    seen_dirs.add(dd)
            with open(hostpath, "rb") as f:
                payload = f.read()
            fi = tarfile.TarInfo(arcpath)
            fi.size = len(payload)
            fi.mode = 0o644
            fi.mtime = 0
            t.addfile(fi, io.BytesIO(payload))
        e = tarfile.TarInfo("bin/" + binary)
        e.size = len(elf_bytes)
        e.mode = 0o755
        e.mtime = 0
        t.addfile(e, io.BytesIO(elf_bytes))
    return buf.getvalue()


def build_manifest(pkg, version, layer, depends):
    lines = ["# pkg %s %s (built by tools/pkgbuild.py)" % (pkg, version)]
    lines.append("name: %s" % pkg)
    lines.append("version: %s" % version)
    if depends:
        lines.append("depends: %s" % depends)
    lines.append(layer)
    return ("\n".join(lines) + "\n").encode()


def build_package(crate_dir, pkg, version, binary, out_dir=None,
                  extra=(), depends=""):
    elf = build_elf(crate_dir, binary)
    layer = "%s.tar" % pkg
    tar = build_layer(binary, elf, list(extra))
    manifest = build_manifest(pkg, version, layer, depends)
    if out_dir is not None:
        os.makedirs(out_dir, exist_ok=True)
        with open(os.path.join(out_dir, "manifest"), "wb") as f:
            f.write(manifest)
        with open(os.path.join(out_dir, layer), "wb") as f:
            f.write(tar)
        print("pkgbuild: wrote %s/ (manifest %dB + %s %dB)"
              % (out_dir, len(manifest), layer, len(tar)), flush=True)
    return manifest, tar


def main(argv):
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--crate", default=None)
    ap.add_argument("--src", nargs=3, metavar=("KIND", "NAME", "VER"),
                    default=None)
    ap.add_argument("--pkg", default=None)
    ap.add_argument("--version", default=None)
    ap.add_argument("--bin", default=None)
    ap.add_argument("--out", default=None)
    ap.add_argument("--add", action="append", default=[],
                    help="arcpath=hostpath extra file (repeatable)")
    ap.add_argument("--depends", default="")
    a = ap.parse_args(argv)
    crate_dir = a.crate
    if a.src is not None:
        kind, name, ver = a.src
        if kind != "crates":
            raise SystemExit("only --src crates NAME VER is supported")
        import tempfile
        tmp = tempfile.mkdtemp(prefix="pkgbuild-src-")
        crate_dir = fetch_crate(name, ver, tmp)
        if a.pkg is None:
            print("pkgbuild: fetched only (no --pkg given)")
            return 0
    if not (crate_dir and a.pkg and a.version and a.bin and a.out):
        raise SystemExit("need --crate/--pkg/--version/--bin/--out")
    extra = []
    for spec in a.add:
        arc, host = spec.split("=", 1)
        extra.append((arc, host))
    build_package(crate_dir, a.pkg, a.version, a.bin, a.out,
                  extra=extra, depends=a.depends)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
