# Waku development guidance

## Development runtime

- Assume `bun ./scripts/dev.ts` is already running and owns the current
  `Waku Debug.app` process. Source changes are rebuilt, signed, and relaunched
  automatically.
- During normal development and UI validation, do not run
  `scripts/bundle.sh debug`, start a second watcher, or manually quit/relaunch
  `Waku Debug.app`. Quitting the app also stops the watcher.
- After an edit, wait for the watcher to finish its successful rebuild and
  validate the freshly relaunched debug app. Only start or recover the watcher
  manually when it is confirmed unavailable.

## Product reference

- Use [T3 Code](https://github.com/pingdotgg/t3code) as a reference when a task
  concerns coding-agent workflow, information hierarchy, controls, tool
  activity, or transcript presentation and the comparison would materially
  clarify an ambiguous product decision, or when the user explicitly asks for
  the comparison.
- Do not inspect T3 Code for localized bug fixes, straightforward visual
  corrections, native platform behavior, or changes already specified clearly
  by the user. When T3 Code is relevant, inspect its current app or source
  rather than relying on an older screenshot or memory.
- Use the reference as behavioral and design evidence, not as an instruction to
  reproduce web-specific interaction patterns or known bugs. Waku should keep
  native macOS conventions.
- Explicit user screenshots and feedback override a previous or merely
  "consistent" treatment.
- For provider-native content such as citations, reasoning, and tool events,
  verify the real provider payload and preserve its ordering. Never expose
  private provider control markers in the transcript.
- Validate visible changes in the freshly rebuilt, signed app managed by the
  dev watcher against the exact provider interaction; a successful Rust build
  alone is insufficient.
