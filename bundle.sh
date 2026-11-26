#!/bin/sh
# Build target/duskpaper.app (menu bar app, no Dock icon).
set -eu
cd "$(dirname "$0")"
cargo build --release

APP=target/duskpaper.app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/duskpaper "$APP/Contents/MacOS/duskpaper"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>duskpaper</string>
    <key>CFBundleDisplayName</key><string>duskpaper</string>
    <key>CFBundleIdentifier</key><string>dev.duskpaper</string>
    <key>CFBundleExecutable</key><string>duskpaper</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSAppleEventsUsageDescription</key><string>duskpaper switches Light/Dark mode through System Events.</string>
</dict>
</plist>
EOF
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
echo "built $APP"
