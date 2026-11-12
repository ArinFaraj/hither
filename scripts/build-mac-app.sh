#!/bin/sh
set -e
cd "$(dirname "$0")/.."
cargo build --release
APP=target/Drop.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
xcrun swiftc -O -swift-version 5 -parse-as-library -target arm64-apple-macos14 mac/DropApp.swift -o "$APP/Contents/MacOS/DropMenu"
cp target/release/drop "$APP/Contents/MacOS/drop"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Drop</string>
  <key>CFBundleIdentifier</key><string>local.drop.menu</string>
  <key>CFBundleExecutable</key><string>DropMenu</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
</dict>
</plist>
PLIST
codesign --force --deep -s - "$APP"
echo "$APP"
