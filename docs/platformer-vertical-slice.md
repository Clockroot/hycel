# Windowed platformer vertical slice

This small sample composes Hycel's current desktop, input, fixed-tick, physics, animation, audio, project-asset, and progress-save APIs. It is executable evidence for an early integration path, not a stable game template or a release-quality game.

## Run and verify

From the repository root:

```sh
cargo run -p hycel-cli -- check examples/platformer-game --json
cargo test -p hycel-demo --example playable_platformer
cargo run -p hycel-cli -- run examples/platformer-game
```

The window starts on a title overlay. Press **Space** to begin. Hold **A/D** or **Left/Right** to move, tap **Space** to jump, and press **R** to respawn at the active checkpoint. Collect the cyan shards, avoid rose hazards, touch the amber checkpoint, and reach the gold exit. The second room ends the slice; press **R** on the completion screen to replay.

The two strict schema-2 scenes and schema-1 animation clip live under [`examples/platformer-game`](../examples/platformer-game). Scene transform translations use milli-world-units; in this sample, the absolute scale values are physics/render half-extents. The two player-frame files are bounded 8×8 raw RGBA pixels referenced by stable resource UUIDs. The renderer receives the decoded pixels through `RgbaImage`; file decoding remains outside `hycel-render`. The sample validates scene, animation, texture-descriptor, input, and dependency documents at startup. The jump effect and looping two-second chiptune are synthesized as bounded PCM WAV clips in memory.

## Runtime and persistence boundaries

- The window host samples normalized input into 60 Hz tick-indexed frames. Physics and animation advance only at those explicit ticks; audio playback and rendering remain presentation work.
- Rapier collisions supply platform/hazard contact behavior. Checkpoints, chimes, and exits are bounded scene-tag/position triggers in this sample. Collected item UUIDs persist in save schema 2; legacy schema-1 saves migrate with an empty collection.
- Audio startup/playback failure reports a silent fallback; it never changes gameplay state.
- Progress is keyed by the project's stable UUID and written to the per-user OS data directory, not beside the project. A damaged primary is not overwritten. To explicitly restore its one last-known-good backup, restart with `--recover-save`; unsupported future schemas are still refused. If the OS provides no user-data path, the example continues with memory-only progress and displays a warning.
- The content path is resolved from the current directory or one of its ancestors; run the documented command from this checkout/workspace.

The headless reference-game tests exercise authored-document/dependency validation, tick-driven movement/jumping and room transitions, collectible/checkpoint persistence, and WAV construction without opening a native window. The separate [Bellglass Courier](game-design.md) prototype adds tick-indexed Echo Flight puzzles. Hosted CI compiles/tests the example on its declared targets, but this does not replace physical-device input, GPU, audio, packaged-game, or mixed-DPI validation. It does not establish cross-platform replay identity or input-to-photon latency.
