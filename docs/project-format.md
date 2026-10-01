# Hycel project format (schema design)

This document specifies the project layout and manifest contract implemented by `hycel-project`. The concrete example is [`examples/empty-project/hycel.toml`](../examples/empty-project/hycel.toml). The manifest is schema 1; scene documents have an independent schema version and a schema-1-to-2 migration described in [`scene-format.md`](scene-format.md).

## Layout

```text
my-game/
├── hycel.toml          # required project manifest
├── src/                # Rust game code
├── assets/             # source textures, audio, fonts, and other imported content
├── scenes/             # versioned, strict JSON scene/resource documents
├── build/              # generated build outputs; safe to regenerate
└── .hycel/             # generated caches and editor/engine state; safe to regenerate
```

`hycel.toml` is the only project-root marker. Source, asset, and scene paths are project-relative, UTF-8, and must resolve inside the project root after normalization and symlink resolution. Paths may not be absolute or contain a traversal outside the project. Their actual existence and file types are checked by project validation, not by the manifest parser. Containment checks are point-in-time; file-opening callers must prevent concurrent project path-component changes and must not use these checks alone as a sandbox against a hostile concurrent process. `build/` and `.hycel/` are generated directories and should be ignored by version control; project source and authored content are never generated or discarded by the engine.

The first layout uses fixed directory names rather than user-configurable roots. This keeps tooling predictable and avoids allowing project files to redirect writes to arbitrary locations. A later ADR is required before making roots configurable.

## Manifest schema 1

The manifest is UTF-8 TOML and has these top-level tables and fields:

```toml
format_version = 1

[project]
id = "00000000-0000-4000-8000-000000000001"
name = "My Game"

[engine]
min_version = "0.1.0"
max_version_exclusive = "0.2.0"

[build]
default_profile = "development"

[build.profiles.development]
optimization = 0
debug_info = true

[build.profiles.release]
optimization = 3
debug_info = false
```

- `format_version` is a positive integer selecting the manifest schema. Readers reject unsupported versions; migrations are explicit and never silently rewrite files.
- `project.id` is a stable UUID string, generated once and retained across renames and moves. `project.name` is a non-empty display name; it is not used to construct filesystem paths.
- `engine.min_version` is an inclusive SemVer lower bound and `engine.max_version_exclusive` is an exclusive SemVer upper bound. Both are required, valid SemVer values, and the lower bound must be less than the upper bound. The engine version must satisfy `min_version <= engine < max_version_exclusive`.
- `build.default_profile` names a profile present in `build.profiles`. Profile identifiers use lowercase ASCII letters, digits, and hyphens, beginning with a letter. `optimization` is an integer from 0 through 3; `debug_info` is a boolean. The built-in profile names `development` and `release` have no special parser semantics; the example merely provides conventional defaults.
- Content roots are fixed by this schema: `src/`, `assets/`, and `scenes/`; generated output is fixed to `build/` and `.hycel/`. No manifest field can override these paths in schema 1.

Unknown fields, duplicate keys, malformed values, and invalid paths must produce actionable diagnostics; they must not be ignored. Manifest parsing uses a 64 KiB input bound; scene/resource documents use an 8 MiB bound. `hycel-project` returns stable `HYCEL-*` diagnostic codes with file and field/JSON-path context. Project and scene schema versions are independent of the engine's SemVer compatibility range.

## Dependency review

Direct dependencies in `hycel-project` include `toml` (manifest decoding), `semver` (engine compatibility bounds), `serde_json` (strict scene/resource parsing), and `atomic-write-file` 0.3.1 (cross-platform sibling-temp atomic replacement for migrations/rollback). `atomic-write-file` is BSD-3-Clause, declares Rust 1.85 MSRV (below Hycel's 1.87.0), and documents Unix/Windows/WASI support without exposing unsafe code to Hycel. It uses `rand` and `nix` transitively. Its documented limitations include temporary files after abrupt process termination and lack of non-Unix ACL/ownership/timestamp preservation; scene migrations retain an exact-content backup before replacement. These dependencies require the checked-in lockfile and `cargo deny` review. The new random temporary-name path adds a second `cpufeatures` version through `rand`; TOML also pulls multiple `winnow` versions. `cargo deny` reports both duplicate-version warnings for explicit review; neither is suppressed.

## Compatibility and evolution

The TOML manifest and JSON scene/resource choice was selected to balance editable project settings and strict machine-readable content. Scene/resource JSON uses UTF-8, rejects unknown fields unless a later version explicitly defines an extension mechanism, and is bounded before parsing. [`scene-format.md`](scene-format.md) defines the initial scene/resource envelopes, UUID references, integer transforms, defaults, and authored ordering; [`examples/empty-project/scenes/first-room.json`](../examples/empty-project/scenes/first-room.json) is a sample. Both formats are versioned and validated independently. A format change requires fixtures, migration/compatibility tests, and a release note; unknown authored data is never silently discarded.

## Persisted format and migration coverage

Each persisted envelope has an independent version and unsupported versions fail closed:

| Format | Current schema | Older schemas | Policy |
| --- | ---: | --- | --- |
| `hycel.toml` manifest | 1 | none supported | Strict parse; no rewrite/migration until a real prior schema exists. |
| Scene JSON | 2 | 1 | Explicit 1→2 migration adds empty entity tags; per-file backup, atomic commit, and validated rollback are implemented in `hycel-project`. |
| Resource descriptor JSON | 1 | none supported | Strict parse; no rewrite/migration until a real prior schema exists. |
| Replay JSON | 1 | none supported | Separate deterministic replay format; compatibility is documented in [ADR 0002](adr/0002-replay-format-and-state-hash.md). Unsupported versions fail closed. |
| Component payload | Component-registered | Per component | The component registry accepts only the registered type/version; payload migrations are not yet implemented. |
| Generated import record | 1 | none supported | Strict, fingerprint-verified metadata under `.hycel/`; see [`asset-pipeline.md`](asset-pipeline.md). |
| Asset dependency report | 1 | none supported | Deterministic derived snapshot; direct scene-to-resource dependencies only, not authored source of truth. |

Format versions are not interchangeable: changing a scene does not change the manifest, resource, replay, component, or generated import-record versions. A migration applies only to the named file format and rejects unknown fields rather than dropping them. Current schema design/strict rejection is defined above, in [`scene-format.md`](scene-format.md), and in [`asset-pipeline.md`](asset-pipeline.md).

## Generated and ignored content

Only `build/` and `.hycel/` are engine-generated in schema 1. They may be removed and recreated by engine commands. Authored files under `src/`, `assets/`, and `scenes/` must not be overwritten by generated output. Root `.gitignore` entries should ignore the two generated directories without ignoring the rest of the project.
