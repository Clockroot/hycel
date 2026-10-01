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

- fixed-step simulation at 60 Hz by default; the host samples monotonic wall time and supplies integer nanoseconds, while simulation code never reads a clock;
- entity IDs include slot generations, component iteration order is deterministic, and 2D authoritative math uses documented integer fixed-point conventions;
- interactive catch-up is capped at eight simulation steps per host frame. Excess whole-step wall-time debt is discarded and reported (simulation tick IDs are never skipped), while the fractional tick remainder is preserved;
- pause is represented by a zero time scale: elapsed host time is not accumulated and the pre-pause fractional remainder is preserved. Slow motion uses an explicit rational scale quantized to Q32.32, not floating-point state;
- deterministic per-system PCG-XSH-RR 64/32 RNG streams are derived from explicit schedule seed/base-stream and stable system ID values, then exposed only through the tick context;
- systems are registered before tick zero, execute serially by `(order, SystemId)`, and cannot be changed once execution starts; tick-indexed input snapshots use ordered numeric action maps;
- events emitted in a tick are delivered next tick in `(delivery tick, producer SystemId, emission sequence)` order, with one shared immutable event set per tick;
- a failed system restores the cloned authoritative state, RNG stream, queued events, and tick position. Callbacks must not cause external side effects;
- replay format includes schema version, seed, tick-indexed inputs, and compatibility metadata;
- headless execution uses the same simulation schedule as the interactive executable;
- deterministic guarantee is scoped to the same engine version, platform target, and supported game-code subset until cross-platform bitwise tests prove more.

Do not claim universal bitwise determinism across CPU architectures until tested. Rendering and audio are presentation; they do not write authoritative simulation state.

## World primitives and math conventions

`mycel-core` uses slot-index/generation [`EntityId`](../crates/mycel-core/src/world.rs) values: new slots are allocated in increasing index order when no reusable slot exists; otherwise the most recently despawned reusable slot is reused. Generation overflow retires a slot instead of wrapping. Live-entity iteration is in increasing slot order. Typed `ComponentStorage<T>` values are ordered by `EntityId`, validate liveness on insert/query, and can reclaim entries for despawned entities with `retain_alive`; world owners must call that on stores after despawns. This is intentionally a small set of explicit stores, not a reflective/global ECS registry.

Authoritative 2D values use `SimScalar` in milli-world-units, with +X right and +Y down. `Angle` uses 65,536 clockwise units per turn. Fixed-point arithmetic truncates toward zero and reports overflow/division errors instead of saturating. Transform conversion to backend floats belongs to the renderer/presentation boundary.

## Project format and API stability

Project files are text-first, UTF-8, schema-versioned, and human diffable. Every serialized format has a version and validation errors include file, path, and actionable explanation. Migrations are explicit, transactional, and preserve a backup. Unknown fields must not be silently discarded. Runtime/agent commands are versioned separately from file schemas.

Game logic may initially use Rust modules compiled into the game. Do not make dynamic scripting a release blocker. Evaluate a scripting language only after the 2D vertical slice, with sandboxing, deterministic behavior, error diagnostics, and editor tooling as acceptance criteria.

## Renderer/platform decision gate

The planned 1.0 OS floors, Rust triples, and graphics backend mapping are recorded in [`support-matrix.md`](support-matrix.md). They remain planned—not supported—until native CI and actual runtime/package tests pass. Before committing to a graphics abstraction, build a small feasibility spike that opens a window, clears/presents frames, handles resize/key input, and runs on representative OS/architecture pairs; compile it on all declared targets. The accepted Phase 4 candidate from ADR 0001 is Rust `wgpu` + `winit`, using Metal on macOS, Direct3D 12 on Windows, and Vulkan on Linux; the isolated probe pins versions, which must be revalidated in Phase 4.1 before they become engine dependencies. Phase 4.3 must extend the selected backend with a textured sprite before the renderer is considered functional. Record backend coverage, shader workflow, packaging, minimum-OS behavior, and failure modes in an ADR. Keep the renderer boundary replaceable; do not expose backend types in game APIs.

## Failure model

- Invalid project content returns structured diagnostics; it does not panic.
- Runtime user/game errors are reported with system, scene, entity, and tick context where available.
- Asset imports are content-addressed and transactional; interrupted imports leave the prior valid asset intact.
- Engine-owned background work has explicit cancellation and bounded resource use.
- Crash reports are opt-in and local by default; no project content is uploaded implicitly.
