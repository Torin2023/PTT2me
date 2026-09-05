#!/bin/bash
set -euo pipefail

MODE="${1:---build-only}"
case "$MODE" in
  --build-only|--preflight|--run) ;;
  *) echo "Usage: $0 [--build-only|--preflight|--run]" >&2; exit 2 ;;
esac
if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
  echo "Requires Apple Silicon macOS 13+" >&2
  exit 2
fi
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
TARGET_DIR="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
OUT="$TARGET_DIR/insertion-gui"
APP="$OUT/InsertionFixture.app"
if [[ "$MODE" == --build-only ]]; then
cargo build --locked --example insertion_bridge --features test-support
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp tests/gui/fields.html "$APP/Contents/Resources/fields.html"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.ptt2me.insertion-fixture</string>
<key>CFBundleName</key><string>PTT2me Insertion Fixture</string>
<key>CFBundleExecutable</key><string>InsertionFixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
xcrun clang -target arm64-apple-macosx13.0 -fobjc-arc \
  -c tests/gui/clipboard.m -o "$OUT/clipboard.o"
xcrun swiftc -swift-version 5 -target arm64-apple-macosx13.0 \
  -module-cache-path "$OUT/swift-module-cache" \
  -import-objc-header tests/gui/insertion_bridge.h tests/gui/main.swift \
  "$TARGET_DIR/debug/examples/libinsertion_bridge.a" "$OUT/clipboard.o" \
  -framework AppKit -framework WebKit -framework ApplicationServices \
  -framework CoreGraphics -framework Security -framework SystemConfiguration \
  -lc++ -lresolv -o "$APP/Contents/MacOS/InsertionFixture"
codesign --force --sign - "$APP"
echo "Built: $APP"
elif [[ ! -x "$APP/Contents/MacOS/InsertionFixture" ]]; then
  echo "Build the fixture first: bash scripts/test-insertion-gui.sh --build-only" >&2
  exit 2
fi
case "$MODE" in
  --preflight) "$APP/Contents/MacOS/InsertionFixture" --preflight ;;
  --run)
    REPORT="$OUT/report-$(date -u +%Y%m%dT%H%M%SZ).json"
    echo "Report: $REPORT"
    exec "$APP/Contents/MacOS/InsertionFixture" "$REPORT"
    ;;
esac
