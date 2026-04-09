# Kiro Rules — snapcast-rs

## CRITICAL: Never Do Without Explicit User Consent

- **Never `git push`** — always ask first
- **Never create PRs** — always ask first
- **Never merge PRs** — always ask first
- **Never delete branches/tags/releases** — always ask first
- **Never run `cargo publish`** — always ask first

## Commit Rules

- Commit locally without asking — this is safe and expected
- Always use conventional commits
- Amend only the most recent commit, and only when it's the same topic
- Split unrelated changes into separate commits

## Principles

**IMPORTANT: No hacks, no shortcuts.** When tempted to say "the simplest way" — stop, step back, and think about the proper enterprise-grade solution. Even if it requires a heavy refactor, do it right. Quick fixes accumulate into technical debt. The correct pattern, implemented once, is always cheaper than a hack that needs to be replaced later.

### SSOT — Single Source of Truth
Every piece of knowledge (config, constants, types, logic) must live in exactly one place. If something needs to be referenced elsewhere, import it — don't duplicate it. When a value changes, it should only need to change in one location.

### DRY — Don't Repeat Yourself
Duplicated code is a bug waiting to happen. Extract shared logic into functions, traits, or modules. If you find yourself copying code, that's a signal to refactor. Two is a coincidence, three is a pattern — extract it.

### Convention over Configuration
Prefer sensible defaults and predictable project structure. Follow Rust ecosystem conventions (module layout, error handling patterns, naming). When the community has a standard way, use it.

### Fail Fast, Recover Gracefully
Validate inputs early. Return errors immediately rather than propagating invalid state. Use `Result` and `?` — don't `unwrap()` in library code. Provide clear, actionable error messages that help the user fix the problem.

### No Dead Code Exemptions
Never add `#![allow(dead_code)]` or `#[allow(unused)]` to "fix later." If code is unused after a refactor, remove it in the same commit. Blanket allows hide real issues and accumulate technical debt. Fix it now or don't commit.

### Test-Driven Development
Write tests first, then implementation. Every protocol message, every decoder, every state machine must have tests before the implementation is written. Use test vectors derived from the original C++ snapcast code to ensure byte-level compatibility.

## Project Context

This is a Rust rewrite of the [Snapcast](https://github.com/snapcast/snapcast) client (`snapclient`). The goal is byte-level protocol compatibility with the existing C++ snapserver. The binary protocol specification lives in the original repo at `doc/binary_protocol.md`.

### Workspace Structure
```
snapcast-rs/
├── Cargo.toml                  # [workspace]
├── crates/
│   ├── snapcast-proto/         # Shared: binary protocol, message types, sample format
│   └── snapclient-rs/          # Client binary
```

### FFI Policy
- Prefer pure Rust crates where possible (decoders, resampling, protocol logic)
- Accept FFI for platform audio APIs (ALSA, PulseAudio, CoreAudio, WASAPI, PipeWire)
- Accept FFI for Opus decoding (`audiopus`) if no pure-Rust alternative exists

### Error Handling
- `thiserror` in library crates (`snapcast-proto`)
- `anyhow` in binary crates (`snapclient-rs`)
- Return `Result<T>` from all fallible functions
- No `.unwrap()` or `.expect()` outside of tests
- No `panic!()` in library code

### Async Architecture
- `tokio` with `rt-multi-thread` for network I/O, timers, signals
- Audio playback runs on a dedicated OS thread (not async) — audio backends require real-time guarantees
- Bridge between async and audio thread via lock-free channel or ring buffer

## Code Changes

- Use `str_replace` for edits, not Python scripts
- Run `cargo build` and `cargo test` after changes before reporting success
- Run `cargo fmt --all` before committing

## Communication

- Don't run interactive commands (cargo login, git rebase -i) — they freeze the terminal
- When a command fails, show the error and propose a fix — don't retry silently
- When context is getting long, summarize progress and suggest saving

### Naming
- Modules: `snake_case` (Rust convention)
- Types: `PascalCase`
- Functions/methods: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- Crate names: `snapcast-proto`, `snapclient-rs`

### Formatting & Linting
- `rustfmt` with default config
- `clippy` with `-D warnings`
- CI enforces both — no merge without clean fmt + clippy

### Testing
- Unit tests: `#[cfg(test)] mod tests` in the same file
- Integration tests: `tests/` directory
- Test vectors: hardcoded byte arrays derived from C++ serialization code
- `#[tokio::test]` for async tests
- Every protocol message type must have round-trip serialization tests
- Every decoder must be tested against known encoded input

### Dependencies
- Prefer pure Rust crates where possible (see FFI Policy above)
- Pin workspace dependency versions in root `Cargo.toml` `[workspace.dependencies]`
- Minimize dependency count — every crate must justify its existence

### Git Workflow

**Conventional Commits** — every commit message follows the format:
```
<type>(<scope>): <description>

[optional body]
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `ci`, `chore`, `perf`, `style`
Scopes: `proto`, `client`, `build`, `ci`, `docs`

Examples:
```
feat(proto): implement BaseMessage serialization
test(proto): add round-trip tests for WireChunk
fix(client): handle reconnect on connection loss
refactor(client): extract time sync into separate module
ci: add clippy to pre-push hook
```

**Git Hooks (enforced):**
- **pre-commit**: `cargo fmt -- --check`
- **pre-push**: `cargo clippy -- -D warnings`
- Hooks live in `.githooks/`, activated via `make setup` (runs `git config core.hooksPath .githooks`)

## Development Workflow
```bash
make setup                     # First-time: configure git hooks
cargo build                    # Build
cargo test                     # Run all tests
cargo clippy -- -D warnings    # Lint (deny all warnings)
cargo fmt -- --check           # Format check
```
