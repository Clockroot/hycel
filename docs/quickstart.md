# Hycel quickstart

Hycel is an experimental Rust-first desktop 2D engine. This quickstart runs the compiled-in reference game **The Bellglass Courier** from this checkout; it does not yet turn arbitrary project `src/` code into a game executable.

## 1. Prepare the workspace

Install Rust 1.87.0 or newer with `rustup` and use the repository root as the working directory. The target matrix and graphics requirements are in [`support-matrix.md`](support-matrix.md); all targets remain experimental and hosted smoke results do not certify a physical device.

```sh
rustup toolchain install 1.87.0
cargo +1.87.0 --version
```

A compatible native graphics adapter/driver is required for the window. Audio is best-effort and falls back to silence. Linux needs a usable Vulkan driver/display session for the native window. Windows/macOS mixed-DPI and minimum-OS behavior are not certified.

## 2. Validate and test the sample

```sh
cargo +1.87.0 run -p hycel-cli -- check examples/bellglass-courier --json
cargo +1.87.0 run -p hycel-cli -- inspect examples/bellglass-courier --json
cargo +1.87.0 run -p hycel-cli -- test examples/bellglass-courier --json
```

The headless suite includes authored-content, fixed-tick progression, `echo-flight`, hazard recovery, platform support, and audio decoding. It needs no game window or audio device. See [`game-design.md`](game-design.md) for the rules and implementation boundary.

## 3. Play

```sh
cargo +1.87.0 run -p hycel-cli -- run examples/bellglass-courier
```

Press **Space** to start; hold **A/D** or **Left/Right** to move; tap **Space** to jump; press **E** to replay up to 120 prior simulation ticks as an echo; press **R** to return to the active checkpoint. Use the echo to hold a violet plate while the moth reaches the sealed bellglass gate. The screen reports gate/chime/echo state in text as well as color. Closing the window exits.

Progress is saved in the current user's local-data directory, separate from the project. If a save is corrupt or missing while a backup exists, recovery is explicit; read the diagnostic and pass `--recover-save` only when you intend to restore that backup.

## 4. Explore the files and editor

Start with [`first-game.md`](first-game.md) for a guided project tour and safe edit. `hycel.toml` identifies the game; `input.json` names its controls; `scenes/` contains strict versioned scene JSON; `assets/` contains stable resource descriptors and original raw RGBA frames; `src/` is currently inert project data.

```sh
cargo +1.87.0 run -p hycel-cli -- inspect examples/bellglass-courier --offset 0 --limit 50 --json
cargo +1.87.0 run -p hycel-editor -- examples/bellglass-courier
```

The editor is a keyboard-first prototype, not a complete or accessibility-certified product. Its **F5** action uses the same compiled-in reference runtime; it does not execute project source. See [`editor.md`](editor.md) for the current controls and limits.

## 5. Package a current-host reference build

```sh
cargo +1.87.0 run -p hycel-cli -- build examples/bellglass-courier --output /tmp/bellglass-courier
cargo +1.87.0 run -p hycel-cli -- package /tmp/bellglass-courier --output /tmp/bellglass-courier.tar
```

These commands package the current-host CLI binary, validated project data, and named tests. The bundle is experimental, host-specific, not an installer, and not evidence for another OS/architecture. Project source remains inert. For ordinary workspace validation use `cargo +1.87.0 test --workspace --all-targets`.

## Next references

- [The Bellglass Courier: controls, rooms, and game scope](game-design.md)
- [First-game walkthrough](first-game.md)
- [CLI command contract](cli.md) and [editor prototype](editor.md)
- [Project and scene formats](project-format.md), [asset pipeline](asset-pipeline.md), and [input bindings](input.md)
- [Troubleshooting](troubleshooting.md) and [planned target matrix](support-matrix.md)
