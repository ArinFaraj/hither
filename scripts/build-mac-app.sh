#!/bin/sh
set -e
cd "$(dirname "$0")/.."
TARGET="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$TARGET" in
  aarch64-*) ARCH=arm64 ;;
  x86_64-*) ARCH=x86_64 ;;
  *) echo "unsupported target $TARGET" >&2; exit 1 ;;
esac
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --target "$TARGET"
APP=target/Hither.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
ICONSET=target/AppIcon.iconset
rm -rf "$ICONSET"
xcrun swift mac/icon.swift "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
xcrun swiftc -O -swift-version 5 -parse-as-library -target "$ARCH-apple-macos14" mac/HitherApp.swift -o "$APP/Contents/MacOS/HitherMenu"
cp "target/$TARGET/release/hither" "$APP/Contents/MacOS/hither"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Hither</string>
  <key>CFBundleIdentifier</key><string>io.github.arinfaraj.hither</string>
  <key>CFBundleExecutable</key><string>HitherMenu</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>io.github.arinfaraj.hither</string>
      <key>CFBundleURLSchemes</key><array><string>hither</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
codesign --force --deep -s - "$APP"
echo "$APP"
