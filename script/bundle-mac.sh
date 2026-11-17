#!/usr/bin/env bash
# Build Scriptorium.app (macOS only).
#
#   script/bundle-mac.sh                # arm64 (Apple Silicon), the default
#   UNIVERSAL=1 script/bundle-mac.sh    # arm64 + x86_64 merged with lipo
#
# Output: target/bundle/Scriptorium.app, containing the app binary and the
# `scriptorium` CLI (so agents on the Mac can use it), ad-hoc signed.
# Not notarized: on first launch, right-click > Open, or
#   xattr -dr com.apple.quarantine Scriptorium.app
set -euo pipefail

[[ "$(uname -s)" == Darwin ]] || { echo "bundle-mac.sh must run on macOS" >&2; exit 1; }

cd "$(dirname "$0")/.."
ROOT=$PWD

export MACOSX_DEPLOYMENT_TARGET=13.0

if [[ "${UNIVERSAL:-0}" == 1 ]]; then
  TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
else
  TARGETS=(aarch64-apple-darwin)
fi

VERSION=$(cargo pkgid -p scriptorium-app | sed 's/.*@//')

for t in "${TARGETS[@]}"; do
  if command -v rustup >/dev/null; then rustup target add "$t" >/dev/null; fi
  cargo build --release --locked --target "$t" -p scriptorium-app -p scriptorium-cli
done

APP="$ROOT/target/bundle/Scriptorium.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

for bin in scriptorium-app scriptorium; do
  inputs=()
  for t in "${TARGETS[@]}"; do inputs+=("$ROOT/target/$t/release/$bin"); done
  if (( ${#inputs[@]} > 1 )); then
    lipo -create "${inputs[@]}" -output "$APP/Contents/MacOS/$bin"
  else
    cp "${inputs[0]}" "$APP/Contents/MacOS/$bin"
  fi
done

# Icon: use a real one if it's been added, else draw a plain placeholder.
ICON_SRC="$ROOT/crates/app/assets/Scriptorium.icns"
if [[ -f "$ICON_SRC" ]]; then
  cp "$ICON_SRC" "$APP/Contents/Resources/Scriptorium.icns"
else
  work=$(mktemp -d)
  python3 - "$work/icon.png" <<'PY'
# A 1024px cream page with a dark margin rule: placeholder until a real icon.
import struct, sys, zlib
n = 1024
paper, ink, edge = (250, 246, 234), (60, 60, 70), (200, 192, 170)
rows = []
for y in range(n):
    row = bytearray([0])
    for x in range(n):
        inside = 120 <= x < n - 120 and 80 <= y < n - 80
        if not inside:
            row += bytes((0, 0, 0, 0))
        elif x < 128 or x >= n - 128 or y < 88 or y >= n - 88:
            row += bytes(edge + (255,))
        elif 260 <= x < 272 or (y - 200) % 64 < 4 and 300 <= x < n - 200 and 200 <= y < n - 180:
            row += bytes(ink + (255,))
        else:
            row += bytes(paper + (255,))
    rows.append(bytes(row))
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
PY
  set=$work/Scriptorium.iconset
  mkdir "$set"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$work/icon.png" --out "$set/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) "$work/icon.png" --out "$set/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$set" -o "$APP/Contents/Resources/Scriptorium.icns"
  rm -rf "$work"
fi

cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Scriptorium</string>
  <key>CFBundleExecutable</key><string>scriptorium-app</string>
  <key>CFBundleIconFile</key><string>Scriptorium</string>
  <key>CFBundleIdentifier</key><string>org.scriptorium.app</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Scriptorium</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSAppleEventsUsageDescription</key><string>Scriptorium quits and reopens Claude so it picks up its new settings.</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Ad-hoc signature: arm64 binaries won't run unsigned. Not notarized.
codesign --force --deep -s - "$APP"
codesign --verify --deep --strict "$APP"

echo "Built $APP ($VERSION, ${TARGETS[*]})"
lipo -info "$APP/Contents/MacOS/scriptorium-app"
