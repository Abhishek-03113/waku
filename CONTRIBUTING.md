# Contributing to Waku

Thanks for helping improve Waku. Bug reports, focused fixes, tests, and
well-scoped features are welcome.

## Development setup

The debug app currently requires:

- macOS
- Rust 1.96 or newer
- Bun
- A supported agent CLI when testing a provider integration

Install dependencies and start the development watcher from the repository
root:

```sh
bun install
bun run dev
```

The watcher builds and signs `target/debug/Waku Debug.app`, launches it, and
rebuilds and relaunches it after source changes. Keep that watcher running while
you work. Do not start a second watcher or manually relaunch the debug app.
Press `Ctrl-C`, or quit the app, to stop it.

## Making changes

- Before starting work on anything larger than a bug fix, open an issue and
  discuss the proposal first.
- Keep changes focused and follow the existing Rust and GPUI conventions.
- Keep filesystem, process, network, and other blocking work off the UI thread.
  Rendering and row-building paths must read data already held in memory.
- Keep long collections virtualized and per-frame work proportional to visible
  content.
- Make every mouse control keyboard-operable, preserve visible focus, honor
  reduce-motion settings, and do not communicate state with color alone.
- Prefer provider-neutral behavior when a change applies to every agent, while
  preserving provider-native event order and session semantics.
- Add or update tests for behavior that can be verified without the UI.

## Checks

Run the focused checks relevant to your change, then run the full baseline
before opening a pull request:

```sh
cargo fmt --package waku -- --check
cargo check
cargo test
```

For user-visible changes, wait for the watcher to report a successful rebuild
and validate the freshly relaunched app. Include screenshots or a short
recording in the pull request when they make the result easier to review.

## Pull requests

In the pull request description:

- Explain the problem and the chosen solution.
- List the checks you ran.
- Call out known limitations or follow-up work.
- Link the related issue, if one exists.

By contributing, you agree that your contribution will be licensed under the
[GNU General Public License v3.0 only](LICENSE).
