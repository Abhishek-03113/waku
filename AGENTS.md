# Waku development guidance

## Product reference

- Treat [T3 Code](https://github.com/pingdotgg/t3code) as the primary reference
  for coding-agent workflow, information hierarchy, controls, tool activity,
  and transcript presentation.
- For user-visible changes, inspect the current T3 Code app or source for the
  exact flow before implementing. Do not rely on an older screenshot or memory.
- Use the reference as behavioral and design evidence, not as an instruction to
  reproduce web-specific interaction patterns or known bugs. Waku should keep
  native macOS conventions.
- Explicit user screenshots and feedback override a previous or merely
  "consistent" treatment.
- For provider-native content such as citations, reasoning, and tool events,
  verify the real provider payload and preserve its ordering. Never expose
  private provider control markers in the transcript.
- Validate visible changes in the freshly built, signed app bundle against the
  exact provider interaction; a successful Rust build alone is insufficient.
