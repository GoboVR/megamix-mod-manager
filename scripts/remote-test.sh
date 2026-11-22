#!/usr/bin/env bash
# Cross-compile megamix-core's test binaries and run them on another machine
# over SSH (no Rust needed there). Files go to ~/megamix-test on the remote.
#
#   scripts/remote-test.sh <ssh-host> windows   # x86_64 Windows (mingw-w64)
#   scripts/remote-test.sh <ssh-host> arm64     # aarch64 Linux (static musl, rust-lld)
#
# Extra args after the platform are passed to every test binary
# (e.g. `--ignored --nocapture` to also run the real-system detection test).
set -euo pipefail
host=$1; platform=$2; shift 2
cd "$(dirname "$0")/.."

case $platform in
  # RAR (UnRAR C++) is left out of these cross builds: unrar_sys doesn't
  # build with MinGW, and the static musl build has no C++ compiler.
  # Release builds use MSVC on Windows, where RAR is included.
  windows) target=x86_64-pc-windows-gnu; ext=.exe; features=--no-default-features ;;
  arm64)   target=aarch64-unknown-linux-musl; ext=; features=--no-default-features
           export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld ;;
  *) echo "unknown platform: $platform" >&2; exit 2 ;;
esac

rustup target add "$target" >/dev/null
mapfile -t exes < <(cargo test -p megamix-core ${features:-} --target "$target" --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import sys, json
for l in sys.stdin:
    m = json.loads(l)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        print(m["executable"])')

stage=$(mktemp -d "${TMPDIR:-/tmp}/megamix-remote.XXXXXX")
trap 'rm -rf "$stage"' EXIT
names=()
for f in "${exes[@]}"; do
  n=$(basename "$f" | sed -E "s/-[0-9a-f]+$ext\$/$ext/")
  cp "$f" "$stage/$n"; names+=("$n")
done
cp -r crates/megamix-core/tests/fixtures "$stage/"

ssh -o BatchMode=yes "$host" "$( [ $platform = windows ] \
  && echo 'if not exist megamix-test mkdir megamix-test' \
  || echo 'rm -rf ~/megamix-test && mkdir -p ~/megamix-test')"
scp -q -r "$stage"/* "$host:megamix-test/"

if [ $platform = windows ]; then
  cmd="cd /d %USERPROFILE%\\megamix-test && set \"MEGAMIX_FIXTURES=%USERPROFILE%\\megamix-test\\fixtures\" && set \"MEGAMIX_TEST_TMP=%USERPROFILE%\\megamix-test\\tmp\""
  for n in "${names[@]}"; do cmd+=" && $n $*"; done
else
  cmd="cd ~/megamix-test && export MEGAMIX_FIXTURES=\$HOME/megamix-test/fixtures MEGAMIX_TEST_TMP=\$HOME/megamix-test/tmp"
  for n in "${names[@]}"; do cmd+=" && ./$n $*"; done
fi
ssh -o BatchMode=yes "$host" "$cmd"
