# Mycel product scope (1.0)

This document records Mycel's Phase 1.1 product contract. It defines the 1.0 promise, not every game the runtime may eventually be capable of running. Broaden the promise only through an explicit roadmap/scope review backed by implementation and test capacity.

## Product statement

Mycel is a Rust-first, 2D desktop game engine for building, inspecting, testing, and shipping small games. Human developers and AI agents use the same readable project files, runtime services, diagnostics, tests, and build workflow. The engine should make a game change verifiable—not just easy to generate.

## 1.0 target

- **Implementation:** Rust is the primary language for engine, runtime, editor, CLI, and agent-facing services. Keep handwritten non-Rust implementation small and limited to necessary platform bindings, shaders, or build integration; do not make a second language or web runtime a core requirement.
- **Genre explicitly promised:** small, single-player, side-view 2D platformers.
- **Platforms:** macOS, Linux, and Windows on x86-64 and ARM64, but an individual OS/architecture is a supported 1.0 target only after its native CI, runtime smoke tests, and packaging pass. Untested pairs remain experimental.
- **Workflow:** project creation, validation, scene/content editing, run, test, replay, diagnose, and package work through documented interfaces. A visual editor is helpful but not required for headless/agent operation.
- **Agent access:** basic agents can read and edit text project/code files and invoke documented build/test commands. Advanced agents can use bounded, structured inspect/edit/test/replay operations through a stable interface. Agent integrations are optional adapters; no model vendor/account is required.

## Minimum game scale promised

A 1.0 Mycel project should be able to produce a complete, small single-player platform game that has:

- a player character with responsive horizontal movement and jumping;
- a side-view camera and at least one scrolling level/scene;
- solid platforms and collision, plus hazards or simple enemies;
- at least one objective/collectible and a clear success state;
- failure/retry behavior and a basic progress/checkpoint or level-transition mechanic;
- title/start flow, a complete playable loop, and an ending or completion screen;
- enough distinct content to demonstrate reuse of assets/scenes, not just a single static test room.

The promise is about complete game capabilities, not a fixed entity count, art budget, level count, or performance ceiling. The reference/sample game should be short and finishable, with multiple compact stages where feasible. Users may build larger games, but Mycel 1.0 does not promise to scale to arbitrary world sizes or asset counts.

## Out of scope for 1.0

- General-purpose 3D, 3D rendering, or 3D physics.
- Multiplayer, networking, online services, or cloud-required workflows.
- Mobile, console, browser, or embedded targets.
- A universal solution for every 2D genre. Top-down RPGs, fighting games, simulation/strategy games, and complex physics sandboxes are not explicit 1.0 support promises.
- Ragdolls, fluid/soft-body simulation, advanced procedural generation, large streamed worlds, or an all-purpose visual scripting system.
- Feature parity with established engines or a mandatory cloud/model service.

These limits do not forbid experimentation or community extensions. They bound the official compatibility, documentation, sample coverage, and support promise.

## Current proof of direction

`crates/mycel-demo` contains a headless Rust prototype for fixed-tick horizontal movement, a single jump, and landing on a flat floor, with deterministic unit tests. It demonstrates that the selected genre has a concrete code path in the current workspace; it does **not** fulfill the 1.0 game-scale promise, establish a stable gameplay API, or replace the planned general collision/physics, renderer, scenes, assets, editor, and agent tools.

## Product principles

1. **Rust-first, portable by design.** Platform-specific code stays at narrow boundaries; all target support is verified on native runners/devices.
2. **Text is a first-class interface.** Project intent and game logic should be reviewable in version control; the editor must not hide authoritative state.
3. **Determinism is evidence-based.** Explicit ticks, seeded randomness, and replayable input support reliable testing; cross-architecture guarantees are claimed only when proven.
4. **Agents use safe public surfaces.** Read-only inspection is the default; changes are scoped, validated, visible, and recoverable.
5. **Small complete games before broad features.** A polished tested platformer vertical slice outranks unintegrated engine subsystems.
