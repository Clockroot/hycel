# Troubleshooting

Start with the structured error from `hycel --json`; the `code`, `file`, and `path` fields are more actionable than a generic failure message. Check [`cli.md`](cli.md) and the project/scene schemas before hand-editing strict data.

## Project validation fails

- Run `cargo run -p hycel-cli -- check <project-path> --json` and fix the first diagnostic's file/path. Unknown fields and unsupported schema versions fail intentionally; do not delete data to make validation pass.
- Inspect the resource/dependency maps with `hycel inspect <project-path> --json`. Resource IDs in scenes and animation frames must refer to descriptors of compatible kinds.
- Scene/resource paths must stay inside the project and avoid symlink escapes. Engine-generated cache/preview data goes under `.hycel/`; authored data remains under `scenes/` and `assets/`.
- Keep project files quiescent while applying edits. Hash/path checks detect ordinary stale previews but are not filesystem compare-and-swap against another process.

## Game test or launch fails

- `hycel test` is headless and is the first check to run. The reference runtime supports the authored two-room platformer and the three-room Bellglass Courier profiles; arbitrary project code is not compiled or executed.
- Replay files are bounded and must match the runtime's engine version, target triple, 60 Hz tick rate, zero seed/stream profile, and fixed reference action IDs. A target mismatch is expected when replaying a file recorded for another OS/architecture; cross-platform identity is not promised.
- Close and reopen after editing project files. If the renderer cannot initialize, check the planned graphics backend and driver in [`support-matrix.md`](support-matrix.md). Hosted software-adapter smokes do not prove a physical device or minimum-OS install.
- Audio is best-effort. A silent fallback or audio diagnostic does not change simulation correctness; run headless tests to isolate gameplay.
- Progress saves are in the user's OS local-data directory, not the project. A corrupt primary/available backup requires explicit `--recover-save`; unsupported future saves and oversized files fail closed. Do not delete or overwrite save data before retaining a copy.

## Editor or window issues

- The editor requires a native window/graphics adapter and is a prototype. Try `cargo run -p hycel-editor -- --smoke <project-path>` to diagnose presentation setup; this 30-present smoke is not usability or accessibility certification.
- If the window opens but a feature is missing, check [`editor.md`](editor.md): live tick debugging, importer execution, mouse gizmos, and arbitrary project-code execution are not implemented.
- Keyboard-only scene changes remain staged until preview/apply; use `R` to discard. A successful edit creates a non-overwriting `.hycel/backups/` file.

## Build and dependency issues

- Use the repository MSRV, Rust 1.87.0: `cargo +1.87.0 test --workspace --all-targets`. For contributions, also run `cargo +1.87.0 fmt --all -- --check`, strict workspace Clippy, and `cargo deny check`.
- `hycel build` and `package` produce only an experimental current-host reference-game bundle. They are not cross-target builds or clean-OS certification. Project `src/` remains inert.
- Check `cargo --version`, target/toolchain availability, graphics SDK/driver setup, and the exact CI target row in [`support-matrix.md`](support-matrix.md). Do not infer support for an OS/architecture from a successful cross-compile.
