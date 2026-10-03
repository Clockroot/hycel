# First game: explore and personalize The Bellglass Courier

This guided exercise makes a fresh project copy with a **new project UUID**, checks it, tests its headless gameplay, and runs the compiled-in reference runtime. It uses no external engine and does not execute project `src/` code.

## 1. Create a working copy

Run these from the repository root. `hycel new` creates a unique game ID and the required empty layout. Copy the authored sample into that new project; the scene/resource UUIDs are valid project-local identifiers, and the project ID remains the newly generated one.

```sh
cargo run -p hycel-cli -- new ./MyBellglass --name "My Bellglass"
cp -R ./examples/bellglass-courier/assets/. ./MyBellglass/assets/
cp -R ./examples/bellglass-courier/scenes/. ./MyBellglass/scenes/
cp ./examples/bellglass-courier/input.json ./MyBellglass/input.json
cargo run -p hycel-cli -- check ./MyBellglass --json
```

If a destination already exists, choose another name; `hycel new` never overwrites it. Keep a copy of the original sample or use version control before authoring changes.

## 2. Understand the project

- `hycel.toml`: display name, generated project UUID, engine range, and build profiles.
- `input.json`: named move, jump, restart, and echo actions with stable numeric replay IDs.
- `scenes/first-room.json`, `middle-room.json`, `last-room.json`: the Mosslight Roofs, Brass Moonworks, and Bellglass Spire. Each describes the moth, platforms, moonpool hazard, checkpoint, chime, echo plate, and exit using strict versioned JSON.
- `assets/*.hycel.json`: stable texture/animation resource descriptors. Their source `.rgba` frames are original 8×8 art; the animation clip refers to textures by resource UUID.
- `src/`: currently inert source data. The reference runtime is compiled into Hycel and does not build or execute files from this directory.

Use `inspect` for stable IDs and dependencies rather than guessing:

```sh
cargo run -p hycel-cli -- inspect ./MyBellglass --offset 0 --limit 50 --json
```

## 3. Change one scene safely

Open the file-backed editor:

```sh
cargo run -p hycel-editor -- ./MyBellglass
```

Use `Tab` to choose a room, `Up`/`Down` to choose an entity, and `I/J/K/L` to stage a small move. Press `P` to inspect the typed operation, source/candidate hashes, and bounded diff; press `R` to discard it. If you want to keep the change, press `S` once to preview and `S` again to apply that exact preview. A successful write gets an engine-created backup. Use `F6` to revalidate, `F7` to run tests, `F8` to export a deterministic scene overview, and `F5` to play the compiled-in reference runtime.

For a command-line alternative, preview a typed operation without changing files:

```sh
cargo run -p hycel-cli -- edit ./MyBellglass \
  --file scenes/first-room.json \
  --operation-json '{"operation":"rename_scene","name":"Mosslight Roofs — My Version"}' \
  --json
```

Apply requires the exact `original_sha256` returned by the preview. For experiments, the editor's preview/discard path is safer than applying directly.

## 4. Test, run, and package

```sh
cargo run -p hycel-cli -- test ./MyBellglass --json
cargo run -p hycel-cli -- test ./MyBellglass --test echo-flight --json
cargo run -p hycel-cli -- run ./MyBellglass
cargo run -p hycel-cli -- build ./MyBellglass --output /tmp/my-bellglass
cargo run -p hycel-cli -- package /tmp/my-bellglass --output /tmp/my-bellglass.tar
```

`test` is headless and reports named outcomes. In the game, Space starts/jumps, A/D or the arrows move, E replays up to 120 past simulation ticks, and R returns to a checkpoint. A ghost echo holds the violet plate; reach the exit before its recorded flight ends. Chimes are saved by stable entity ID in the per-user local save, not in project assets.

Build/package currently means an experimental **current-host reference-game bundle**, not a cross-compiler or certified installer. Check [`support-matrix.md`](support-matrix.md) and [troubleshooting](troubleshooting.md) before treating a machine/target as supported.
