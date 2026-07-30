#!/bin/sh
set -eu

profile="${1:-debug}"
case "$profile" in
  debug)
    cargo build
    ;;
  release)
    cargo build --release
    ;;
  *)
    echo "usage: scripts/bundle.sh [debug|release]" >&2
    exit 2
    ;;
esac

bundle="target/$profile/Waku.app"
contents="$bundle/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "target/$profile/waku" "$contents/MacOS/Waku"
cp resources/Info.plist "$contents/Info.plist"
codesign --force --deep --sign - "$bundle"

echo "$bundle"
