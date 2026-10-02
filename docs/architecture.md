# Hycel architecture (proposed)

## Product contract

Hycel's primary design constraint is that the engine must be operable and diagnosable through a documented interface without a GUI. A GUI is a client of the engine, not the source of truth. Humans and agents use the same project files, validation rules, runtime, and diagnostics.

## Dependency direction

```text
project files / game code / agent clients
                 |
       CLI + editor + agent protocol
                 |
       project, asset, and build services
                 |
       runtime (schedule, scenes, input)
       /             |             |           \
simulation core    physics      renderer       platform
(deterministic)  (replaceable) (replaceable) (window/input/audio)
```

Lower layers never depend on editor UI, agent protocol, or a particular host OS. Avoid global mutable state and implicit filesystem/network access. Platform-specific code stays behind narrow interfaces and is isolated in platform crates.

## Workspace direction

Workspace crate boundaries (split only when boundaries are real; avoid premature micro-crates):

- `hycel-core`: deterministic world/simulation types, fixed-step schedule, stable IDs, math-facing abstractions. No OS, renderer, wall clock, or I/O.
- `hycel-project`: project manifest, versioned scene/resource schemas, optional strict input-binding validation, bounded parsing, structured validation diagnostics, and explicit per-file scene migration/rollback.
- `hycel-assets`: asset identity, import metadata, dependency reports, and content/import fingerprinting. Actual format decoders and transactional import execution remain future work.
- `hycel-input`: versioned strict `input.json` schema with named, stable-ID game actions; backend-independent physical control events; deterministic tick-indexed `InputFrame` mapping; and press/release edges derived from replayable frames. It depends on the platform-free core; `hycel-project` validates the optional project file and `hycel-platform` maps native events to its owned controls.
- `hycel-physics`: Hycel-owned fixed-point box/body/contact APIs over Rapier2D. It steps serially at an explicit fixed tick rate, bounds body/coordinate inputs, and returns contact transitions sorted by stable Hycel body IDs. Rapier and floating-point details do not leak into `hycel-core`; cross-platform determinism is not claimed until integrated-game replay tests prove it.
- `hycel-runtime`: game lifecycle, scenes, input frames, event/schedule orchestration.
- `hycel-render`: 2D renderer behind a backend boundary; ADR 0001 provisionally selects `wgpu 30.0.1` for Phase 4. Its initial API owns surface/device/pipeline objects and draws sorted tinted sprites from bounded RGBA uploads, applies camera/viewport transforms, and provides a bounded screen-space bitmap debug-text overlay. Asset decoding, advanced batching, general typography, and device recovery remain future work; see [`rendering.md`](rendering.md).
- `hycel-platform`: window, files, clock, input, and OS integration adapters. Its initial single-window lifecycle wraps provisional `winit 0.31.0-beta.3` with Hycel-owned config/events and opaque `WindowHandle`; it maps a documented physical-key/mouse-button subset into `hycel-input` types while keeping native window/event-loop types private.
- `hycel-cli`: stable human CLI plus versioned JSON output; Phase 3.6 foundations implement `new`, `check`, and `inspect`, while build/test/run/replay/screenshot commands remain later work.
- `hycel-agent`: optional protocol adapters (MCP and/or JSON-RPC stdio) that call the same typed application services as the CLI. Protocol glue must not contain engine logic.
- `hycel-editor`: defer until project format and runtime loop work headlessly. Editor operations must round-trip project files without hidden data loss.

## Determinism boundary

Interactive wall time is sampled only by the host loop. It is converted to fixed simulation ticks; simulation receives explicit tick-indexed input frames. No system time, unseeded randomness, thread scheduling, filesystem iteration order, or GPU result may affect authoritative simulation state.

For release 0.1:

- fixed-step simulation at 60 Hz by default; the host samples monotonic wall time and supplies integer nanoseconds, while simulation code never reads a clock;
- entity IDs include slot generations, component iteration order is deterministic, and 2D authoritative math uses documented integer fixed-point conventions;
- interactive catch-up is capped at eight simulation steps per host frame. Excess whole-step wall-time debt is discarded and reported (simulation tick IDs are never skipped), while the fractional tick remainder is preserved;
- pause is represented by a zero time scale: elapsed host time is not accumulated and the pre-pause fractional remainder is preserved. Slow motion uses an explicit rational scale quantized to Q32.32, not floating-point state;
- deterministic per-system PCG-XSH-RR 64/32 RNG streams are derived from explicit schedule seed/base-stream and stable system ID values, then exposed only through the tick context;
- systems are registered before tick zero, execute serially by `(order, SystemId)`, and cannot be changed once execution starts; tick-indexed input snapshots use ordered numeric action maps;
- simulation callbacks have no parallel execution or shared mutable access: authoritative state changes happen in the serial schedule, while callbacks can only use their own deterministic RNG stream and queue next-tick events. Rendering, asset preparation, and build work may use workers only across immutable inputs/results that are committed at an explicit host boundary; they cannot mutate authoritative state or influence simulation order;
- parallel simulation is deferred until a separate design defines conflict/access declarations, deterministic reductions and event merge order, failure rollback, and cross-platform replay/hash evidence. Thread completion order, host scheduling, and atomics are never valid simulation ordering inputs;
- events emitted in a tick are delivered next tick in `(delivery tick, producer SystemId, emission sequence)` order, with one shared immutable event set per tick;
- a failed system restores the cloned authoritative state, RNG stream, queued events, and tick position. Callbacks must not cause external side effects;
- replay JSON schema 1 records engine version, target triple, tick rate, seed/base RNG stream, and contiguous tick-indexed input frames; unknown/duplicate fields are rejected and input/output sizes are bounded;
- headless playback requires exact replay compatibility metadata and uses the same schedule; the demo verifies playback by comparing versioned SHA-256 hashes of canonical authoritative state;
- no cross-architecture bitwise guarantee is made until explicit cross-target replay/hash fixtures demonstrate it;
- headless execution uses the same simulation schedule as the interactive executable;
- deterministic guarantee is scoped to the same engine version, platform target, and supported game-code subset until cross-platform bitwise tests prove more.

Do not claim universal bitwise determinism across CPU architectures until tested. Rendering and audio are presentation; they do not write authoritative simulation state.

## World primitives and math conventions

`hycel-core` uses slot-index/generation [`EntityId`](../crates/hycel-core/src/world.rs) values: new slots are allocated in increasing index order when no reusable slot exists; otherwise the most recently despawned reusable slot is reused. Generation overflow retires a slot instead of wrapping. Live-entity iteration is in increasing slot order. Typed `ComponentStorage<T>` values are ordered by `EntityId`, validate liveness on insert/query, and can reclaim entries for despawned entities with `retain_alive`; world owners must call that on stores after despawns. This is intentionally a small set of explicit stores, not a reflective/global ECS registry.

Authoritative 2D values use `SimScalar` in milli-world-units, with +X right and +Y down. `Angle` uses 65,536 clockwise units per turn. Fixed-point arithmetic truncates toward zero and reports overflow/division errors instead of saturating. `hycel-physics` explicitly converts bounded core positions, extents, velocities, and gravity to Rapier's internal floats; backend values return through a checked fixed-point quantization boundary. Renderer conversion to floating-point remains presentation-only.

## Project format and API stability

Project files are text-first, UTF-8, schema-versioned, and human diffable. The initial project layout uses a TOML `hycel.toml` manifest and strict JSON scene/resource documents as specified in [`project-format.md`](project-format.md) and [`scene-format.md`](scene-format.md). The `hycel-project` crate bounds parser input, rejects unknown fields, and returns stable diagnostic codes with file/path context. Every serialized format has a version and validation errors include actionable explanation. Migrations are explicit, transactional, and preserve a backup. Unknown fields must not be silently discarded. Runtime/agent commands are versioned separately from file schemas.

Game logic may initially use Rust modules compiled into the game. Do not make dynamic scripting a release blocker. Evaluate a scripting language only after the 2D vertical slice, with sandboxing, deterministic behavior, error diagnostics, and editor tooling as acceptance criteria.

## Renderer/platform decision gate

The planned 1.0 OS floors, Rust triples, and graphics backend mapping are recorded in [`support-matrix.md`](support-matrix.md). They remain planned—not supported—until native CI and actual runtime/package tests pass. Before committing to a graphics abstraction, build a small feasibility spike that opens a window, clears/presents frames, handles resize/key input, and runs on representative OS/architecture pairs; compile it on all declared targets. ADR 0001 provisionally selects `wgpu 30.0.1` + `winit 0.31.0-beta.3` + `pollster 1.0.1` for Phase 4 implementation, using Metal on macOS, Direct3D 12 on Windows, and Vulkan on Linux. The beta API remains behind Hycel-owned interfaces. Before 1.0, replace it with a stable winit release, review dependencies, and validate Windows mixed-DPI behavior on physical hardware; winit issue #4721 and the pre-release status are explicit risks. Phases 4.3–4.4 provide a camera-aware colored/textured quad path, bounded RGBA texture upload, stable sprite layer/order, tint/alpha, screen-space bitmap debug labels, WGSL validation, recoverable surface outcomes, and surfaced asynchronous device failures. CI run 36951787847 validates the minimal path; CI run 36956532269 validates the expanded API, all six native quality/build jobs, and hosted Linux/Vulkan, macOS/Metal, and Windows/D3D12 presentation smokes. Hosted and local feasibility smokes do not constitute support claims. Record backend coverage, shader workflow, packaging, minimum-OS behavior, and failure modes in the ADR. Keep the renderer boundary replaceable; do not expose backend types in game APIs.

## Failure model

- Invalid project content returns structured diagnostics; it does not panic.
- Runtime user/game errors are reported with system, scene, entity, and tick context where available.
- Asset identity and deterministic reimport inputs are implemented; the future import executor must stage outputs transactionally and leave the prior valid cache intact on failure.
- Engine-owned background work has explicit cancellation and bounded resource use.
- Crash reports are opt-in and local by default; no project content is uploaded implicitly.
