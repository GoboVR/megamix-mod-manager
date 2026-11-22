#!/usr/bin/env bash
# Pack release binaries into dist/megamix-linux-<arch>.tar.gz (the layout
# install.sh expects).
#   scripts/package-linux.sh [target-dir]   (default: target/release)
set -euo pipefail
cd "$(dirname "$0")/.."
bin=${1:-target/release}
arch=$(file -b "$bin/megamix-gui" | grep -q aarch64 && echo aarch64 || echo x86_64)
name=megamix-linux-$arch
out=dist/$name
rm -rf "$out"
mkdir -p "$out/icons"
cp "$bin/megamix" "$bin/megamix-gui" packaging/linux/install.sh packaging/linux/uninstall.sh README.md LICENSE-MIT LICENSE-APACHE "$out/"
chmod 755 "$out/install.sh" "$out/uninstall.sh" "$out/megamix" "$out/megamix-gui"
cp assets/icon-{64,128,256,512}.png "$out/icons/"
[ -f assets/icon.svg ] && cp assets/icon.svg "$out/icons/"
cp -r assets/steam "$out/steam"
tar -C dist -czf "dist/$name.tar.gz" "$name"
echo "dist/$name.tar.gz"
