# Scene and resource formats (schema design)

This document specifies schema 1 for scene and resource JSON files. The project manifest and top-level project layout are defined in [`project-format.md`](project-format.md). Parsing and validation implementation follows in Phase 3.3; migrations and asset identity/import behavior follow in 3.4–3.5.

## Common rules

- Files are UTF-8 JSON objects, bounded before parsing. Duplicate keys and unknown fields are errors, not last-write-wins or ignored data.
- Each document has an independent positive integer `schema_version`; scene and resource versions are not coupled to each other, the manifest version, or engine SemVer.
- Persisted scene/entity/resource identities are canonical lowercase hyphenated UUID strings. IDs survive reorder, rename, save, and move. Runtime `EntityId` slot/generation values are ephemeral and must never be serialized as authoring IDs.
- Integers represent authoritative simulation values. No JSON floating-point value is allowed for simulation transforms or ordering.
- Arrays retain authored order as explicit content order. That order may be used for display and serialization, but must not implicitly determine system execution or simulation conflict resolution.
- Project-relative paths are UTF-8, normalized, and confined to the project root after symlink resolution. Absolute paths and escapes are invalid.

## Scene schema 1

Scene files live under `scenes/` and use this shape:

```json
{
  "schema_version": 1,
  "id": "10000000-0000-4000-8000-000000000001",
  "name": "First Room",
  "entities": [
    {
      "id": "20000000-0000-4000-8000-000000000001",
      "name": "Player",
      "parent": null,
      "transform": {
        "translation_milli": [0, 0],
        "rotation_units": 0,
        "scale_milli": [1000, 1000]
      },
      "components": []
    }
  ]
}
```

- Scene `id` and `name` are required. Scene IDs are unique within a project. Names are non-empty display text and are not paths.
- `entities` is required; an empty array is valid. Array order is authored scene order. Entity IDs are unique within the scene and project; duplicate IDs are errors. `name` is non-empty display text.
- `parent` is either `null` or another entity UUID in the same scene. Missing parents and cycles are validation errors. `null` means the entity is a root.
- `transform` may be omitted and defaults to zero translation, zero rotation, and unit scale. `translation_milli` and `scale_milli` are signed integer pairs in milli-world-units. `rotation_units` is an unsigned integer in `0..=65535`, matching `Angle`'s canonical clockwise 1/65,536-turn convention. Zero scale is allowed as data; systems that cannot operate on it report a typed runtime error rather than changing the serialized value.
- `components` may be omitted and defaults to an empty array. Each component has a stable registered `type` string, positive `schema_version`, and `data` object. The component registry owns type-specific validation/defaults. Unknown component types/versions are errors, not silently dropped. A type declares whether multiple instances are allowed; duplicates of non-repeatable types are errors.
- Component payloads may refer to other project entities or resources by stable UUID. Every reference is validated before a scene becomes runnable.

## Resource descriptor schema 1

A resource descriptor is stored next to its authored/imported asset with a `.mycel.json` suffix. It contains:

```json
{
  "schema_version": 1,
  "id": "30000000-0000-4000-8000-000000000001",
  "kind": "texture",
  "source": "sprites/player.png",
  "import": {}
}
```

- `id` is a stable project-wide UUID. Scene and component references resolve to this ID, not to incidental directory enumeration order.
- `kind` is a lowercase registered importer identifier. Unknown kinds return a diagnostic.
- `source` is a project-relative path to authored input, distinct from the descriptor itself and from generated cache/output. The source must resolve inside the project root.
- `import` is a JSON object validated by the registered importer for `kind`. Unknown settings for a known importer are errors unless that importer schema explicitly declares an extension field.
- Source bytes are content-hashed by the asset pipeline. The UUID expresses project identity; content hashes express change detection and do not replace it.

Descriptor suffix/path naming and source hashing are implemented with Phase 3.5. Validation diagnostics must identify the file and JSON path and must not mutate the project. Generated import products live only in `.mycel/` or `build/`, never over authored sources.

## Ordering, defaults, and evolution

Scene entity and component arrays preserve authored order for editor display and stable round-tripping. Entities are instantiated/referenced by UUID; simulation order comes only from the deterministic schedule and explicit system/component rules. No filesystem traversal order, JSON object key order, or hash-map iteration order is authoritative.

Defaults are applied in memory and are explicit above. Saving a file need not materialize omitted defaults, but it must preserve the distinction between user-authored data and generated defaults where a future migration or editor round-trip could otherwise lose information. Unknown fields are rejected until an explicit extension/preservation policy is added. Migrations must be versioned, transactional, preserve backups, and have fixtures for each supported source version.
