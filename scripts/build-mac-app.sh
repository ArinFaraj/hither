#!/bin/sh
set -e
cd "$(dirname "$0")/.."
cargo build --release
APP=target/Drop.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
ICONSET=target/AppIcon.iconset
rm -rf "$ICONSET"
xcrun swift mac/icon.swift "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
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
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>local.drop.menu</string>
      <key>CFBundleURLSchemes</key><array><string>dropmenu</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
codesign --force --deep -s - "$APP"
echo "$APP"
