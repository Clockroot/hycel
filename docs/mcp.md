# Hycel MCP stdio adapter

`hycel-mcp` is a small protocol adapter over the public `hycel-cli` project services. It implements the MCP **2025-11-25** `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call` surface over newline-delimited JSON-RPC on stdin/stdout. It deliberately does not expose resources, prompts, HTTP, shell commands, arbitrary filesystem tools, or project-script execution.

## Start

```sh
cargo run -p hycel-mcp -- --project-root ./examples/platformer-game
```

The process canonicalizes and validates one root at startup, then pins that root for its lifetime. This is a best-effort path boundary, not an OS security sandbox: pathname checks and later file opens are not race-resistant against a concurrent process replacing project directories or files. Callers must prevent concurrent external tree mutation during tool calls and use OS-level sandboxing for hostile projects. Stdout contains only protocol messages; startup/runtime diagnostics go to stderr. The default server exposes only read-only project checks, inspection, named headless tests, replay of a project-relative file, and typed scene/resource-edit previews.

Writing tools require an explicit process-start option:

```sh
cargo run -p hycel-mcp -- --project-root ./examples/platformer-game --allow-writes
```

This additionally exposes `scene_edit_apply`, `resource_edit_apply`, and `scene_screenshot`. Tool metadata marks writes, but MCP annotations are hints, not enforcement; clients should still obtain user confirmation. A preview returns a bounded, single-use, process-local token bound to the scene/resource path, canonical operation, source hash, and candidate hash. The token enforces that a matching preview occurred in this server process; it is not a secret or independent authorization mechanism. Apply consumes that token and revalidates the same preview and source. Existing scene/resource edits create a non-overwriting backup below `.hycel/backups/`, preserve file permissions, and atomically replace the source; scene creation publishes a new validated file without overwriting and has no prior bytes to back up. Source-hash checks are not filesystem compare-and-swap; clients must serialize edits and avoid concurrent external project-tree mutation while an edit is being applied. SVG output paths must remain below the pinned root, their parent directory must already exist, and existing files are never replaced.

## Tools

| Tool | Capability |
| --- | --- |
| `project_check` | Strictly validate the pinned project. |
| `project_inspect` | Read-only paginated scene/resource/dependency summary (`offset`, `limit`). |
| `project_tests` | Run all applicable fixed headless reference-game scenarios; the Bellglass Courier adds `echo-flight`. |
| `project_replay` | Replay a project-relative input file with bounded size and exact runtime metadata. |
| `scene_edit_preview` | Preview typed scene create/backup-restore/rename/transform/entity-create/delete/registered-component-upsert/removal changes and a bounded diff. |
| `scene_edit_apply` | Opt-in scene mutation using a single-use token for the exact prior preview; preserves original bytes where present. |
| `resource_edit_preview` | Preview a typed resource descriptor source-path change and bounded diff. |
| `resource_edit_apply` | Opt-in resource descriptor mutation using its single-use preview token; creates a backup. |
| `scene_screenshot` | Opt-in bounded deterministic SVG scene overview (not a live GPU frame). |

Typed operations are the `SceneEditOperation` variants `create_scene`, `restore_scene_backup`, `rename_scene`, `rename_entity`, `set_entity_transform`, `set_entity_tags`, `create_entity`, `delete_entity`, `remove_entity_component`, and `set_entity_component`. `set_entity_tags` replaces an entity's schema-2 tags with at most 64 unique lowercase ASCII identifiers. A `restore_scene_backup` operation takes the original-content SHA-256 from an engine-created `.hycel/backups/` filename; preview validates that exact backup against the current project, and apply backs up the current bytes before replacing the scene. Entity deletion refuses to orphan direct children. Component changes must pass registered schemas and resource-reference checks. Resource editing supports changing a descriptor's project-relative `source` path or restoring an exact hash-addressed resource backup; the source must be in-root and at most 64 MiB, and project/dependency/animation validation runs again. Restore also checks resource identity/settings and preserves the current descriptor in a new backup. Import settings and source payload editing remain unavailable. All CLI calls use the same tested JSON contracts as direct CLI calls.

## Bounds and compatibility

- Protocol messages are capped at 1 MiB; each server session is capped at 10,000 requests. Responses are capped at 1 MiB.
- Project parsing retains the CLI's per-document, aggregate, directory-depth, entry-count, and path-containment limits. Replay files are separately capped by the replay parser. Scene diffs and SVG previews have their own output limits.
- Replay files must resolve inside the pinned project root. The reference game replay maps numeric input IDs 0/1/2 to horizontal move/jump/restart and does not resolve project action names; other IDs are ignored. Screenshot parent directories must already exist inside the root. Scene edits are restricted to validated `scenes/*.json` documents; resource edits are restricted to validated `assets/*.hycel.json` descriptors and only change `source`.
- The adapter makes no network requests, opens no arbitrary project commands, and never invokes Cargo or project-provided source. OS save data remains outside the source project.
- The adapter implements MCP revision `2025-11-25` only. It proposes this revision during initialization; clients proposing a different string receive the server's supported revision and decide whether to continue. It is stdio-only, synchronous, and does not implement protocol task/cancellation features; tool work is constrained through input sizes and fixed scenarios rather than an advertised wall-clock timeout. Do not connect it to untrusted projects without an OS-level process sandbox and an appropriate client confirmation policy.
- Project inspection supports deterministic `offset`/`limit` pages capped at 1000 results. `tools/list` is intentionally small and unpaginated. Unknown parameters and unknown tool arguments are rejected. A project check at startup fails closed.

Validation: `cargo test -p hycel-mcp --locked` exercises lifecycle negotiation, the read-only/write-tool boundary, scene/resource preview-token flows, tool calls, static root-boundary checks, and a separate-process newline-delimited stdio round trip with a fixture client; CLI service tests cover the underlying preview/apply behavior. `tests/mcp-sdk-client` pins the official TypeScript MCP SDK 1.30.0 and checks protocol initialization, read-only tool discovery, inspection, project validation, and the `echo-flight` scenario through its stdio client. A second opt-in-write session mutates only a temporary copy: it previews a scene rename, verifies no preview write, applies the matching single-use token, checks the exact backup, and verifies token replay is rejected. Run it with `npm ci --ignore-scripts --prefix tests/mcp-sdk-client`, `cargo build -p hycel-mcp`, then `npm run --prefix tests/mcp-sdk-client conformance`; CI runs this smoke on Ubuntu. These fixtures do not validate resistance to concurrent filesystem replacement or certify every MCP client, protocol feature, or newer 2026 revision.
