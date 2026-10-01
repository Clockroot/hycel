# ADR 0001: Windowing and renderer candidate

- **Status:** Accepted as the Phase 4 candidate; production dependency adoption remains subject to Phase 4.1 revalidation.
- **Date:** 2026-10-01
- **Scope:** Phase 1.5 feasibility only. This is not a production dependency adoption.

## Context

Mycel needs a replaceable 2D renderer and desktop window/input integration for macOS, Linux, and Windows on x86-64 and ARM64. The existing boundary keeps platform/renderer APIs out of deterministic `mycel-core`. The declared MSRV was explicitly raised from Rust 1.85.1 to 1.87.0 so current `wgpu` can be evaluated without pinning an older release solely to retain the former floor.

The isolated probe under [`../../spikes/phase-1.5-renderer`](../../spikes/phase-1.5-renderer) evaluates exact crate versions `wgpu = 30.0.1` and `winit = 0.30.13`, using Metal on macOS, Direct3D 12 on Windows, and Vulkan on Linux. It opens a native window, clears and presents frames, responds to close/resize, and maps Space/Escape input. It intentionally does not draw a textured sprite; that remains a Phase 4.3 requirement.

## Candidates

| Candidate | Strengths | Tradeoffs / current evidence |
|---|---|---|
| `wgpu 30.0.1` + `winit 0.30.13` | Safe Rust-facing APIs; low-level renderer/window boundaries fit Mycel's layering; intended Metal/DX12/Vulkan mapping; WGSL option; no wgpu 26 `paste` advisory path in this lock graph. | Larger dependency graph; significant API surface and shader/device lifecycle work. Requires Rust 1.87.0. Local Apple M4 Metal smoke passed; native Linux/Windows and six-target compile evidence pending CI. |
| `macroquad` | Higher-level 2D/game-loop APIs; fast path to a first interactive 2D game. | Bundles more runtime/render-loop policy and is less suited to Mycel owning stable lifecycle, input, diagnostics, and replaceable subsystem boundaries. Not spiked. |
| SDL3 Rust bindings | Integrated window/input/audio support and long-lived native ecosystem. | Brings the SDL native C library/build/linkage and distribution footprint into a Rust-first toolchain. Not spiked; more appropriate if later OS/runtime requirements defeat pure-Rust adapters. |
| `softbuffer`/CPU framebuffer | Small presentation surface and possible software path for simple 2D output. | Does not establish the planned accelerated shader/texture path or promised native GPU mappings; potential optional fallback only. Not spiked. |

This is a feasibility comparison, not a benchmark or legal certification. Exact dependency graphs are reviewed by the CI `cargo-deny` job and emitted as machine-readable artifacts.

## Decision

1. Use `winit` for native windows/events and `wgpu` as the renderer candidate; wrap both behind Mycel-owned interfaces.
2. Keep all graphics/platform dependencies out of `mycel-core`; make the simulation authoritative and independent of adapter availability.
3. Retain Rust 1.87.0 MSRV. Do not claim supported graphics targets until native smoke and later packaging/minimum-OS tests pass.
4. The user accepted this direction as the Phase 4 candidate after all six native target builds and representative macOS, Linux, and Windows runtime smokes passed, with dependency/license/advisory checks acceptable.
5. Re-evaluate crate versions, MSRV, licenses, backend health, and API stability at Phase 4.1 before adding product dependencies; the spike's exact pins are evidence, not a long-term pin policy.

## Evidence

- Apple M4 / macOS host: `cargo +1.87.0 clippy`, tests, release build, and `--smoke` succeeded using `wgpu 30.0.1` / Metal; smoke reported adapter `Apple M4`, backend `Metal`, and 168 presented frames in three seconds.
- Input mapping tests cover Space press/release and Escape close behavior. Window close/resize handling is exercised by the native event loop implementation, but physical input automation is not part of this probe.
- `cargo deny` reports no advisory, license, or source failures for the spike; its duplicate-version warnings are retained for review.
- [CI run 36815524964](https://github.com/aaf2tbz/mycel/actions/runs/36815524964): renderer probe formatted, linted, tested, and built on all six native target rows. Representative runtime smokes passed on Linux x86-64 (llvmpipe/Vulkan under Xvfb, 5,181 frames), macOS ARM64 hosted runner (Apple Paravirtual Metal device, 99 frames), and Windows x86-64 hosted runner (Microsoft Basic Render Driver/D3D12, 176 frames). macOS x86-64, Linux ARM64, and Windows ARM64 were build/test validated but not runtime-smoked.
- The same CI run passed the revised Rust 1.87.0 MSRV job, root/spike cargo-deny advisory/license/source checks, docs, and the fresh-runner development setup baseline.
- Hosted software/virtual graphics devices prove API feasibility only; they are not hardware, minimum-OS, packaging, or end-user fallback certification.

## Consequences

- `mycel-core` remains graphics/window independent; new renderer/platform crates own adapters and diagnostics.
- Phase 4.3 must add textured sprite drawing, shader/error handling, and resize/device-loss coverage.
- CI proves only the runner/driver paths it actually exercises. The hosted runner's OS floor does not prove macOS 14, Windows 11 24H2, or packaged application compatibility.
- Renderer changes require ADR updates if the backend, MSRV, data boundary, or target matrix changes.
