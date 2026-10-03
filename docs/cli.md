# Hycel project CLI

The `hycel-cli` crate exposes the `hycel` executable and a library `execute` API returning captured output/status values. It implements project creation, validation, paginated read-only inspection, explicit launch and named headless tests for Hycel's compiled-in reference games, target-specific replay, deterministic authored-scene SVG previews, typed scene/resource edit preview/apply, experimental current-host build bundles, and USTAR packaging. `run`/`build` do not compile or execute project-provided code; only the supported authored reference-game project format is currently runnable.

## Commands

```text
hycel new <path> [--name <display-name>] [--json]
hycel check [<project-path>] [--json]
hycel inspect [<project-path>] [--offset <n>] [--limit <1..1000>] [--json]
hycel run <project-path> [--recover-save] [--json]
hycel test <project-path> [--test <scenario>] [--json]
hycel replay <project-path> <replay-file> [--json]
hycel screenshot <project-path> --scene <scene-uuid> --output <file.svg> [--json]
hycel edit <project-path> --file <scenes/file.json> --operation-json <json> [--apply <original-sha256>] [--json]
hycel resource-edit <project-path> --file <assets/file.hycel.json> --operation-json <json> [--apply <original-sha256>] [--json]
hycel build <project-path> --output <new-directory> [--json]
hycel package <bundle-directory> --output <new-file.tar> [--json]
hycel --help [--json]
hycel --version [--json]
```

`check` and `inspect` default to the current directory. `inspect` pages scene, resource, and dependency maps with a shared deterministic `--offset` (default 0) and `--limit` (default 100, maximum 1000); the result reports total counts and `next_offset`. `run` requires a project path and launches only one of the compiled-in reference-game profiles; use `--recover-save` to explicitly restore the last known-good game save after recovery is required. It does not invoke Cargo, a shell, or project-defined code. `test` runs the bounded, named, headless scenarios supported by the selected compiled-in reference-game project; the two-room platformer has five scenarios, while The Bellglass Courier adds `echo-flight` for six total. `--test` selects one (`authored-content`, `fixed-tick-gameplay`, `echo-flight`, `hazard-respawn`, `platform-support`, or `audio-decode`). `replay` reads a bounded core Replay JSON file and plays it through the reference game after checking exact engine version, target triple, tick rate, and the reference profile's zero seed/stream. The reference replay action IDs are fixed: 0 = horizontal move axis, 1 = jump button, 2 = restart button, and 3 = echo button; other actions are ignored. This mapping is not resolved from project `input.json`. Results include a canonical state hash; cross-platform replay identity is not claimed. `screenshot` emits a deterministic, GPU-free SVG overview of one validated authored scene (it is not a live rendered frame); outputs are capped at 16 MiB and existing output files are never overwritten. `edit` accepts typed scene creation, rename/transform/entity-tag replacement/entity-create/delete/registered-component-upsert/removal, and exact `restore_scene_backup` operations. `set_entity_tags` replaces the selected schema-2 entity's tags with at most 64 unique lowercase ASCII identifiers. Scene creation targets a missing `scenes/*.json` path, previews an empty-source hash and complete candidate diff, validates IDs/references against the whole project, then publishes the file without overwriting via a same-directory staged hard link; its receipt has `backup_path: null` because there were no prior bytes. Other edits return a bounded diff and SHA-256 preview by default, and write only when `--apply` supplies the exact source hash. Component upserts are checked against registered engine schemas and known resource references. Edits to existing scenes create a non-overwriting backup under `.hycel/backups/`, preserve file permissions, and atomically replace the validated scene; stale previews and symlink escapes are rejected. To restore an earlier engine backup, pass `{"operation":"restore_scene_backup","backup_sha256":"<64-character original hash>"}`; the preview identifies that exact backup, revalidates the restored scene and project references, and apply preserves the current bytes in a new backup. A later restore is only accepted when that candidate also validates in the current project. `resource-edit` currently supports typed `set_source` and `restore_resource_backup` operations. `set_source` strictly revalidates the descriptor, verifies the selected source exists within the project and is at most 64 MiB, revalidates resource references/dependencies (and animation clips), and uses the same preview-hash, backup, permission-preserving atomic-replacement flow. Restoring a backup verifies its content hash, identity, and importer settings, revalidates the source and project, and backs up the current descriptor before replacement. Importer settings and source payload editing are not supported. Hash/path checks are not resistant to concurrent external filesystem mutation. `build` runs the named reference-game scenarios, then creates a new current-host bundle containing the running Hycel CLI binary, authored `hycel.toml`/`input.json`/`scenes/`/`assets/`/`src/` files, `BUILD.json`, and one launcher. It supports the compiled-in reference-game format (two or three ordered room scenes); project `src/` remains inert data. `.hycel/` caches and per-user save data are excluded. `package` validates that bundle and writes a deterministic USTAR archive without overwriting an existing file. These are experimental current-host artifacts, not clean-room certification or cross-target builds. Build/package path checks reject static symlinks and escapes but are not race-resistant; keep project and bundle trees quiescent during these operations and use OS sandboxing for hostile inputs. Generic projects can be checked/inspected but are not runnable until they have a supported game runtime. `new` uses the destination basename as the display name unless `--name` is supplied. It generates UUIDv4 project/scene IDs, schema-1 `hycel.toml`, an optional `input.json` schema-2 starter with named `jump` and `move_horizontal` actions and Space/A/D/arrow bindings, an empty schema-2 `scenes/first-room.json`, and the required `src/`, `assets/`, and `scenes/` directories. The destination directory must not already exist. Existing files are never overwritten. If creation stops after reserving the destination, the partial directory is preserved and the diagnostic names it; the CLI never recursively removes user data to clean up a failed creation.

`check` and `inspect` are read-only. They validate the manifest and fixed project layout, strictly validate `input.json` if present (bounded independently to 64 KiB), then recursively discover UTF-8 `.json` documents under `scenes/` and `.hycel.json` resource descriptors under `assets/`, in lexical path order. Each document, including discovered animation clip sources, is bounded to 8 MiB before parsing; project discovery is capped at 128 directory levels, 10,000 directory entries per directory, 10,000 directories, 100,000 total entries, 10,000 scene/resource descriptors, and 256 MiB aggregate document bytes. Path containment is checked when files are opened. `inspect` includes stable scene/resource summaries and deterministic scene→resource plus animation→texture dependency edges.

The CLI registers the built-in `hycel.animation` scene component and the `texture`/`animation` resource kinds; unknown component types, resource kinds, and payload fields fail closed. Animation clip source JSON is parsed strictly, and its frame references must resolve to texture-kind resources. The current registry is deliberately small and has no plugin loading. `check` does not verify texture source-file existence, decode asset formats, or run importers. Input binding schema, supported physical controls, focus behavior, and current exclusions are documented in [`input.md`](input.md).

## Output and exit codes

Without `--json`, success goes to stdout and diagnostics go to stderr. `--json` writes exactly one newline-terminated JSON envelope to stdout and leaves stderr empty, including on failure:

```json
{
  "schema_version": 1,
  "command": "check",
  "ok": false,
  "result": null,
  "diagnostics": [
    {
      "code": "HYCEL-SCENE-001",
      "file": "scenes/broken.json",
      "path": "$",
      "message": "invalid JSON or unknown/duplicate field ..."
    }
  ]
}
```

`result` is command-specific on success and `null` on error. Responses are capped at 384 KiB; if a complete response exceeds that limit, the CLI returns `HYCEL-CLI-199` instead of a partial payload. Use `inspect --offset/--limit` to page large indexes. Diagnostic `code` values from `hycel-project` remain stable machine identifiers; human-readable messages may improve. Output arrays and map-derived lists are ordered deterministically. The envelope version changes only when its top-level contract changes; each persisted project format retains its own independent schema version.

| Exit | Meaning |
| ---: | --- |
| 0 | Command succeeded (including help/version). |
| 1 | Project could not be validated/inspected. |
| 2 | Invalid command/arguments or invalid requested project name. |
| 3 | Filesystem, entropy, runtime launch, or command execution failure. |

The `execute` library API accepts arguments excluding the executable name and returns the same exit code, stdout, and stderr as the binary would emit. This makes CLI behavior testable without subprocess-specific fixtures.

## Filesystem and identity notes

Scene and resource descriptor edit persistence directly uses the existing locked `atomic-write-file` 0.3.1 dependency to stage and atomically commit validated edits; it adds no new crate/version, and stays outside deterministic simulation. Project creation gets UUIDv4 bytes from the OS random source, then sets the RFC 4122 version/variant bits. Dependency review for direct `getrandom` 0.4.3: it serves only initial stable-ID generation; it is not consulted by deterministic simulation. The `getrandom` API was chosen over manually invoking OS APIs or adding a UUID abstraction dependency; it is the narrow OS-entropy interface rather than a general RNG. The crate is maintained by the Rand Project Developers at [`rust-random/getrandom`](https://github.com/rust-random/getrandom), is MIT OR Apache-2.0 licensed, and declares Rust 1.85 MSRV (below Hycel's 1.87.0). The exact package was already locked transitively through `atomic-write-file` → `rand`; enabling this direct edge adds no new package/version or optional feature. Default native implementations use OS-provided entropy on Linux, macOS, and Windows without extra system libraries. All six declared native target triples build and exercise UUID creation in CI; `cargo deny check` reports no advisory/license/source failure for it.

Path containment uses `hycel-project` canonical resolution. This is point-in-time validation, not a race-resistant file handle. Callers must prevent concurrent project-tree mutation during checking/inspection, and must ensure exclusive editing while applying a scene operation: source-hash checks detect ordinary stale previews but cannot provide filesystem compare-and-swap semantics against a hostile concurrent writer. See [`engineering-contracts.md`](engineering-contracts.md) and [`project-format.md`](project-format.md).

## Examples

```sh
cargo run -p hycel-cli -- new ./MyGame --name "My Game"
cargo run -p hycel-cli -- check ./MyGame
cargo run -p hycel-cli -- inspect ./MyGame --limit 50 --offset 0 --json
cargo run -p hycel-cli -- run ./examples/platformer-game
cargo run -p hycel-cli -- test ./examples/platformer-game --json
cargo run -p hycel-cli -- replay ./examples/platformer-game ./run.replay.json --json
cargo run -p hycel-cli -- screenshot ./examples/platformer-game --scene 11000000-0000-4000-8000-000000000001 --output /tmp/room.svg
cargo run -p hycel-cli -- edit ./examples/platformer-game --file scenes/first-room.json --operation-json '{"operation":"rename_scene","name":"First Room"}' --json
# Empty-scene creation preview; apply with its reported original_sha256 if approved:
cargo run -p hycel-cli -- edit ./examples/empty-project --file scenes/second-room.json --operation-json '{"operation":"create_scene","scene_id":"11000000-0000-4000-8000-000000000009","name":"Second Room"}' --json
cargo run -p hycel-cli -- resource-edit ./examples/platformer-game --file assets/player-frame-1.hycel.json --operation-json '{"operation":"set_source","source":"assets/player-frame-2.rgba"}' --json
# Restore with the original_sha256 reported by a prior edit receipt:
cargo run -p hycel-cli -- resource-edit ./examples/platformer-game --file assets/player-frame-1.hycel.json --operation-json '{"operation":"restore_resource_backup","backup_sha256":"<64-character-original-hash>"}' --json
cargo run -p hycel-cli -- build ./examples/platformer-game --output /tmp/hycel-platformer
cargo run -p hycel-cli -- package /tmp/hycel-platformer --output /tmp/hycel-platformer.tar
```

The command contract and fixtures are covered in `crates/hycel-cli`, including a native bundle/package integration fixture that validates USTAR, extracts the archive into a clean temporary directory, and runs the bundled project's named diagnostics. See [`agent-interface.md`](agent-interface.md) and [`mcp.md`](mcp.md) for the broader agent workflow.
