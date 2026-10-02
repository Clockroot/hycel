# Hycel project CLI (Phase 3.6)

The `hycel-cli` crate exposes the `hycel` executable and a library `execute` API returning captured output/status values. This phase implements project creation, validation, and read-only inspection. It does not implement build, run, test, replay, edit, or importer execution.

## Commands

```text
hycel new <path> [--name <display-name>] [--json]
hycel check [<project-path>] [--json]
hycel inspect [<project-path>] [--json]
hycel --help [--json]
hycel --version [--json]
```

`check` and `inspect` default to the current directory. `new` uses the destination basename as the display name unless `--name` is supplied. It generates UUIDv4 project/scene IDs, schema-1 `hycel.toml`, an optional-schema-1 `input.json` starter with Space-to-button and A/D/arrow-to-axis bindings, an empty schema-2 `scenes/first-room.json`, and the required `src/`, `assets/`, and `scenes/` directories. The destination directory must not already exist. Existing files are never overwritten. If creation stops after reserving the destination, the partial directory is preserved and the diagnostic names it; the CLI never recursively removes user data to clean up a failed creation.

`check` and `inspect` are read-only. They validate the manifest and fixed project layout, strictly validate `input.json` if present (bounded independently to 64 KiB), then recursively discover UTF-8 `.json` documents under `scenes/` and `.hycel.json` resource descriptors under `assets/`, in lexical path order. Each document is bounded to 8 MiB before parsing; project discovery is capped at 128 directory levels, 10,000 directory entries per directory, 10,000 directories, 100,000 total entries, 10,000 scene/resource documents, and 256 MiB aggregate document bytes. Path containment is checked when files are opened. `inspect` includes stable scene/resource summaries and direct dependency edges.

The current engine has no built-in component or resource importer registry and no plugin-registry loading mechanism. The CLI therefore uses empty registries: empty scenes/projects pass; a project containing a component or resource descriptor fails closed with an unregistered type/kind diagnostic. This is an implementation boundary, not a promise that those schemas are unsupported forever. `check` does not verify referenced source-file existence, decode asset formats, or run importers. Input binding schema, supported physical controls, focus behavior, and current exclusions are documented in [`input.md`](input.md).

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

`result` is command-specific on success and `null` on error. Diagnostic `code` values from `hycel-project` remain stable machine identifiers; human-readable messages may improve. Output arrays and map-derived lists are ordered deterministically. The envelope version changes only when its top-level contract changes; each persisted project format retains its own independent schema version.

| Exit | Meaning |
| ---: | --- |
| 0 | Command succeeded (including help/version). |
| 1 | Project could not be validated/inspected. |
| 2 | Invalid command/arguments or invalid requested project name. |
| 3 | Filesystem, entropy, or command execution failure. |

The `execute` library API accepts arguments excluding the executable name and returns the same exit code, stdout, and stderr as the binary would emit. This makes CLI behavior testable without subprocess-specific fixtures.

## Filesystem and identity notes

Project creation gets UUIDv4 bytes from the OS random source, then sets the RFC 4122 version/variant bits. Dependency review for direct `getrandom` 0.4.3: it serves only initial stable-ID generation; it is not consulted by deterministic simulation. The `getrandom` API was chosen over manually invoking OS APIs or adding a UUID abstraction dependency; it is the narrow OS-entropy interface rather than a general RNG. The crate is maintained by the Rand Project Developers at [`rust-random/getrandom`](https://github.com/rust-random/getrandom), is MIT OR Apache-2.0 licensed, and declares Rust 1.85 MSRV (below Hycel's 1.87.0). The exact package was already locked transitively through `atomic-write-file` → `rand`; enabling this direct edge adds no new package/version or optional feature. Default native implementations use OS-provided entropy on Linux, macOS, and Windows without extra system libraries. All six declared native target triples build and exercise UUID creation in CI; `cargo deny check` reports no advisory/license/source failure for it.

Path containment uses `hycel-project` canonical resolution. This is point-in-time validation, not a race-resistant file handle; callers must prevent concurrent project-tree mutation while checking/inspecting. See [`engineering-contracts.md`](engineering-contracts.md) and [`project-format.md`](project-format.md).

## Examples

```sh
cargo run -p hycel-cli -- new ./MyGame --name "My Game"
cargo run -p hycel-cli -- check ./MyGame
cargo run -p hycel-cli -- inspect ./MyGame --json
```

The command contract and fixtures are covered in `crates/hycel-cli`; the roadmap's remaining CLI commands are implemented in later phases. See [`agent-interface.md`](agent-interface.md) for the broader intended agent workflow.
