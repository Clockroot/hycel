# Working in Mycel (for AI agents and humans)

Mycel is a pre-alpha Rust game-engine project. Read `README.md`, `docs/architecture.md`, and `docs/engineering-contracts.md` before changing architectural boundaries, persistence, privacy, or dependencies.

## Current invariants

- `mycel-core` is platform-independent and contains no unsafe code, I/O, wall-clock access, renderer, or OS API.
- Simulation time is explicit and tick-based. Keep float/render/platform concerns outside deterministic core code.
- Do not add a dependency without recording why it is needed, its maintenance/license/MSRV/target status, and what boundary it belongs behind; update `Cargo.lock` and run `cargo deny check`.
- Do not silently change project formats or public command output. Add schema/command tests and migration notes.
- Do not claim x86-64/ARM64 or OS support without CI evidence.

## Change workflow

1. Inspect relevant code and docs; state assumptions if the request is underspecified.
2. Make the smallest cohesive change.
3. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --all-targets --locked`, `cargo deny check`, and `cargo run -p mycel-demo --locked`.
4. Report changed files, commands run, results, and known gaps. Never claim a test passed unless it was run.
5. For architecture/API/schema/security changes, update the relevant docs and tests.

Prefer structured, incremental changes and actionable diagnostics. Do not perform broad cleanup unrelated to the requested task.
