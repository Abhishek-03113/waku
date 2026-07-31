# Waku

Waku is a fast, native macOS control plane for local coding agents. It is built
in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts, and provider IDs on the local
machine.

The first MVP supports:

| Provider | Transport | Session continuity | Checkpoint rollback |
| --- | --- | --- | --- |
| Claude Code | `stream-json` | Native Claude session cursor | Not exposed by this transport |
| Codex CLI | `app-server` JSON-RPC | `thread/start` and `thread/resume` | `thread/rollback` |
| OpenCode | Native JSON events | Native OpenCode session cursor | Not exposed by this transport |
| Grok Build | Native `streaming-json` | Native Grok session cursor | Not exposed by this transport |

Each provider is connected through its strongest structured interface and
translated into Waku's small, provider-neutral event model. Grok uses its
ordered headless NDJSON stream rather than ACP.

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
- In a Git project, revert a completed Codex turn from its checkpoint action.
- Toggle the sidebar with `⌘⇧S` and focus the composer with `⌘L`.

State is written atomically to the platform-local application data directory as
`Waku/state.json`. No telemetry or remote Waku service is involved.

## Architecture

`src/driver/` contains provider-specific transports. They emit normalized
`DriverEvent` values for connection state, streamed text, reasoning, tool
activity, permissions, completion, and errors. `src/app.rs` owns the GPUI view
state and never parses a provider wire format.

The MVP keeps the Codex transport alive for a session. Claude Code, OpenCode,
and Grok use resumable per-turn processes, preserving their native session IDs.
The next product layer is richer provider-native configuration: model pickers,
structured diffs, file attachments, and OpenCode's managed server API.

### Sessions and checkpoints

Waku follows [T3 Code](https://github.com/pingdotgg/t3code)'s
split-responsibility design: Waku's own state is the canonical UI timeline,
while each task also stores a typed provider resume cursor (`threadId`,
`sessionId`, and provider-specific extensions). Resuming a task starts or
reconnects the provider transport with that cursor; Waku does not reconstruct
the provider conversation by replaying transcript text.

Every submitted prompt creates a durable turn record. In Git projects, Waku
captures the full working tree in an isolated temporary index and stores the
snapshot under a hidden ref:

```text
refs/waku/session-<session-id>-turn-<turn-number>
```

Reverting restores that ref, asks the provider to roll back the same number of
native turns, then truncates Waku's turn-owned messages and activity blocks.
Before any restore, Waku creates a temporary safety ref; if provider rollback
fails, it restores the original working tree and leaves the local timeline
unchanged. The revert action is capability-gated, so providers whose current
transport cannot roll back native conversation state never offer a
filesystem-only revert.

## Verify

```sh
cargo fmt --package waku -- --check
cargo check
cargo test
```
