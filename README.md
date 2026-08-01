# Waku

Waku is a fast, native, cross-platform control plane for local coding agents. It is built
in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts, and provider IDs on the local
machine.

The first MVP supports:

| Provider | Transport | Session continuity | Checkpoint rollback |
| --- | --- | --- | --- |
| [Amp](https://ampcode.com/) | Claude-compatible `stream-json` | Native Amp thread ID | Not exposed by this transport |
| Claude Code | `stream-json` | Native Claude session cursor | Message-point session fork |
| Codex CLI | `app-server` JSON-RPC | `thread/start` and `thread/resume` | `thread/rollback` |
| OpenCode | Native JSON events | Native OpenCode session cursor | Not exposed by this transport |
| Grok Build | Native `streaming-json` | Native Grok session cursor | Not exposed by this transport |
| Pi | Native RPC JSONL | Pi session file and ID | Not exposed by this transport |

Each provider is connected through its strongest structured interface and
translated into Waku's small, provider-neutral event model. Grok uses its
ordered headless NDJSON stream rather than ACP.

## Product reference

[T3 Code](https://github.com/pingdotgg/t3code) is Waku's primary product
reference for coding-agent workflows, information hierarchy, tool activity,
and transcript presentation. Before changing user-visible behavior, inspect
the same flow in the current T3 Code app or source rather than relying on an
older screenshot or recollection.

T3 Code is reference evidence, not a requirement to copy web-specific details
or existing bugs. Waku should preserve the intent of the reference while using
native macOS interaction conventions and provider-native structured data.
Explicit user screenshots and feedback are the acceptance criteria when they
conflict with a prior treatment.

## Run

Requirements:

- Rust 1.96 or newer
- macOS, Linux, or Windows
- At least one supported agent CLI available in `PATH` or a common local
  install directory

For development, run:

```sh
bun ./scripts/dev.ts
```

This builds and signs `target/debug/Waku Debug.app`, launches the bundled
executable, and watches Rust sources, embedded assets, resources, and Cargo
manifests. After a successful rebuild it restarts the app automatically. A
failed build leaves the last working app open. Press `Ctrl-C` to stop the
watcher and app; quitting the app also exits the watcher.

Once started, the normal development assumption is that this watcher and its
debug app are already running. Leave their lifecycle to the watcher: edit the
source, wait for its successful rebuild/relaunch, and validate the updated app.
Do not run `scripts/bundle.sh debug`, launch a second watcher, or manually
quit/relaunch `Waku Debug.app` unless recovering a watcher that is confirmed to
be unavailable.

Debug builds use the app name `Waku Debug`, bundle identifier `codes.waku.dev`,
and their own `Waku Debug/state.json` local data directory. Release builds use
`Waku`, bundle identifier `codes.waku`, and `Waku/state.json`, so both apps can
be installed and used without sharing state.

Running `cargo run` directly is useful for quick terminal debugging, but it
launches a bare executable without the macOS app-bundle identity used by
accessibility tools.

Waku detects `amp`, `claude`, `codex`, `opencode`, `grok`, and `pi` at launch.
Existing CLI authentication and configuration remain owned by each provider.
Amp's Low, Medium, High, and Ultra agent modes appear in the model picker, and
its Fast serving tier is available in model traits. Pi models are read from its
live RPC catalog, including model-specific thinking levels; Waku does not
invent fallback Pi models.

To produce a native app bundle:

```sh
scripts/bundle.sh release
open target/release/Waku.app
```

### Production DMG

The Bun TypeScript packager builds the release executable, assembles and signs
`Waku.app`, uses [`create-dmg`](https://github.com/create-dmg/create-dmg) for
the Finder layout and Applications drop target, submits the result for
notarization, and staples the accepted ticket:

```sh
brew install create-dmg
xcrun notarytool store-credentials NOTARY

bun scripts/package-dmg.ts
```

The packager selects the Developer ID identity matching team `GJE9R5VE87` and
the `NOTARY` keychain profile by default. The default artifact is
`dist/Waku-<version>.dmg`. Use an ad-hoc signature to exercise the complete
local build and disk-image flow without Apple distribution credentials:

```sh
bun scripts/package-dmg.ts --adhoc
```

Run `bun scripts/package-dmg.ts --help` for output, version, build-number, and
notarization options.

## Interaction model

- Add local project folders from the sidebar.
- Start independent sessions with `⌘N`.
- Search and select a provider model before the first message; its reasoning
  effort and service tier are remembered for new tasks too, and favorite models
  stay available in the picker.
- Tune provider-advertised reasoning effort and speed from one compact model
  traits menu.
- Choose Supervised, Auto-accept edits, Auto, or Full access independently from
  the Build/Plan interaction mode.
- Amp currently supports Build with Full access; Waku passes the selected agent
  mode and Fast tier to the CLI and resumes the exact native Amp thread.
- Pi currently supports Build with Full access; unsupported Pi access modes
  fail explicitly instead of pretending to provide approval semantics.
- Stop the active turn with `Escape`.
- In a Git project, use **Rewind to here** beneath a Claude Code prompt or
  **Edit** beneath a Codex prompt.
- Toggle the sidebar with `⌘⇧S` and focus the composer with `⌘L`.

State is written atomically to the profile-specific platform-local application
data directory described above. No telemetry or remote Waku service is involved.

## Architecture

`src/driver/` contains provider-specific transports. They emit normalized
`DriverEvent` values for connection state, streamed text, reasoning, tool
activity, permissions, completion, and errors. `src/app.rs` owns the GPUI view
state and never parses a provider wire format.

The MVP keeps the Codex and Pi transports alive for a session. Amp, Claude Code,
OpenCode, and Grok use resumable per-turn processes, preserving their native
thread or session IDs. The next product layer is richer provider-native
configuration: structured diffs, file attachments, and OpenCode's managed
server API.

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

User-message actions restore the checkpoint immediately before the selected
prompt, ask the provider to roll back that prompt and every later native turn,
then truncate Waku's matching messages and activity blocks. Codex's **Edit**
button opens an inline editor in the original user bubble; Send performs
`thread/rollback` and starts a replacement turn with the edited prompt.
Claude's **Rewind to here** restores the original prompt into the main composer.
Claude Code records the native message UUID for each completed turn and creates
a fresh, remapped session fork through the preceding turn, so the original
Claude conversation remains intact while Waku resumes from the rewound point.
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
