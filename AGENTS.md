# AGENTS.md

Project conventions and rules for AI agents working on easy-usb.

## Deferred Items & Tech Debt

During development or code review, whenever you identify:

- A task that should be done but is out of scope for the current story
- Technical debt (refactor needed, missing tests, deprecated API usage)
- A known limitation or shortcut taken intentionally

**ALWAYS create a GitHub issue** in `easy-usb/easy-usb` with:

- Title prefixed with `[debt]` or `[deferred]`
- Body describing each finding, where it was found (file/line), and suggested fix
- Label: `tech-debt` or `deferred`
- Link to the story/PR that surfaced it

**One issue per session/code-review**, not one per finding. Batch all deferred items from the same review into a single issue with sections per item.

**Encoding:** When creating GitHub issues via CLI, always use `[System.IO.File]::WriteAllText($path, $body, [System.Text.UTF8Encoding]::new($false))` to write a UTF-8 file (no BOM), then pass it with `--body-file`. Do NOT pass multi-line bodies via `--body` in PowerShell — backticks and special chars will get mangled.

## Rust Conventions

- Edition 2024, `rustfmt.toml` at root (`max_width = 120`)
- No `unwrap()` / `expect()` in production code (clippy denies them)
- Core crate (`easy-usb-core`) must have zero platform-specific dependencies (AD-1)
- Tests use `#[cfg(test)] mod tests` inside each crate's `src/`

## CI

- `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace` on every push
- `cargo deny check --config .github/cargo-deny.toml advisories licenses` for license/audit
- Codecov via `cargo-llvm-cov`

## TDD (red-green-refactor)

When implementing stories via `dev-story`, the Step 5 red-green-refactor cycle is **mandatory**, not aspirational. For every task/subtask:

1. **Red phase (required):** write a failing test first. For type-definition tasks (structs, enums, constants) where a test can't compile without the type, write the test alongside the type definition in the same file block, then verify that `cargo test` reports the test failure (compilation failure = red phase). For logic tasks (functions, encode/decode, state machines), write the test before the implementation — the test should fail with a clear compile error.
2. **Green phase:** write the **minimum** code to make the test pass. Run `cargo test` to confirm.
3. **Refactor phase:** improve structure while keeping tests green.

**Evidence rule:** Before marking any task complete (Step 8), the `cargo test` output must show at least one test transition from failing to passing for that task. If all tests pass on the first run, the red phase was skipped — redo the task.
