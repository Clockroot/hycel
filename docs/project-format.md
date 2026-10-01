# Mycel project format (schema design)

This document establishes the Phase 3 project layout and manifest contract. The concrete example is [`examples/empty-project/mycel.toml`](../examples/empty-project/mycel.toml). Parsing, validation, migrations, and diagnostics are implemented in later Phase 3 subphases; this document is the design source of truth meanwhile.

## Layout

```text
my-game/
├── mycel.toml          # required project manifest
├── src/                # Rust game code
├── assets/             # source textures, audio, fonts, and other imported content
├── scenes/             # versioned, strict JSON scene/resource documents
├── build/              # generated build outputs; safe to regenerate
└── .mycel/             # generated caches and editor/engine state; safe to regenerate
```

`mycel.toml` is the only project-root marker. Source, asset, and scene paths are project-relative, UTF-8, and must remain inside the project root after normalization and symlink resolution. Paths may not be absolute or contain a traversal outside the project. Their actual existence and file types are checked by project validation, not by the manifest parser. `build/` and `.mycel/` are generated directories and should be ignored by version control; project source and authored content are never generated or discarded by the engine.

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
- Content roots are fixed by this schema: `src/`, `assets/`, and `scenes/`; generated output is fixed to `build/` and `.mycel/`. No manifest field can override these paths in schema 1.

Unknown fields, duplicate keys, malformed values, and invalid paths must produce actionable diagnostics; they must not be ignored. Manifest parse limits and diagnostic codes are defined with the validation implementation in Phase 3.3. Project and scene schema versions are independent of the engine's SemVer compatibility range.

## Compatibility and evolution

The TOML manifest and JSON scene/resource choice was selected to balance editable project settings and strict machine-readable content. Scene/resource JSON uses UTF-8, rejects unknown fields unless a later version explicitly defines an extension mechanism, and is bounded before parsing. [`scene-format.md`](scene-format.md) defines the initial scene/resource envelopes, UUID references, integer transforms, defaults, and authored ordering; [`examples/empty-project/scenes/first-room.json`](../examples/empty-project/scenes/first-room.json) is a sample. Both formats are versioned and validated independently. A format change requires fixtures, migration/compatibility tests, and a release note; unknown authored data is never silently discarded.

## Generated and ignored content

Only `build/` and `.mycel/` are engine-generated in schema 1. They may be removed and recreated by engine commands. Authored files under `src/`, `assets/`, and `scenes/` must not be overwritten by generated output. Root `.gitignore` entries should ignore the two generated directories without ignoring the rest of the project.
