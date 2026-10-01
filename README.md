# Hycel

**A small, deterministic 2D game engine designed to be built and tested by people and AI agents.**

Hycel is at the architecture-and-kernel stage. The current workspace contains a tiny platform-independent simulation core and a headless demo; it is not yet a graphical game engine. The first release is intentionally a narrow, reliable 2D engine—not a broad 3D editor.

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
- `hycel-demo`: headless Rust platformer-motion prototype exercising fixed ticks, horizontal movement, jumping, and landing. It is a proof of direction, not a stable physics API or rendered game.
- CI: format, lint, test, and build across macOS, Linux, and Windows runners.
- Project and scene schema proposals: [`docs/project-format.md`](docs/project-format.md) and [`docs/scene-format.md`](docs/scene-format.md), with parser implementation in `hycel-project`.
- Design and release requirements: [`docs/`](docs/).
- Product contract: [`docs/product-scope.md`](docs/product-scope.md).
- Planned OS, CPU, compiler, and GPU-backend matrix: [`docs/support-matrix.md`](docs/support-matrix.md).
- Engineering, data-integrity, privacy, and dependency rules: [`docs/engineering-contracts.md`](docs/engineering-contracts.md).
- Phased path to stable 1.0: [`docs/roadmap.md`](docs/roadmap.md).

Run the current kernel:

```sh
cargo run -p hycel-demo
cargo test --workspace
```

Run it with `cargo run -p hycel-demo`; it reports a repeatable 90-tick platformer movement/jump/landing scenario.

## Status and compatibility

This is pre-alpha and does not yet promise a stable project format, gameplay API, or save compatibility. Target matrix for the first release: macOS, Linux, and Windows on x86-64 and ARM64. Platform support will be declared per release only after native CI and game smoke tests pass. The project is licensed under Apache-2.0; see [`LICENSE`](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), [`MAINTAINERS.md`](MAINTAINERS.md), the [Code of Conduct](CODE_OF_CONDUCT.md), [architecture](docs/architecture.md), [engineering contracts](docs/engineering-contracts.md), and [release 0.1](docs/release-0.1.md). No engine API is considered stable until a later release explicitly says so.
