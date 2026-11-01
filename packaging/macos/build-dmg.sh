#!/bin/bash
# Builds dist/Spotui-<version>.dmg. Run on macOS from the repository root.
# Bundles a universal (Apple Silicon + Intel) spotui and spotifyd. Binaries are
# ad-hoc signed only: there is no Apple Developer ID, so Gatekeeper will warn.
set -euo pipefail

SPOTIFYD_VERSION="${SPOTIFYD_VERSION:-v0.4.2}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
work="$(mktemp -d)"
app="$work/dmg/Spotui.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/bin" dist

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  rustup target add "$target"
  cargo build --release --locked --target "$target"
done
lipo -create -output "$app/Contents/Resources/bin/spotui" \
  target/aarch64-apple-darwin/release/spotui target/x86_64-apple-darwin/release/spotui

base="https://github.com/Spotifyd/spotifyd/releases/download/$SPOTIFYD_VERSION"
for arch in aarch64 x86_64; do
  name="spotifyd-macos-$arch-default"
  mkdir -p "$work/$arch"
  (
    cd "$work/$arch"
    curl -fsSL -O "$base/$name.tar.gz" -O "$base/$name.sha512"
    shasum -a 512 -c "$name.sha512"
    tar xzf "$name.tar.gz"
  )
done
lipo -create -output "$app/Contents/Resources/bin/spotifyd" \
  "$work/aarch64/spotifyd" "$work/x86_64/spotifyd"

install -m 755 packaging/macos/Spotui "$app/Contents/MacOS/Spotui"
install -m 755 packaging/macos/run.sh "$app/Contents/Resources/run.sh"
install -m 644 packaging/macos/Spotui.icns "$app/Contents/Resources/Spotui.icns"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
chmod 755 "$app/Contents/Resources/bin/"*

codesign --force --sign - "$app/Contents/Resources/bin/spotui"
codesign --force --sign - "$app/Contents/Resources/bin/spotifyd"
codesign --force --sign - "$app"

ln -s /Applications "$work/dmg/Applications"
out="dist/Spotui-$version.dmg"
rm -f "$out"
hdiutil create -volname "Spotui" -srcfolder "$work/dmg" -ov -format UDZO "$out"
echo "Built $out"
