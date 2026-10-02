# Phase 4.1 winit beta probe

Isolated feasibility probe for provisional `wgpu 30.0.1` + `winit 0.31.0-beta.3` + `pollster 1.0.1`. It opens a native window, clears/presents, handles resize/close, and maps Space/Escape. It exists to revalidate dependency/platform behavior; it is not a product crate or a Windows mixed-DPI certification.

Run from the repository root:

```sh
cargo fmt --manifest-path spikes/phase-4.1-winit-beta/Cargo.toml -- --check
cargo clippy --manifest-path spikes/phase-4.1-winit-beta/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path spikes/phase-4.1-winit-beta/Cargo.toml --all-targets --locked
cargo build --manifest-path spikes/phase-4.1-winit-beta/Cargo.toml --release --locked
cargo run --manifest-path spikes/phase-4.1-winit-beta/Cargo.toml --release --locked -- --smoke
cargo deny --manifest-path spikes/phase-4.1-winit-beta/Cargo.toml check
```

The smoke runs for three seconds and reports the selected adapter/backend and presented-frame count. On Linux CI it is run under Xvfb with Mesa Vulkan. Native builds, tests, representative runtime smokes, and dependency inventories are wired into `.github/workflows/ci.yml`. Rust 1.87.0 is the declared MSRV. See [ADR 0001](../../docs/adr/0001-windowing-and-renderer.md) for the provisional decision, target evidence, and pre-1.0 validation requirements.
