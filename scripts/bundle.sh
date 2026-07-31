#!/bin/sh
set -eu

profile="${1:-debug}"
case "$profile" in
  debug)
    cargo build
    app_name="Waku Debug"
    bundle_identifier="codes.waku.dev"
    ;;
  release)
    cargo build --release
    app_name="Waku"
    bundle_identifier="codes.waku"
    ;;
  *)
    echo "usage: scripts/bundle.sh [debug|release]" >&2
    exit 2
    ;;
esac

bundle="target/$profile/$app_name.app"
contents="$bundle/Contents"
rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "target/$profile/waku" "$contents/MacOS/$app_name"
cp resources/Info.plist "$contents/Info.plist"
plutil -replace CFBundleDisplayName -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleExecutable -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$bundle_identifier" "$contents/Info.plist"
plutil -replace CFBundleName -string "$app_name" "$contents/Info.plist"
codesign --force --deep --sign - "$bundle"

echo "$bundle"
