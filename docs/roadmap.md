# Hycel roadmap to 1.0

This roadmap describes the path from the current kernel to a stable first release. It is outcome- and gate-based, not date-based: phases finish when their exit criteria are met, not when a calendar estimate expires. Subphases may overlap only when their dependencies are satisfied. Revisit scope after each phase; do not add features that threaten the 1.0 reliability bar.

## 1.0 destination

Hycel 1.0 will let a developer create, edit, build, run, test, and ship a small 2D desktop game on macOS, Linux, and Windows on x86-64 and ARM64. Its project data and core commands will be understandable to a person or AI agent, and its simulation will be inspectable and replayable. A small sample game will prove the complete workflow.

This does **not** promise a general-purpose 3D engine, consoles/mobile, multiplayer framework, visual scripting, or feature parity with mature commercial engines. 1.0 stability means documented and tested formats/APIs with explicit compatibility policy—not a claim that every imaginable game is supported.

## Phase overview

1. **Project foundation and product contract** — settle scope, support matrix, engineering rules, and decision gates.
2. **Deterministic simulation kernel** — grow the current clock into a tested, headless simulation runtime.
3. **Project, scene, and asset model** — define versioned, readable game data and validation/migration behavior.
4. **Cross-platform 2D runtime** — deliver the window, input, renderer, and first interactive scene across target systems.
5. **Core 2D game capabilities** — provide only the gameplay systems needed for a small complete game.
6. **Agent-native interface** — make human and agent workflows use the same stable, testable application services.
7. **Editor and content workflow** — add a useful visual authoring layer without making it the source of truth.
8. **Complete sample game and user experience** — prove Hycel by building and shipping a real small game with it.
9. **Hardening, compatibility, and release candidate** — test failure modes, compatibility, performance, packaging, and usability.
10. **1.0 release and maintenance readiness** — freeze contracts, publish artifacts, and establish the support process.

---

## Phase 1 — Project foundation and product contract

**Outcome:** Hycel's first-release boundaries are explicit, and the repository can safely grow without premature commitments.

### Subphases

1. **1.1 — Confirm product scope — COMPLETE.** Hycel is Rust-first, 2D, and desktop-focused. 1.0 explicitly promises small single-player side-view platformers at the scale defined in [`product-scope.md`](product-scope.md); general-purpose 3D and networking are out of scope.
2. **1.2 — Make the support matrix executable — COMPLETE.** Exact OS/architecture pairs, minimum OS floors, Rust MSRV/stable policy, and planned native graphics backends are recorded in [`support-matrix.md`](support-matrix.md). Its six native target rows drive CI. The revised MSRV 1.87.0 and six-row matrix passed native quality jobs, MSRV, docs, dependency audit, and renderer-probe builds in [CI run 36815524964](https://github.com/aaf2tbz/hycel/actions/runs/36815524964). Engine/runtime targets remain experimental until minimum-OS and packaging tests pass.
3. **1.3 — Establish engineering contracts — COMPLETE.** [`engineering-contracts.md`](engineering-contracts.md) and linked contributor/security guidance define crate boundaries, workspace unsafe-code prohibition, dependency review/audit, error policy, transactional data safety, local-first privacy, and no telemetry by default. The MSRV is 1.87.0 by explicit user decision to evaluate current wgpu 30 without the unmaintained transitive Metal dependency found in wgpu 26.
4. **1.4 — Set repository hygiene — COMPLETE.** Added issue forms, pull-request template, code of conduct, Keep a Changelog file/release categories, ownership and triage rules, CODEOWNERS, and CI-generated Cargo metadata/license inventory with pinned cargo-deny checks.
5. **1.5 — Run feasibility spikes — COMPLETE.** An isolated wgpu 30.0.1 + winit 0.30.13 probe builds on all six native OS/architecture rows. Runtime clear/present smoke passed on hosted macOS ARM64 (Metal, 99 frames), Linux x86-64 (llvmpipe/Vulkan, 5,181 frames), and Windows x86-64 (Microsoft Basic Render Driver/D3D12, 176 frames); local Apple M4/Metal also passed. Keyboard mapping has unit coverage. The user accepted [`ADR 0001`](adr/0001-windowing-and-renderer.md) as the Phase 4 candidate; these dependencies remain isolated until Phase 4.1 revalidation. Software adapters are CI feasibility only, not an end-user fallback promise.
6. **1.6 — Measure an initial clean build — COMPLETE.** The contributor setup passed on a fresh Ubuntu 24.04 hosted runner. [CI run 36815524964](https://github.com/aaf2tbz/hycel/actions/runs/36815524964) measured Rust 1.87.0 build/test at 0.21/0.25 seconds, stable 1.98.1 build/test at 0.59/0.17 seconds, and demo run at 0.15 seconds. Details and artifact reference are in [`development-baseline.md`](development-baseline.md).

**Exit gate:** Product scope, declared support matrix, security/data principles, and renderer/platform decision are written down. All accepted CI runner labels work in a test workflow. No major technical unknown blocks the 2D vertical slice.

## Phase 2 — Deterministic simulation kernel

**Outcome:** Game logic runs without a window, at explicit fixed ticks, and can be tested independently of the host OS.

### Subphases

1. **2.1 — Specify time and catch-up policy — COMPLETE.** `hycel-core::FixedClock` uses a 60 Hz initial default and host-supplied integer nanoseconds; `TimeScale` uses deterministic Q32.32 ratios, including zero-scale pause; interactive advancement is explicitly capped (default eight steps), reports dropped whole wall-time debt, preserves the fractional remainder, and never skips logical tick IDs. Tests cover rate, drift, pause, slow motion, catch-up, and failure atomicity.
2. **2.2 — Establish world primitives — COMPLETE.** Added generation-checked `EntityId` allocation/reuse, live-entity iteration, typed deterministic `ComponentStorage<T>`, explicit despawn/stale-component cleanup rules, fixed-point `SimScalar`, `Vec2`, `Angle`, and `Transform2D` in `hycel-core`. The APIs are documented in source and [`architecture.md`](architecture.md); tests cover reuse, stale handles, deterministic iteration, cleanup, math conventions, and arithmetic failures.
3. **2.3 — Build the deterministic schedule — COMPLETE.** `hycel-core::Schedule<State, Event>` freezes a serial `(order, SystemId)` system order before tick zero, consumes tick-matched ordered `InputFrame`s, derives independent PCG-XSH-RR 64/32 RNG streams from the seed/base stream/system ID, and delivers events next tick in canonical order. Tick errors restore cloned state, RNG, and pending events; callbacks are contractually forbidden from external side effects. The platformer demo now runs through this schedule.
4. **2.4 — Add replay and state inspection — COMPLETE.** `Replay` stores strict, bounded schema-1 JSON with engine/target/tick-rate/seed/stream metadata and contiguous input frames; playback validates compatibility and runs headlessly through the same schedule. `CanonicalState` supplies a versioned SHA-256 encoder with deterministic world/component/math implementations. The demo records, serializes, reloads, replays, and asserts a matching state hash; same-target determinism is covered, with no cross-architecture guarantee claimed. Dependency and format decisions are in [`ADR 0002`](adr/0002-replay-format-and-state-hash.md). All six native target jobs, including the replay/hash test suite, passed in [CI run 36875349085](https://github.com/aaf2tbz/hycel/actions/runs/36875349085).
5. **2.5 — Specify concurrency boundaries — COMPLETE.** The authoritative simulation schedule is serial and single-threaded; callbacks have isolated deterministic RNG and next-tick event interfaces. Worker threads may prepare immutable presentation/build/asset results, but may not mutate authoritative state or affect simulation ordering. Parallel simulation requires an explicit deterministic conflict/merge/failure model and cross-platform replay/hash evidence; see [`architecture.md`](architecture.md).
6. **2.6 — Add invariants and regression tests — COMPLETE.** Tests cover pause/resume, bounded catch-up, stable IDs, deterministic system/event ordering, replay/hash equality, tick failure rollback, and explicit clock/schedule tick-overflow atomicity. Workspace tests and Clippy, MSRV tests, docs, dependency audits, and all six native targets passed in [CI run 36877007899](https://github.com/aaf2tbz/hycel/actions/runs/36877007899).

**Exit gate:** Headless sample simulation can run, stop, replay, and report state with no window/GPU dependency. Repeated runs produce the same state hash on supported test targets; any cross-architecture guarantee is stated only if demonstrated.

## Phase 3 — Project, scene, and asset model

**Outcome:** A Hycel project is readable, versioned, validated, diffable, and safe to migrate.

### Subphases

1. **3.1 — Define project layout and manifest — COMPLETE.** [`project-format.md`](project-format.md) specifies the TOML manifest and JSON scene/resource contract, stable project identity, inclusive/exclusive engine SemVer compatibility range, fixed `src/`, `assets/`, and `scenes/` content roots, build profiles, generated directories, and path-boundary rules. A representative manifest is in [`examples/empty-project`](../examples/empty-project/hycel.toml).
2. **3.2 — Define scene/resource schemas — COMPLETE.** [`scene-format.md`](scene-format.md) defines bounded UTF-8 JSON scene/resource schema 1, canonical UUID identities distinct from runtime handles, parent/component/resource references, fixed-point transform defaults, authored ordering, and reject-unknown behavior. A sample scene is in [`examples/empty-project/scenes`](../examples/empty-project/scenes/first-room.json). Keep serialization separate from runtime implementation details.
3. **3.3 — Implement strict validation — IMPLEMENTED; SIX-TARGET CI PENDING.** New `hycel-project` APIs bound TOML/JSON parsing, reject unknown/duplicate fields, validate manifest profiles and SemVer ranges, scene UUIDs/transforms/parents/cycles/component registries, project-wide IDs and declared entity/resource references, and project-relative paths including canonical symlink escapes. Diagnostics use stable `HYCEL-*` codes and retain file/path context. Tests pass locally; close after MSRV and six-target CI pass.
4. **3.4 — Implement transactional migration.** Version all persisted formats, preserve backups, write atomically, and test interrupted migrations and rollback. Never silently discard fields or overwrite user files.
5. **3.5 — Establish asset identity and dependency tracking.** Hash source assets, record import settings/tool versions, report dependencies, and make reimport behavior explicit and reproducible.
6. **3.6 — Add project CLI foundations.** Implement `new`, `check`, and `inspect` first; define human-readable and versioned JSON output, stable exit codes, and test fixtures.

**Exit gate:** A clean project can be created, parsed, validated, inspected, migrated between at least two schema versions, and round-tripped without unintentional data loss. Invalid fixtures produce actionable diagnostics.

## Phase 4 — Cross-platform 2D runtime

**Outcome:** The same simple project opens a window, receives input, and draws a scene on every declared target.

### Subphases

1. **4.1 — Finalize platform/rendering ADR.** Select window/input and renderer dependencies after evaluating maintenance, licenses, backend coverage, shader workflow, error recovery, packaging, and ARM64 behavior.
2. **4.2 — Implement lifecycle and windowing.** Create a window, handle resize/focus/close, report platform failures clearly, and keep native types behind Hycel interfaces.
3. **4.3 — Implement a minimal render path.** Clear/present, draw a colored shape and textured quad, handle camera/viewport transforms, and surface shader/device-loss errors without corrupting simulation state.
4. **4.4 — Add 2D presentation basics.** Sprites, texture loading, camera, layers/order, color/alpha, and a basic text/debug overlay sufficient for the sample game.
5. **4.5 — Normalize input.** Convert native keyboard/mouse events to stable tick-indexed action frames; support configurable bindings and focus-loss behavior.
6. **4.6 — Exercise actual devices and runners.** Build and launch a smoke app on each declared OS/architecture; include headless/lavapipe or software paths where practical and a documented manual GPU matrix where CI hardware cannot assert behavior.

**Exit gate:** A minimal interactive scene launches and renders on every claimed target. Backend selection, fallbacks, known limitations, and clean build instructions are documented.

## Phase 5 — Core 2D game capabilities

**Outcome:** The engine supports the mechanics and content needed for the chosen sample game without ballooning into a general engine feature checklist.

### Subphases

1. **5.1 — Choose the gameplay capability set.** Use the sample game's design to determine minimum requirements; document exclusions before implementation.
2. **5.2 — Add collision and physics.** Select or integrate a maintained 2D physics backend, wrap it behind stable engine concepts, and define deterministic stepping/contact ordering. Test tunneling, boundaries, and invalid geometry.
3. **5.3 — Add game input actions and events.** Bind physical controls to named actions; support rebinding and replay; keep raw device events out of gameplay logic.
4. **5.4 — Add animation and scene transitions.** Provide only the animation/state transitions needed by the sample; make scene loading errors and resource dependencies inspectable.
5. **5.5 — Add audio if required by the sample.** Hide device/backend variation, make missing devices non-fatal, and separate audio timing from authoritative simulation.
6. **5.6 — Add save/load only if required.** Version user save data independently from project assets; make writes atomic and migrations testable.
7. **5.7 — Add integration/performance baselines.** Benchmark representative entity counts, draw workload, asset load, and input-to-frame latency; publish baselines, not unsupported performance promises.

**Exit gate:** The sample game's gameplay loop works using documented engine APIs. Each shipped capability has unit/integration tests, diagnostics, and at least one sample use.

## Phase 6 — Agent-native interface

**Outcome:** Agents at basic and advanced capability levels can reliably contribute changes and verify outcomes without privileged editor access.

### Subphases

1. **6.1 — Complete the file-native experience.** Document project structure, engine lifecycle, game-code patterns, schemas, examples, and safe build/test commands for agents that can only read and edit files.
2. **6.2 — Stabilize CLI contracts.** Implement `run`, `test`, `replay`, `screenshot`, and the remaining release commands. Version JSON schemas; define stable diagnostics, exit codes, pagination, limits, and compatibility policy.
3. **6.3 — Add safe resource-level operations.** Expose typed project/scene inspect and validated edit services. Mutations must be scoped, previewable/diffable, atomic, and recoverable; inspection is read-only by default.
4. **6.4 — Add protocol adapters.** Implement MCP only as an adapter over shared application services; keep CLI and test harness first-class. Protocol failure must not affect game execution or project data.
5. **6.5 — Bound agent capabilities.** Enforce project-root path boundaries, input/output limits, timeouts/cancellation, no implicit network, and no project-script execution. Define read-only vs mutating tool groups.
6. **6.6 — Create agent conformance fixtures.** Test simple file-only tasks, CLI tasks, and advanced tool tasks. Require validation, test/replay evidence, clear failure reporting, and unchanged files outside the allowed scope.
7. **6.7 — Test with independent agents.** Try more than one agent/client and model family; revise docs and schemas based on observed failures. Do not make vendor-specific behavior part of the engine contract.

**Exit gate:** A documented basic agent can implement a small game change from files; a tool-using agent can inspect, edit, test, and report proof through the same public interfaces. Fixtures pass without hidden prompting or manual intervention.

## Phase 7 — Editor and content workflow

**Outcome:** Humans can visually author/debug the project, while text/files remain canonical and agents can perform equivalent operations.

### Subphases

1. **7.1 — Identify editor 1.0 must-haves.** Prioritize scene hierarchy, inspector, viewport, play/stop, and diagnostics; explicitly defer cosmetic customization and large-editor features.
2. **7.2 — Build a thin editor shell.** The editor calls the same project/runtime services as CLI and agent tools. Do not duplicate validation, serialization, or game execution logic.
3. **7.3 — Implement safe scene editing.** Select/move/create entities and components; show stable IDs and references; save text files transactionally and make every change visible in diffs.
4. **7.4 — Implement import/asset feedback.** Show import state, dependencies, errors, and reimport consequences; avoid silently rewriting assets or settings.
5. **7.5 — Add inspectable runtime debugging.** Show entity/component state, current tick, input, logs, performance, and replay/screenshot controls; support headless workflows equally.
6. **7.6 — Verify editor round-trip and accessibility.** Open-save-reopen fixtures without data loss; keyboard navigation, scalable UI, usable error messages, and platform behavior are tested.

**Exit gate:** A human can create/edit the sample game in the editor, and the same project can be inspected and tested without launching the editor. Editor round-trip tests preserve unknown and untouched data.

## Phase 8 — Complete sample game and user experience

**Outcome:** Hycel proves its full lifecycle by creating, testing, packaging, and playing a polished small game.

### Subphases

1. **8.1 — Freeze sample-game scope.** Select a compact 2D game that exercises scenes, assets, input, rendering, collisions, audio if included, save/load if included, and test/replay.
2. **8.2 — Build the game entirely with Hycel.** No external-engine runtime or hand-edited build-machine state. Keep game source/assets in-repository or reproducibly fetchable.
3. **8.3 — Add gameplay test hooks.** Named scenarios, input recordings, deterministic assertions, known-good screenshots where stable, and failure artifacts usable by humans and agents.
4. **8.4 — Package and run clean-room builds.** Produce native playable builds for declared target pairs; test install/launch/uninstall on clean environments and verify assets are included.
5. **8.5 — Write the learning path.** Quickstart, first game, project anatomy, editor/CLI alternatives, agent guide, troubleshooting, and target-specific installation instructions.
6. **8.6 — Conduct external usability sessions.** Test with a Rust developer new to Hycel, a game developer new to Rust, and an AI-assisted workflow. Track completion and confusion rather than relying on team intuition.

**Exit gate:** A clean user can follow the guide to run and modify the game; release candidates pass game tests and native packaging across the declared matrix; at least one independent agent completes a documented task.

## Phase 9 — Hardening, compatibility, and release candidate

**Outcome:** Common failures are caught before release, project data is protected, and compatibility promises are credible.

### Subphases

1. **9.1 — Establish compatibility policy.** Define engine versioning, supported project/save schema upgrades, deprecation window, plugin/script policy if applicable, and emergency security fixes.
2. **9.2 — Fuzz and adversarially test parsers/importers.** Cover malformed/oversized files, path traversal, symlinks, decompression limits, interrupted I/O, invalid references, and corrupt resources. Fix panics and unbounded resource use.
3. **9.3 — Harden determinism and replay.** Run golden replay hashes on all native targets; document exact guarantees and known platform-sensitive operations. Fail CI on unexplained divergence.
4. **9.4 — Test failure recovery.** Simulate crashes during saves/import/migration, missing graphics/audio devices, corrupted caches, disk full, permission denial, and process cancellation. Ensure recovery is non-destructive.
5. **9.5 — Test clean builds and dependency supply chain.** Build from locked dependencies; review licenses/advisories; generate SBOM; verify reproducible build steps and artifact provenance to the degree practical.
6. **9.6 — Profile and trim.** Meet declared startup, frame-time, memory, and build-time budgets on representative modest hardware; remove avoidable complexity/dependencies.
7. **9.7 — Run release-candidate period.** Publish at least one RC, collect actionable feedback, publish known issues, and fix all release-blocking data-loss, crash, security, packaging, or compatibility defects.
8. **9.8 — Audit release docs.** Validate all examples/commands on clean machines; ensure every support claim has a test artifact or stated manual verification.

**Exit gate:** No known release blocker remains; clean install, migration, rollback, replay, agent fixtures, packaging, and native CI all pass. RC feedback is triaged and addressed or explicitly deferred.

## Phase 10 — 1.0 release and maintenance readiness

**Outcome:** Hycel 1.0 is installable, understandable, supportable, and has a clear compatibility and maintenance story.

### Subphases

1. **10.1 — Freeze 1.0 contracts.** Review public Rust APIs, CLI/JSON schemas, project formats, save formats, agent operations, and platform guarantees. Mark stable surfaces and record remaining experimental ones.
2. **10.2 — Prepare release artifacts.** Produce signed/notarized packages as appropriate, archives/checksums, SBOM and third-party notices, source archive, version metadata, and verified update/download links.
3. **10.3 — Publish migration and support policy.** Explain semantic versioning, LTS/security policy, project upgrade behavior, supported host matrix, known limitations, and how to report bugs securely.
4. **10.4 — Perform release rehearsal.** From a clean checkout, run the exact tagged build/release process, verify signatures/checksums, install and launch each artifact, and test sample project creation and export.
5. **10.5 — Tag and publish 1.0.** Use a signed tag, changelog, release notes, downloads, documentation, source/license notices, and a clear status statement. Do not publish a target platform whose validation gate failed.
6. **10.6 — Begin maintenance.** Triage bugs, publish patch releases when warranted, keep CI/dependencies current, track compatibility regressions, and maintain a public roadmap without promising unsized features.

**Exit gate:** The tagged source and published artifacts match; downloads verify and launch on every supported pair; documentation and support policy are public; maintainers can reproduce the release.

---

## Cross-phase quality bar

These requirements apply throughout all phases, not just before 1.0:

- CI runs formatting, linting, tests, docs, dependency/ license checks, and native builds on every declared OS/architecture pair.
- No unsafe code in core unless a reviewed, documented exception is justified and isolated.
- Parsers and user data operations return typed errors; malformed user input must not crash the engine.
- Avoid silent data loss: explicit diffs, transactional writes, backups/migrations, and failure injection tests.
- Simulation determinism is carefully scoped and evidence-backed; never promise cross-platform bitwise identity by assumption.
- Agent-facing actions are documented, typed, bounded, least-privilege, observable, and tested with realistic tasks.
- No mandatory network service, account, cloud backend, or telemetry for core use.
- Third-party dependencies are reviewed for maintenance, security, license, platform coverage, and supply-chain risk.

## Current position

The repository has completed **Phase 1, Phase 2, and Phase 3.1–3.2**. The deterministic simulation kernel includes fixed timing, ordered systems/events, replay, canonical state inspection, and documented single-threaded concurrency boundaries; CI run 36877007899 passed all six native target jobs, MSRV, dependency audit, docs, and renderer checks. The initial project, manifest, scene, and resource schema contracts are documented in `docs/project-format.md` and `docs/scene-format.md`, with TOML/JSON fixtures. The new `hycel-project` parser and validator are implemented and locally tested, pending the updated CI run before Phase 3.3 closes. Phase 1.5 accepted `wgpu` + `winit` as the Phase 4 candidate; Phase 4.1 will revalidate exact dependencies before product integration. No renderer, GPU driver, minimum OS installation, or packaged game is certified; all targets remain experimental until later minimum-OS and packaging gates pass.
