# Release 0.1: first useful release

This is an early, non-stable vertical-slice milestone, not the compatibility promise for 1.0. The full staged path and 1.0 gates are in [`roadmap.md`](roadmap.md).

## Release promise

A developer can create, run, inspect, and test a small 2D game on supported desktop targets; an AI agent can perform the same tasks through documented project files and CLI commands, then provide reproducible evidence. This is a small usable engine, not an editor-complete competitor to Godot/Unity/Unreal.

## Required vertical slice

Ship one polished small single-player side-view platformer created with Hycel itself, aligned with the 1.0 product contract in [`product-scope.md`](product-scope.md). A clean clone must build and launch it without hand-edited machine state.

### Engine capabilities

- Window, resize, frame presentation, keyboard/mouse input.
- 2D sprites/textures, camera, transforms, layers, basic animation.
- Scene/project files with stable IDs, validation, clear diffs, and safe round-trip.
- Basic collision and 2D physics sufficient for the sample game; choose/integrate a maintained backend rather than inventing rigid-body physics.
- Fixed-step simulation, seeded RNG, recorded/replayed input, headless run.
- Asset import with clear errors, content identity, dependency reporting, and no silent overwrite.
- Rust game code and a documented lifecycle; hot reload is optional and only if reliable.
- CLI: `hycel new`, `check`, `run`, `test`, `replay`, `inspect`, `screenshot`, `--version`, `--json`.
- Structured diagnostics: stable code, severity, message, path/span or entity/tick context, remediation hint.
- User-facing sample and quickstart; no requirement that users install the source toolchain to run exported game.

### Agent usability

- Machine-readable `hycel --help --json` and schema-versioned command output.
- `hycel check` is read-only; agent actions are explicit, scoped, and never silently destructive.
- Agent can list project resources, inspect scenes/entities/components, apply validated edits, run tests/replays, and retrieve logs/screenshots through stable commands/services.
- MCP is an adapter, not a requirement: the core agent API remains callable by CLI/tests and does not depend on a particular model/vendor.
- Documented `AGENTS.md`/agent guide with project structure, command recipes, permissions, output schemas, and task examples.
- Golden agent task fixtures test that an agent can make one small change, run validation, and surface failure evidence. Do not claim agent capability from demos alone.

## Explicit non-goals for 0.1

General-purpose 3D, visual scripting, multiplayer/networking, consoles/mobile, plugin marketplace, arbitrary language bindings, advanced animation/particles, full visual editor, and production asset suite. Keep APIs open to these directions, but do not build them before the vertical slice works.

## Exit criteria

1. Sample game clean-builds and plays on macOS, Linux, and Windows for each declared architecture.
2. CI passes formatting, lints, unit/integration tests, docs, and packaging smoke tests.
3. Headless replay produces the same authoritative state hash for supported deterministic targets from the same seed/input recording.
4. Invalid project/scene/asset fixtures fail with stable actionable diagnostics and no panic.
5. Agent fixture completes a task using only documented interfaces and validates the resulting game.
6. Upgrade/migration, data-loss, crash, and clean-install tests pass.
7. Release artifacts, checksums, SBOM/dependency inventory, licenses, and signing strategy are documented.
8. A user can follow the quickstart from a clean machine in under 15 minutes (measure this with external testers).
