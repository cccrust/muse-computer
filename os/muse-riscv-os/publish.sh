#!/bin/bash
# v3.8: publish helper for the crates.io guest libraries.
# Bumps (major|minor|patch, default patch) and `cargo publish`es each
# crate. The tree here is perpetually dirty (fs.img, qemu logs), so we
# publish with --allow-dirty -- the version bump itself is the payload.
#
#   tools/publish.sh [--dry-run] [major|minor|patch] [--all | <dir>...]
#
# Examples:
#   ./publish.sh                        # patch x.y.z+1, all three
#   ./publish.sh minor user-lib/guest-fmt
#   ./publish.sh --dry-run patch --all  # bump + package, no upload
# (run from os/muse-riscv-os/)
set -e
cd "$(dirname "$0")"
DRY=""
PART="patch"
DIRS=()
EXPLICIT=0
for a in "$@"; do
  case "$a" in
    --dry-run) DRY="--dry-run" ;;
    major|minor|patch) PART="$a" ;;
    # default: user-lib itself plus every publishable lib under it
    # (subdir with its own Cargo.toml); future libs are picked up
    # automatically. Explicit dirs override the default.
    --all) DIRS=(user-lib user-lib/*/); DIRS=("${DIRS[@]%/}") ;;
    *) DIRS+=("$a"); EXPLICIT=1 ;;
  esac
done
if [ ${#DIRS[@]} -eq 0 ]; then
  DIRS=(user-lib user-lib/*/)
  DIRS=("${DIRS[@]%/}")
fi
# keep only dirs that actually hold a crate (explicit typos still fail)
LIBS=()
for d in "${DIRS[@]}"; do
  if [ -f "$d/Cargo.toml" ]; then
    LIBS+=("$d")
  elif [ "$EXPLICIT" = "1" ]; then
    echo "publish.sh: no such crate dir: $d" >&2
    exit 1
  fi
done
if [ ${#LIBS[@]} -eq 0 ]; then
  echo "publish.sh: no crates found" >&2
  exit 1
fi

bump() {
  python3 - "$1" "$2" <<'EOF'
import re, sys
path, part = sys.argv[1], sys.argv[2]
p = path + "/Cargo.toml"
src = open(p).read()
m = re.search(r'(?m)^version = "(\d+)\.(\d+)\.(\d+)"', src)
if not m:
    sys.exit("no version line in %s" % p)
x, y, z = map(int, m.groups())
if part == "major":
    x, y, z = x + 1, 0, 0
elif part == "minor":
    y, z = y + 1, 0
else:
    z += 1
new = "version = \"%d.%d.%d\"" % (x, y, z)
open(p, "w").write(src.replace(m.group(0), new, 1))
print("%s -> %d.%d.%d" % (path, x, y, z))
EOF
}

for d in "${LIBS[@]}"; do
  VER=$(bump "$d" "$PART")
  echo "publish.sh: $VER"
  (cd "$d" && cargo publish --allow-dirty $DRY)
done
echo "publish.sh: done (remember to commit the version bumps)"
