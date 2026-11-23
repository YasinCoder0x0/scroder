#!/bin/sh
# Builds dist/ScreenRec.app and dist/ScreenRec-<version>-macOS.dmg.
#
#   packaging/macos/bundle.sh                 # native architecture
#   packaging/macos/bundle.sh --universal     # Intel + Apple Silicon
#
# The app is ad-hoc signed. For distribution outside your own machines, sign
# it with a Developer ID and notarize it (see README).
set -eu

cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

if [ "${1:-}" = "--universal" ]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    mkdir -p target/universal
    lipo -create -output target/universal/screen_rec \
        target/aarch64-apple-darwin/release/screen_rec \
        target/x86_64-apple-darwin/release/screen_rec
    binary=target/universal/screen_rec
    arch=universal
else
    cargo build --release
    binary=target/release/screen_rec
    arch=$(uname -m)
fi

app=dist/ScreenRec.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/ScreenRec"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"

# Icon: the 1024 px master scaled into every .icns slot.
iconset=$(mktemp -d)/ScreenRec.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    double=$((size * 2))
    sips -z $size $size resources/icon_1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $double $double resources/icon_1024.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/ScreenRec.icns"

codesign --force --deep --sign - "$app"

dmg="dist/ScreenRec-$version-macOS-$arch.dmg"
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
hdiutil create -volname "ScreenRec" -srcfolder "$staging" -ov -format UDZO "$dmg" >/dev/null
echo "Built $app and $dmg"
