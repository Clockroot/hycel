# ADR 0005: Local progress saves

- **Status:** Accepted for Phase 5.6; runtime integration remains experimental.
- **Decision owner:** User selected per-user application data and one backup with explicit recovery.
- **Scope:** Small local single-player progress only; no arbitrary entity/world serialization.

## Context

The reference platformer needs progress that survives process restart, at minimum current stage/checkpoint progress. Saves must be separate from project/source assets, strict, bounded, versioned, validated, and atomically written. The game may be installed in a read-only location. Cloud sync and account/shared save services are excluded.

## Decision

`hycel-save` stores an explicit `GameProgress` record containing the current scene UUID, an optional checkpoint entity UUID, and up to 256 unique completed scene UUIDs. The store binds each file to the stable project/game UUID; the host resolves scene/entity references against authored content before applying the progress. It does not serialize arbitrary components, runtime handles, or physics state.

Save data is strict UTF-8 JSON schema 1, capped at 1 MiB. Unknown and duplicate fields, malformed identifiers, duplicate completion IDs, foreign game IDs, unsupported schema versions, and oversized files fail validation without rewriting the source file. There is no older save schema to migrate; future schema changes require explicit migration code and fixtures. Readers never silently rewrite saves.

`ProgressStore::for_game` uses the current user's local data directory, then `hycel/<game UUID>/progress.json`. It uses `LOCALAPPDATA` (with `USERPROFILE/AppData/Local` fallback) on Windows, `HOME/Library/Application Support` on macOS, and absolute `XDG_DATA_HOME` or `HOME/.local/share` on Linux. Missing or invalid roots produce a typed error. `at_data_root` supports explicit host paths and tests. A game install or project directory is never used as the default save location.

Each save uses a sibling-temp atomic replacement through the already-reviewed `atomic-write-file` dependency. Before replacing an existing primary, the previous valid bytes are atomically written to the single `progress.json.bak` backup. Invalid, foreign, oversized, or unsupported primary/backup data is never overwritten. If the primary is missing but a backup remains, a new save is refused until explicit recovery.

`recover_backup` validates the backup before use. If a bounded primary is corrupt, it first preserves the original bytes in `progress.json.corrupt` using create-new semantics, then atomically restores the backup. It never overwrites an existing corrupt-file preservation copy, never replaces a valid primary, and refuses to downgrade a primary using a newer schema. If the primary is missing, recovery restores the backup directly. Oversized or unreadable primaries fail closed and remain untouched; the valid backup remains available, but the user must manually move the oversized/unreadable primary before recovery. Recovery is never automatic; errors retain stable `HYCEL-SAVE-*` codes.

Each store expects one active writer. It detects content changes between validation and commit, but hosts must still serialize writes from multiple instances/processes. The system does not claim multi-process locking or power-loss durability beyond the atomic-write dependency's documented guarantees.

## Dependency and platform review

The format reuses existing `serde`/`serde_json`; atomic replacement reuses the existing BSD-3-Clause `atomic-write-file` 0.3.1 dependency (Rust 1.85 MSRV, below Hycel's 1.87). A platform-directory crate was considered, but `directories` 6.0.0 pulls `option-ext` under MPL-2.0, outside the user's narrowly approved audio-license exceptions. It was not added. The small OS path selection therefore uses only standard environment variables and validates that selected roots are absolute. No new dependency or license exception is introduced. The lockfile and `cargo deny check` are still run because workspace membership/lock metadata changed.

## Consequences and limitations

- Saves remain local to the current user/device. Project moves do not move progress; cloud, shared, and portable saves are not provided.
- The format stores stage/checkpoint references, not scene snapshots; missing references require a host/game policy and must not silently deserialize as valid gameplay state.
- A bounded corrupt current save remains available alongside an explicit backup recovery. Oversized/unreadable saves require manual relocation before backup recovery; the engine does not delete unbounded data or guess which version to load.
- Save I/O is outside `hycel-core`; callers must invoke it at host/application boundaries, never in an authoritative callback subject to retries or rollback.
- Physical packaged-game path behavior and power-loss/device-failure cases remain release validation work.

## Validation

Unit tests cover strict schema handling, bounds, game-ID/reference validation, atomic write failure behavior, backup rotation, missing/corrupt/future-schema recovery, non-overwrite guarantees, symlink rejection, and stable error codes. Hosted six-target CI is required before marking Phase 5.6 complete.
