#!/bin/sh
set -u

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$root"

app="$root/target/debug/Waku Debug.app"

source_signature() {
    find Cargo.toml Cargo.lock src assets resources -type f \
        -exec shasum -a 256 {} \; |
        shasum -a 256 |
        awk '{ print $1 }'
}

stop_app() {
    pkill -TERM -x "Waku Debug" 2>/dev/null || true
}

build_and_restart() {
    echo "[waku-dev] Building app bundle..."
    if ! "$root/scripts/bundle.sh" debug; then
        echo "[waku-dev] Build failed; keeping the current app open." >&2
        return 1
    fi

    stop_app

    echo "[waku-dev] Launching $app"
    open -n "$app"
}

cleanup() {
    trap - INT TERM EXIT
    stop_app
    exit 0
}

trap cleanup INT TERM EXIT

last_signature="$(source_signature)"
build_and_restart || exit 1
last_signature="$(source_signature)"

echo "[waku-dev] Watching for source changes. Press Ctrl-C to stop."

while :; do
    sleep 0.5
    next_signature="$(source_signature)"

    if [ "$next_signature" = "$last_signature" ]; then
        continue
    fi

    # Give editors a moment to finish atomic saves and related file updates.
    sleep 0.15
    changed_signature="$(source_signature)"
    build_and_restart || true

    # If files change during the build, the next iteration rebuilds once more.
    last_signature="$changed_signature"
done
