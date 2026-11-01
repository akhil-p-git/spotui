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

# Spotify's prebuilt macOS spotifyd binaries link Homebrew's libportaudio (and libdbus),
# which a stock Mac lacks. Build it from source with only the pure-Rust rodio backend.
git clone --depth 1 --branch "$SPOTIFYD_VERSION" https://github.com/Spotifyd/spotifyd "$work/spotifyd-src"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  (
    cd "$work/spotifyd-src"
    cargo build --release --locked --target "$target" \
      --no-default-features --features rodio_backend
  )
done
lipo -create -output "$app/Contents/Resources/bin/spotifyd" \
  "$work/spotifyd-src/target/aarch64-apple-darwin/release/spotifyd" \
  "$work/spotifyd-src/target/x86_64-apple-darwin/release/spotifyd"

install -m 755 packaging/macos/Spotui "$app/Contents/MacOS/Spotui"
install -m 755 packaging/macos/run.sh "$app/Contents/Resources/run.sh"
install -m 644 packaging/macos/Spotui.icns "$app/Contents/Resources/Spotui.icns"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
chmod 755 "$app/Contents/Resources/bin/"*

# The bundle must run on a Mac with nothing installed: fail on any non-system library.
for bin in "$app/Contents/Resources/bin/"*; do
  for arch in arm64 x86_64; do
    extra="$(otool -arch "$arch" -L "$bin" | tail -n +2 | awk '{print $1}' \
      | grep -Ev '^(/System/Library/|/usr/lib/)' || true)"
    if [ -n "$extra" ]; then
      echo "error: $bin ($arch) needs libraries a stock Mac lacks:" >&2
      echo "$extra" >&2
      exit 1
    fi
  done
done

codesign --force --sign - "$app/Contents/Resources/bin/spotui"
codesign --force --sign - "$app/Contents/Resources/bin/spotifyd"
codesign --force --sign - "$app"

ln -s /Applications "$work/dmg/Applications"
out="dist/Spotui-$version.dmg"
rm -f "$out"
hdiutil create -volname "Spotui" -srcfolder "$work/dmg" -ov -format UDZO "$out"
echo "Built $out"
