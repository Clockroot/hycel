# Development setup and baseline

The supported development baseline is Rust 1.87.0 (MSRV) or newer, Git, and the commands in [`CONTRIBUTING.md`](../CONTRIBUTING.md). Normal kernel development has no native system-library prerequisites. Renderer/platform development may add target-specific prerequisites; those must be documented alongside the selected backend.

## Fresh-runner validation

CI job `Fresh-runner development setup baseline` starts from a new `ubuntu-24.04` hosted runner, checks out the repository, installs the exact MSRV, and runs cold release build and all-target test commands under both the MSRV and current stable Rust. It deliberately does not use the Rust build cache. The `clean-development-baseline-<commit>` workflow artifact stores the runner/toolchain identity and measured wall-clock times for 90 days.

| Runner/toolchain | Release build (wall seconds) | Tests (wall seconds) | CI run / artifact |
|---|---:|---:|---|
| Ubuntu 24.04, Rust 1.87.0 | 0.21 | 0.25 | [CI run 36815524964](https://github.com/aaf2tbz/hycel/actions/runs/36815524964), artifact `clean-development-baseline-cf15e00aafa7e6126466e8e944afc552ff4344ce` |
| Ubuntu 24.04, stable 1.98.1 | 0.59 | 0.17 | [CI run 36815524964](https://github.com/aaf2tbz/hycel/actions/runs/36815524964), same artifact |

The same artifact recorded `cargo +1.87.0 run -p hycel-demo --locked` at 0.15 seconds. The job starts without a Rust build-cache action or prior Hycel artifacts; commands share that job's Cargo target directory, so this is an initial fresh-runner setup baseline, not an isolated benchmark for each toolchain. These are reproducibility/regression baselines, not performance promises. Runner load, hosted hardware, Rust version, and dependency graph can change timings; compare like-for-like runs. Record a new measurement after material dependency/toolchain changes and retain earlier CI artifact links in project history.

## Local commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo run -p hycel-demo --locked
```

For dependency changes, install the pinned `cargo-deny` version shown in [`CONTRIBUTING.md`](../CONTRIBUTING.md) and run `cargo deny check`. Never interpret a successful headless build as proof that a renderer, GPU driver, OS floor, or packaged game works; see [`support-matrix.md`](support-matrix.md).
