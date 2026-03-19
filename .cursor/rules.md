# Rust Code Quality and Conventions (Codex)

This document defines how we write, format, test, lint, and ship Rust code in this repo.

## NON-NEGOTIABLES

- Code must compile in CI with no warnings
- Formatting must be clean with rustfmt
- Linting must be clean with Clippy and warnings denied
- Tests must pass
- No unsafe Rust unless explicitly approved, isolated, and tested
- External side effects must be behind explicit boundaries

## PROJECT SETUP

### Required installs

Install Rust via rustup and ensure these are available:

- `cargo`
- `rustfmt`
- `clippy`

Recommended tools:

- `cargo-deny`
- `cargo-audit`
- `cargo-nextest`
- `cargo-llvm-cov`
- `cargo-machete`

### Rust version policy

- Define and enforce an MSRV
- Put `rust-version` in the workspace `Cargo.toml`
- Use `rust-toolchain.toml` if pinning is helpful

### Workspace conventions

- Prefer a Cargo workspace for multi-crate projects
- Keep crates small and purpose-driven
- Avoid cyclic dependencies between crates

Suggested crate roles:

- `*-core`: IDs, schemas, pure logic, shared types
- `*-store`: persistence boundary
- `*-kernel`: deterministic logic and invariants
- `*-swarm`: orchestration and runtime
- `*-tools`: side-effect boundary

## CI GATES

Required CI checks:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all --all-features`

Recommended later:

- `cargo audit`
- `cargo deny check`
- `cargo machete`
- `cargo llvm-cov --all --all-features --lcov --output-path lcov.info`

## STYLE

- Never hand-format; use `cargo fmt`
- Use descriptive names
- Prefer idiomatic Rust and readable control flow
- Comments explain why, not what

## PUBLIC API DISCIPLINE

- Default to `pub(crate)`
- Keep public APIs small and stable
- Document public items unless self-evident

## ERROR HANDLING

- No `unwrap()` or `expect()` in production code
- Use `thiserror` for library crates
- Use `anyhow` only at application boundaries
- Preserve context on fallible operations
- Do not swallow errors silently

## TYPES AND OWNERSHIP

- Use strong domain types and newtypes for IDs
- Borrow by default
- Avoid unnecessary clones
- Keep lifetimes simple in public APIs

## ASYNC CONVENTIONS

- Never block the runtime
- Use timeouts at external boundaries
- Be mindful of cancellation safety

## TESTING

- Add unit tests, integration tests, and determinism tests where relevant
- Test serialization round-trips for persisted types
- Test ordering, atomicity, and concurrency constraints
- Keep tests deterministic

## LINTING AND DEPENDENCIES

- Fix Clippy warnings instead of suppressing them
- Keep dependencies lean
- Prefer well-maintained crates
- Regularly run advisory checks

## SECURITY AND OBSERVABILITY

- Validate all external inputs
- Enforce limits on reads, writes, and outputs
- Do not log secrets or sensitive payloads
- Use `tracing` for runtime logs with structured fields
- Document invariants and failure modes for critical modules

## REVIEW CHECKLIST

- Format clean
- Clippy clean with warnings denied
- Tests pass
- No unwrap or expect in production paths
- Errors include context
- No blocking operations in async code
- Timeouts and limits are enforced at boundaries
- No sensitive data in logs
