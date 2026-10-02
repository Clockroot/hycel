# ADR 0001: Windowing and renderer

- **Status:** Accepted provisionally for Phase 4 implementation; not a 1.0 support or release decision.
- **Date:** 2026-10-01; Phase 4.1 revalidation recorded 2026-10-01.
- **Scope:** Select a development integration path for the Phase 4 cross-platform 2D runtime. Product APIs must be owned by Hycel; backend types must remain private to implementation crates.

## Context

Hycel needs replaceable 2D rendering and desktop window/input integration for macOS, Linux, and Windows on x86-64 and ARM64. Platform/renderer APIs stay out of deterministic `hycel-core`. The declared Rust MSRV is 1.87.0.

Phase 1.5 evaluated `wgpu 30.0.1` with stable `winit 0.30.13`. That line had a reproducible Windows mixed-DPI resizing defect (winit #4600; related upstream work #4341). At the user's direction, Phase 4.1 compared `winit 0.31.0-beta.3` in a second isolated probe at [`../../spikes/phase-4.1-winit-beta`](../../spikes/phase-4.1-winit-beta), keeping `wgpu 30.0.1` and using `pollster 1.0.1` for the synchronous startup adapter. The beta contains the Windows 11+ suggested-rectangle path associated with PR #4341. Source inspection indicates this should avoid the 0.30.x runaway-resize behavior, but it is not a physical mixed-DPI test.

## Candidates

| Candidate | Strengths | Tradeoffs / evidence |
|---|---|---|
| `wgpu 30.0.1` + `winit 0.31.0-beta.3` + `pollster 1.0.1` | Safe Rust-facing renderer/window APIs; planned Metal/D3D12/Vulkan mapping; WGSL; beta's Windows 11+ DPI rectangle change; beta probe formats, tests, lints, MSRV-checks, cross-builds all six targets, and presents on a native Apple M4/Metal host. | `winit` is still a pre-release with a substantial API migration and known Windows issue #4721: custom size requests via `ScaleFactorChanged`'s size writer can be ignored on Windows 11+. No physical Windows mixed-DPI validation. Treat as provisional, isolate behind Hycel APIs, and revalidate at stable 0.31 and before any support claim. |
| `wgpu 30.0.1` + `winit 0.30.13` | Stable line; six-target native CI and hosted Metal/Vulkan/D3D12 clear/present smokes from Phase 1.5; local M4 Metal smoke. | Reproduces the known Windows mixed-DPI resize concern, so not selected as the Phase 4 implementation line. |
| `macroquad` | Higher-level 2D/game-loop APIs; quick path to interactive output. | Bundles more render-loop policy and is less aligned with Hycel owning lifecycle, input, diagnostics, and replaceable subsystem boundaries. Not spiked. |
| SDL3 Rust bindings | Integrated window/input/audio support and long-lived native ecosystem. | Adds native C library/build/linkage and distribution work. Not spiked; reconsider if OS/runtime requirements defeat the Rust adapters. |
| `softbuffer`/CPU framebuffer | Small presentation surface and possible simple 2D software path. | Does not establish the planned accelerated shader/texture path or target backend mappings. Not selected as a fallback. |

This is a feasibility and architecture decision, not a benchmark, legal certification, or support declaration. Exact dependency graphs are reviewed by CI `cargo-deny` and emitted as machine-readable artifacts.

## Decision

1. Provisionally use `wgpu = 30.0.1`, `winit = 0.31.0-beta.3`, and `pollster = 1.0.1` for Phase 4 implementation. Keep window, event-loop, surface, adapter, device, and texture types behind Hycel-owned interfaces; do not expose them through `hycel-core` or game-facing APIs.
2. Keep authoritative simulation independent of window/GPU/device availability. A renderer failure must be surfaced as a diagnostic and must not mutate or corrupt simulation state.
3. Keep `winit 0.31.0-beta.3` provisional. Before a 1.0 dependency freeze, move to a stable winit release, review its migration/API changes and dependency graph, and rerun the target/runtime checks. Track [winit #4721](https://github.com/rust-windowing/winit/issues/4721) and require Windows mixed-DPI behavior to be validated on physical Windows 11 hardware with at least two differently scaled displays. If the issue remains unresolved, record an explicit resolution or constrained behavior for the engine's public resize contract before release.
4. Intended native mapping remains Metal on macOS, Direct3D 12 on Windows, and Vulkan 1.1+ on Linux. OpenGL/CPU rendering is not a 1.0 fallback promise. A missing compatible adapter/driver must produce an actionable error, not a silent backend switch.
5. Retain Rust 1.87.0 MSRV. Do not claim any planned OS/architecture pair is supported until native runtime, minimum-OS, and packaging evidence passes. Hosted virtual/software adapters are feasibility evidence only.

## Phase 4.1 evidence

- The beta probe was ported to the beta's trait-object window APIs, `can_create_surfaces` lifecycle, `SurfaceResized` event, and consuming `run_app` model. Tests and Clippy passed with Rust 1.87.0; the spike also passed `cargo deny` with no advisory, license, or source-policy failures. `cargo deny` reports duplicate-version warnings for `hashbrown` and `syn`, which remain visible for review.
- Rust 1.87.0 `cargo check --locked` passed for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`. Stable Rust cross-checks passed for `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`; stable checks also passed for `x86_64-pc-windows-gnu`. Cross-compilation is not native runtime evidence.
- Native Apple M4/Metal clear/present smoke passed for 181 frames. It validates the beta event/render loop on this host only; it does not validate Windows DPI behavior, Linux runtime, OS minimums, or packaging.
- The beta requires a substantive event/window API port from 0.30.13. That migration is captured in the isolated probe and is not yet a product API commitment.
- Stable 0.30.13 Phase 1.5 evidence: [CI run 36815524964](https://github.com/aaf2tbz/hycel/actions/runs/36815524964) built all six native targets and passed hosted runtime clear/present smokes on Linux x86-64 (Vulkan/llvmpipe), macOS ARM64 (Metal), and Windows x86-64 (D3D12). It is comparison evidence for the stable line, not validation of beta behavior.

## Runtime and packaging considerations

- `wgpu` selects the native graphics API, but still depends on an available OS graphics driver/adapter. No forced CPU/software fallback is promised.
- Linux runtime needs a usable graphical session (Wayland or X11 as configured), a compatible Vulkan loader/driver, and the runtime libraries selected by the packaged build. CI's Xvfb/llvmpipe path is not a deployment prescription. Exact package dependencies and clean-machine launch behavior remain Phase 4.6/8.4 gates.
- macOS uses system frameworks and Metal; Windows uses system APIs and a D3D12-capable driver. Hosted runner success does not prove the selected macOS 14 or Windows 11 24H2 minimum floors.
- WGSL is the initial shader format. Shader validation, surfaced compilation diagnostics, surface/device loss recovery, and resize handling are Phase 4.3 implementation and test work, not yet proven by the clear/present probe.
- `winit 0.31` public lifecycle changes and pre-release status increase migration risk. Keep adaptation at the platform boundary and re-run the full native matrix when changing to a stable release.

## Consequences

- Phase 4 may add renderer/window dependencies only to implementation crates; `hycel-core` remains graphics/window independent.
- Phase 4.2 implements lifecycle/windowing behind Hycel-owned interfaces using the provisional API; Phase 4.3 must draw a textured sprite and test shader/device-loss/resize errors.
- Before 1.0, replace the beta with a stable winit release and rerun all target, runtime, DPI, dependency/license, minimum-OS, and packaging checks. A clean compile alone is insufficient.
- Renderer decisions must be revisited if the backend, MSRV, data boundary, target matrix, or public resize contract changes.
