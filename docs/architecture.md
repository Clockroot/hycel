# Mycel architecture (proposed)

## Product contract

Mycel's primary design constraint is that the engine must be operable and diagnosable through a documented interface without a GUI. A GUI is a client of the engine, not the source of truth. Humans and agents use the same project files, validation rules, runtime, and diagnostics.

## Dependency direction

```text
project files / game code / agent clients
                 |
       CLI + editor + agent protocol
                 |
       project, asset, and build services
                 |
       runtime (schedule, scenes, input)
          /            |             \
 simulation core   renderer       platform
 (deterministic)   (replaceable)  (window/input/audio)
```

Lower layers never depend on editor UI, agent protocol, or a particular host OS. Avoid global mutable state and implicit filesystem/network access. Platform-specific code stays behind narrow interfaces and is isolated in platform crates.

## Workspace direction

Planned crates (split only when boundaries are real; avoid premature micro-crates):

- `mycel-core`: deterministic world/simulation types, fixed-step schedule, stable IDs, math-facing abstractions. No OS, renderer, wall clock, or I/O.
- `mycel-project`: project manifest, versioned scene/resource schemas, validation, migration.
- `mycel-assets`: asset identity, import metadata, dependency graph, content hashing, cache.
- `mycel-runtime`: game lifecycle, scenes, input frames, event/schedule orchestration.
- `mycel-render`: 2D renderer behind a backend boundary; initial candidate `wgpu`, pending a renderer spike and explicit backend decision.
- `mycel-platform`: window, files, clock, input, and OS integration adapters.
- `mycel-cli`: stable human CLI plus versioned JSON output for validation, build, test, run, inspect, and screenshot.
- `mycel-agent`: optional protocol adapters (MCP and/or JSON-RPC stdio) that call the same typed application services as the CLI. Protocol glue must not contain engine logic.
- `mycel-editor`: defer until project format and runtime loop work headlessly. Editor operations must round-trip project files without hidden data loss.

## Determinism boundary

Interactive wall time is sampled only by the host loop. It is converted to fixed simulation ticks; simulation receives explicit tick-indexed input frames. No system time, unseeded randomness, thread scheduling, filesystem iteration order, or GPU result may affect authoritative simulation state.

For release 0.1:

- fixed-step simulation (initial default 60 Hz), bounded catch-up policy owned by runtime;
- deterministic seeded RNG provided explicitly to game code;
- replay format includes schema version, seed, tick-indexed inputs, and compatibility metadata;
- headless execution uses the same simulation schedule as the interactive executable;
- deterministic guarantee is scoped to the same engine version, platform target, and supported game-code subset until cross-platform bitwise tests prove more.

Do not claim universal bitwise determinism across CPU architectures until tested. Rendering and audio are presentation; they do not write authoritative simulation state.

## Project format and API stability

Project files are text-first, UTF-8, schema-versioned, and human diffable. Every serialized format has a version and validation errors include file, path, and actionable explanation. Migrations are explicit, transactional, and preserve a backup. Unknown fields must not be silently discarded. Runtime/agent commands are versioned separately from file schemas.

Game logic may initially use Rust modules compiled into the game. Do not make dynamic scripting a release blocker. Evaluate a scripting language only after the 2D vertical slice, with sandboxing, deterministic behavior, error diagnostics, and editor tooling as acceptance criteria.

## Renderer/platform decision gate

The planned 1.0 OS floors, Rust triples, and graphics backend mapping are recorded in [`support-matrix.md`](support-matrix.md). They remain planned—not supported—until native CI and actual runtime/package tests pass. Before committing to a graphics abstraction, build a small feasibility spike that opens a window, clears/presents frames, handles resize/key input, and runs on representative OS/architecture pairs; compile it on all declared targets. The Phase 1.5 candidate is Rust `wgpu` + `winit`, using Metal on macOS, Direct3D 12 on Windows, and Vulkan on Linux; the isolated probe pins versions and is not yet an engine dependency. Phase 4.3 must extend the selected backend with a textured sprite before the renderer is considered functional. Record backend coverage, shader workflow, packaging, minimum-OS behavior, and failure modes in an ADR. Keep the renderer boundary replaceable; do not expose backend types in game APIs.

## Failure model

- Invalid project content returns structured diagnostics; it does not panic.
- Runtime user/game errors are reported with system, scene, entity, and tick context where available.
- Asset imports are content-addressed and transactional; interrupted imports leave the prior valid asset intact.
- Engine-owned background work has explicit cancellation and bounded resource use.
- Crash reports are opt-in and local by default; no project content is uploaded implicitly.
