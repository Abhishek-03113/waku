# Waku

Waku is a fast, native macOS control plane for local coding agents. It is built
in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts, and provider IDs on the local
machine.

The first MVP supports:

| Provider | Transport | Session continuity |
| --- | --- | --- |
| Claude Code | `stream-json` | Native Claude session ID |
| Codex CLI | `app-server` JSON-RPC | `thread/start` and `thread/resume` |
| OpenCode | Native JSON events | Native OpenCode session ID |
| Grok Build | ACP over stdio | `session/new` and `session/load` |

ACP is intentionally not the universal abstraction. Each provider is connected
through its strongest native interface and translated into Waku's small,
provider-neutral event model. Grok uses ACP because that is Grok Build's native
GUI protocol.

## Run

Requirements:

- macOS
- Rust 1.96 or newer
- At least one supported agent CLI available in `PATH` or a common local
  install directory

For development, run:

```sh
scripts/dev.sh
```

This builds and signs `target/debug/Waku.app`, launches the bundled executable,
and watches Rust sources, embedded assets, resources, and Cargo manifests. After
a successful rebuild it restarts the app automatically. A failed build leaves
the last working app open. Press `Ctrl-C` to stop the watcher and app.

Running `cargo run` directly is useful for quick terminal debugging, but it
launches a bare executable without the macOS app-bundle identity used by
accessibility tools.

Waku detects `claude`, `codex`, `opencode`, and `grok` at launch. Existing CLI
authentication and configuration remain owned by each provider.

To produce a native app bundle:

```sh
scripts/bundle.sh release
open target/release/Waku.app
```

## Interaction model

- Add local project folders from the sidebar.
- Start independent sessions with `⌘N`.
- Select the provider before the first message.
- Cycle Plan, Ask, and Auto execution modes from the composer.
- Stop the active turn with `Escape`.
- Toggle the sidebar with `⌘⇧S` and focus the composer with `⌘L`.

State is written atomically to the platform-local application data directory as
`Waku/state.json`. No telemetry or remote Waku service is involved.

## Architecture

`src/driver/` contains provider-specific transports. They emit normalized
`DriverEvent` values for connection state, streamed text, reasoning, tool
activity, permissions, completion, and errors. `src/app.rs` owns the GPUI view
state and never parses a provider wire format.

The MVP keeps Codex and Grok transports alive for a session. Claude Code and
OpenCode use resumable per-turn processes, preserving their native session IDs.
The next product layer is richer provider-native configuration: model pickers,
structured diffs, file attachments, and OpenCode's managed server API.

## Verify

```sh
cargo fmt --all --check
cargo check
cargo test
```
