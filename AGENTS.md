# AGENTS.md

Project conventions and rules for AI agents working on easy-usb.

## Deferred Items & Tech Debt

During development or code review, whenever you identify:

- A task that should be done but is out of scope for the current story
- Technical debt (refactor needed, missing tests, deprecated API usage)
- A known limitation or shortcut taken intentionally

**ALWAYS create a GitHub issue** in `easy-usb/easy-usb` with:

- Title prefixed with `[debt]` or `[deferred]`
- Body describing the finding, where it was found (file/line), and suggested fix
- Label: `tech-debt` or `deferred`
- Link to the story/PR that surfaced it

Do NOT rely on inline comments or story notes alone — issues persist and are trackable.

## Rust Conventions

- Edition 2024, `rustfmt.toml` at root (`max_width = 120`)
- No `unwrap()` / `expect()` in production code (clippy denies them)
- Core crate (`easy-usb-core`) must have zero platform-specific dependencies (AD-1)
- Tests use `#[cfg(test)] mod tests` inside each crate's `src/`

## CI

- `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace` on every push
- `cargo deny check --config .github/cargo-deny.toml advisories licenses` for license/audit
- Codecov via `cargo-llvm-cov`
