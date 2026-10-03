# Hycel

**A small, deterministic 2D game engine designed to be built and tested by people and AI agents.**

Hycel is pre-alpha: its workspace includes a deterministic simulation kernel, early native window/sprite/input paths, project tools, a small two-room platformer vertical slice, and an in-progress original three-room puzzle-platformer, The Bellglass Courier. Both are integration proofs rather than release-quality games, and no gameplay API or project format is stable. The first release is intentionally a narrow, reliable 2D engine—not a broad 3D editor.

## Why Hycel

Game projects should be inspectable and testable without clicking through an editor. Hycel aims to make the same project usable by a person, a script, CI, or an AI agent:

- readable, versionable project and scene files;
- deterministic headless simulation and replayable input;
- stable CLI commands with machine-readable output;
- structured diagnostics that explain what failed and where;
- visual runtime/editor workflows added without making them the only interface.

## Current state

- `hycel-core`: fixed-rate integer simulation clock and deterministic world/schedule/replay primitives; no platform APIs.
- `hycel-project`: bounded TOML/JSON manifest and scene/resource parsing with strict fields, path checks, and stable diagnostics; project formats are still experimental.
- `hycel-assets`: bounded source hashing, versioned import fingerprints/records, deterministic cache/reimport decisions, and scene/resource dependency reports including animation-frame texture edges; format decoders and importer execution are not implemented.
- `hycel-animation`: strict project-authored clip resources, contiguous tick-driven playback, deterministic loop/completion events, and scene transitions applied at explicit tick boundaries; see [`docs/scene-format.md`](docs/scene-format.md).
- `hycel-audio`: best-effort Kira-backed one-shot effects and looping music with bounded encoded inputs, silent fallback, and bounded asynchronous diagnostics; see [ADR 0004](docs/adr/0004-audio-backend.md).
- `hycel-save`: strict, bounded per-user progress saves with atomic replacement, one backup, explicit recovery, and schema-1-to-2 migration for stable collected-item IDs; see [ADR 0005](docs/adr/0005-progress-save.md).
- `hycel-cli`: `new`, `check`, paginated `inspect`, explicit `run`, named headless `test`, target-specific `replay`, authored-scene SVG preview, hash-guarded typed scene edits, and experimental current-host reference-game `build`/USTAR `package`; bounded versioned JSON envelopes and stable exit codes.
- `hycel-editor`: keyboard-first native authoring shell for hierarchy, scene overview, inspector, staged edits, resource dependencies, diagnostics, and reference-game play/stop; see [`docs/editor.md`](docs/editor.md). It is an early authoring prototype, not a certified full editor.
- `hycel-mcp`: opt-in MCP 2025-11-25 stdio adapter pinned to one project root, with read-only tools by default; see [`docs/mcp.md`](docs/mcp.md).
- `hycel-demo`: a headless movement prototype, the documented two-room platformer vertical slice, and The Bellglass Courier's three-room Echo Flight prototype, combining tick-indexed input/echoes, Rapier physics, authored animation/scenes, synthesized chimes, and local progress saves. The release-mode performance harness records repeatable CPU workloads; the new game's frozen scope is in [`docs/game-design.md`](docs/game-design.md).
- `hycel-platform`/`hycel-render`: provisional native window and early 2D sprite backend with bounded RGBA uploads, camera/layer/tint support, and a bitmap debug-text overlay. Image decoding remains outside the renderer; see [`docs/rendering.md`](docs/rendering.md).
- `hycel-input`: strict versioned named keyboard/mouse actions, tick-indexed input frames, and replayable action edges; see [`docs/input.md`](docs/input.md).
- `hycel-physics`: early Rapier2D adapter with fixed-tick box bodies, bounded fixed-point conversion, and sorted contact transitions; see [`docs/adr/0003-physics-backend.md`](docs/adr/0003-physics-backend.md).
- CI: format, lint, test, and build across macOS, Linux, and Windows runners.
- Project and scene schema proposals: [`docs/project-format.md`](docs/project-format.md) and [`docs/scene-format.md`](docs/scene-format.md), with parser implementation in `hycel-project`; asset identity and import contracts are in [`docs/asset-pipeline.md`](docs/asset-pipeline.md).
- Design and release requirements: [`docs/`](docs/), including the [`gameplay capability profile`](docs/gameplay-scope.md), [`physics backend decision`](docs/adr/0003-physics-backend.md), [`progress save decision`](docs/adr/0005-progress-save.md), early [`2D rendering guide`](docs/rendering.md), and [`input binding guide`](docs/input.md).
- Product contract: [`docs/product-scope.md`](docs/product-scope.md).
- Planned OS, CPU, compiler, and GPU-backend matrix: [`docs/support-matrix.md`](docs/support-matrix.md).
- Engineering, data-integrity, privacy, and dependency rules: [`docs/engineering-contracts.md`](docs/engineering-contracts.md).
- Phased path to stable 1.0: [`docs/roadmap.md`](docs/roadmap.md); start with the [quickstart](docs/quickstart.md), follow the [first-game guide](docs/first-game.md), and use [troubleshooting](docs/troubleshooting.md) when needed. Baseline methodology and limits are in [`docs/performance-baselines.md`](docs/performance-baselines.md).

Run the headless demo, project CLI, and windowed vertical slice (from the repository root):

```sh
cargo run -p hycel-demo
cargo run -p hycel-demo --example performance_baseline --release
cargo run -p hycel-demo --example playable_platformer
cargo run -p hycel-cli -- --help
cargo run -p hycel-editor -- ./examples/platformer-game
cargo run -p hycel-cli -- new ./MyGame --name "My Game"
cargo run -p hycel-cli -- check ./MyGame --json
cargo run -p hycel-cli -- run ./examples/platformer-game
cargo run -p hycel-cli -- test ./examples/platformer-game --json
cargo run -p hycel-cli -- run ./examples/bellglass-courier
cargo run -p hycel-cli -- test ./examples/bellglass-courier --json
cargo run -p hycel-cli -- replay ./examples/platformer-game ./run.replay.json --json
cargo run -p hycel-cli -- screenshot ./examples/platformer-game --scene 11000000-0000-4000-8000-000000000001 --output /tmp/room.svg
cargo run -p hycel-cli -- edit ./examples/platformer-game --file scenes/first-room.json --operation-json '{"operation":"rename_scene","name":"First Room"}' --json
cargo test --workspace
```

CLI command and JSON contracts are documented in [`docs/cli.md`](docs/cli.md). The experimental native editor's controls and limits are in [`docs/editor.md`](docs/editor.md).

The Bellglass Courier opens on a title overlay. Press Space to start; hold A/D or the arrow keys to move, tap Space to jump, press E to replay the last 120 simulation ticks as an echo, and press R to respawn/replay. Stand on a violet echo plate to open its gate, then move to the exit while the echo keeps the plate held. Check `examples/bellglass-courier` with `hycel-cli` before editing its strict project data. The editor F5 path continues to launch only its compiled-in reference runtime.

## Status and compatibility

This is pre-alpha and does not yet promise a stable project format, gameplay API, or save compatibility. Target matrix for the first release: macOS, Linux, and Windows on x86-64 and ARM64. Platform support will be declared per release only after native CI and game smoke tests pass. The project is licensed under Apache-2.0; see [`LICENSE`](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), [`MAINTAINERS.md`](MAINTAINERS.md), the [Code of Conduct](CODE_OF_CONDUCT.md), [architecture](docs/architecture.md), [engineering contracts](docs/engineering-contracts.md), and [release 0.1](docs/release-0.1.md). No engine API is considered stable until a later release explicitly says so.
