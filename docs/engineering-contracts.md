# Hycel engineering contracts

These rules apply to the engine, editor, tools, samples, and CI. They are the default engineering policy for the 1.0 line; exceptions require an ADR, a concrete user need, tests, and documented operational/compatibility impact.

## Architecture boundaries

- Dependency direction and planned crate responsibilities are defined in [`architecture.md`](architecture.md). Keep simulation, renderer, platform, project/asset services, editor, and agent adapters separate by dependency direction.
- `hycel-core` must not depend on a windowing/graphics API, operating-system API, filesystem, network, wall clock, or editor/agent protocol.
- Editor and agent integrations call shared application services; neither owns validation or persistence rules.
- Keep backend/framework types out of stable game and serialized-project APIs.
- Add a crate boundary only when it enforces a real dependency or test boundary; avoid premature fragmentation.

## Memory and unsafe code

- The workspace forbids `unsafe` Rust via the root Cargo lints. Do not weaken that lint casually.
- If a concrete platform/FFI requirement cannot be met safely, first seek a safe maintained dependency. Otherwise write an ADR that scopes the exact unsafe operation, invariants, alternatives, target coverage, and audit strategy. Any lint exception must be confined to the smallest dedicated crate/module, documented at the declaration, and reviewed independently.
- No unsafe implementation is accepted without focused tests and a safety comment explaining the invariants. Unsafe code is never added merely for speculative performance.

## Errors, panics, and diagnostics

- Invalid projects, assets, input, and user actions are untrusted/recoverable errors: return typed errors or structured diagnostics. Do not panic, abort, or silently fall back for invalid user data.
- Use `Result` at subsystem boundaries. Public errors must carry useful context and stable diagnostic codes where callers/agents need to branch on them.
- Do not use `unwrap`, `expect`, or unchecked assumptions on user-controlled or I/O data in production paths. Assertions/panics may protect an internal invariant only when violating it is a programming bug; include enough context to diagnose it.
- Preserve causal sources; errors should say what failed, where, and a safe next step. Avoid logging secrets or full private project contents.
- Do not catch panics as a substitute for validation or error design.

## Data integrity and compatibility

- Never silently delete, overwrite, truncate, or reinterpret project/user data. Destructive migrations and edits require an explicit preview or clear confirmation and a recoverable backup.
- Write project, configuration, and save files transactionally: write a sibling temporary file, flush/close as appropriate, then atomically replace; handle platform limitations and recovery explicitly.
- Persisted formats are versioned. Migration is explicit, ordered, tested on fixtures, and failure-atomic. Unknown fields are preserved or cause a clear error; they are not silently dropped.
- Every file importer applies size/path/reference validation and bounded resource use. Protect against path traversal, symlink escapes, decompression bombs, and malformed assets.
- Keep `Cargo.lock` checked in for applications/workspace tools and use `--locked` in CI and release builds. Format/schema/API changes require tests and migration/release notes.

## Dependencies and supply chain

- Prefer Rust standard-library capabilities when they are clear and sufficient; otherwise choose a maintained dependency with a license and target matrix compatible with Hycel.
- Every direct dependency addition/update needs a review note: purpose and boundary, alternatives considered, maintainer/activity, license/SPDX expression, MSRV, transitive footprint, native/system requirements, OS/architecture coverage, and security/advisory status.
- Crates.io is the only default source. Git/path overrides require an ADR, a pinned revision, ownership and update plan, and an explicit source allowlist entry.
- Cargo-deny checks the locked graph for advisories, disallowed licenses, source changes, wildcard requirements, and duplicate-version growth. Fix findings or record a narrowly scoped reasoned exception; never blanket-ignore an advisory or license finding.
- Generate a machine-readable dependency/license inventory in CI/release evidence. Re-run the audit on every dependency or lockfile change and on the default branch.

## Privacy, networking, and telemetry

- Hycel core/editor/project operations are local-first. No account, cloud, network connection, telemetry, crash upload, analytics, or model provider is required for normal use.
- No telemetry or crash reporting is sent by default. Do not add tracking identifiers or collect usage data. Any future diagnostic upload must be a separate explicit opt-in, preview exactly what is sent, redact secrets, and be independently disableable.
- Engine code may access the network only for a direct user-requested operation (for example, an explicit package/download action) with a visible purpose. Project data is never uploaded implicitly.
- AI agent adapters do not grant arbitrary network, shell, or filesystem access. Follow the least-privilege and project-root boundaries in [`agent-interface.md`](agent-interface.md).

## Testing and release evidence

- Every behavioral change has a regression test at the narrowest useful layer and integration coverage where subsystem boundaries are crossed.
- Determinism claims are limited to measured fixtures/platforms. Tests must use explicit ticks/seeds; rendering/GPU output is not authoritative simulation state.
- For supported platform claims, require native CI and runtime/package evidence on the exact target pair. Cross-compilation alone is insufficient; see [`support-matrix.md`](support-matrix.md).
- Pull requests state commands run, platforms tested, public-format/API impact, and known limitations. Do not report unrun tests as passing.
