# ADR 0002: Replay format and canonical state hashes

- **Status:** Accepted for Phase 2.4.
- **Date:** 2026-10-01
- **Scope:** Bounded tick-input replay capture/playback and deterministic authoritative-state inspection. Not a project/save-file schema.

## Decision

- Persist replay data as compact UTF-8 JSON with `schema_version: 1`, engine version, native target triple, tick rate, seed, base RNG stream, and contiguous tick-indexed input frames. Reject unknown fields, duplicate input action IDs, unsupported schema versions, invalid metadata, and gaps in tick numbers.
- Require exact engine version, target triple, tick rate, seed, and base stream for playback. Schedule system ordering and game code must also match; this replay format does not embed or verify project/game-code/assets.
- Bound JSON input/output to 16 MiB, frames to 250,000, and combined actions to 1,024 per frame. JSON stores data only and is never executed.
- Hash authoritative state using SHA-256 over a versioned, type-tagged, little-endian canonical encoding. State owners implement `CanonicalState`, emitting fields in stable order and collections in deterministic order. Never use `DefaultHasher`, floating-point presentation data, addresses, or hash-map iteration for authoritative hashes.

## Dependency review

Direct dependencies added to `mycel-core` and locked in `Cargo.lock`:

| Crate | Locked version | Purpose/boundary | License | MSRV / native footprint |
|---|---:|---|---|---|
| `serde` | 1.0.229 | Derive stable replay/input serialization contracts | MIT OR Apache-2.0 | Declares Rust 1.56; pure Rust, no native libraries |
| `serde_json` | 1.0.151 | Bounded, human-inspectable replay JSON | MIT OR Apache-2.0 | Declares Rust 1.71; pure Rust, no native libraries |
| `sha2` | 0.10.9 | Stable SHA-256 digest; standard-library hashing is not a versioned cross-build contract | MIT OR Apache-2.0 | No manifest MSRV declaration; compiled/tested on Rust 1.87.0; no required system library |

Alternatives considered: a handwritten JSON parser/serializer was rejected as unnecessary format-parsing risk; Rust's default hashers were rejected because their algorithm/output is not a persistence contract; a hand-rolled SHA-256 was rejected in favor of an established implementation. All dependencies are sourced from crates.io, covered by the checked-in lockfile, and use maintained permissive licenses. `cargo deny check` passes advisory, ban, license, and source checks; the full workspace test suite passes on the declared Rust 1.87.0 MSRV. The native six-target CI matrix is the final target-coverage gate for this addition.

## Consequences and limitations

- Replays are intended for same-version/same-target deterministic debugging and tests, not as a permanent cross-version save format.
- SHA-256 gives a compact comparison digest, not proof that a caller's custom `CanonicalState` implementation includes every authoritative field. Implementations are code-reviewed and fixture-tested.
- Cross-architecture bitwise simulation identity is not claimed. Compare repeated hashes within each supported target; make broader claims only after explicit cross-target fixtures pass.
- Revisit compatibility rules before public replay stability or 1.0; project identity and game-code compatibility belong to the future project-format layer.
