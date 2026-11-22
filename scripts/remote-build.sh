#!/usr/bin/env bash
# Build release binaries natively on another Linux machine (e.g. the ARM64
# Thor) inside rootless Podman. Nothing is installed on the host.
# Files live in ~/megamix-build on the remote; binaries end up in
# ~/megamix-build/target/release/.
#
#   scripts/remote-build.sh <ssh-host>          # build
#   scripts/remote-build.sh <ssh-host> --test   # run the tests natively instead
set -euo pipefail
host=$1
mode=${2:-build}
cd "$(dirname "$0")/.."

tar --exclude=./target --exclude=./.git --exclude=./diva-mod-manager-prompt.md -czf - . \
  | ssh -o BatchMode=yes "$host" 'mkdir -p ~/megamix-build/src ~/megamix-build/cargo-registry ~/megamix-build/target \
      && rm -rf ~/megamix-build/src/* && tar -xzf - -C ~/megamix-build/src'

if [ "$mode" = --test ]; then
  inner='cargo test --workspace 2>&1 | grep -E "test result|FAILED|panicked|^error"'
else
  inner='cargo build --release -p megamix-gui -p megamix 2>&1 | tail -3 && ls -la target/release/megamix target/release/megamix-gui'
fi
# shellcheck disable=SC2029  # $inner is meant to expand locally
ssh -o BatchMode=yes "$host" "set -e; cd ~/megamix-build
  podman build -q -t megamix-build -f src/scripts/linux-build.Containerfile src/scripts >/dev/null
  podman run --rm \\
    -v \"\$PWD/src:/src:Z\" -v \"\$PWD/cargo-registry:/usr/local/cargo/registry:Z\" -v \"\$PWD/target:/src/target:Z\" \\
    -e MEGAMIX_TEST_TMP=/src/target/test-tmp \\
    -w /src megamix-build sh -c '$inner'"
