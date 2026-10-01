# Mycel 1.0 support matrix

This is the planned 1.0 target contract selected in Phase 1.2. A row is **not yet a supported product target** merely because it appears here or compiles in CI. It becomes supported only after native CI, graphics/runtime smoke tests, and packaged-game tests pass for that exact OS/architecture pair. Until then, describe it as planned/experimental.

## Planned desktop targets

| OS family | Minimum supported OS | CPU architecture | Rust target triple | GitHub Actions runner | Planned graphics backend | Status |
|---|---|---|---|---|---|---|
| macOS | macOS 14 Sonoma | x86-64 | `x86_64-apple-darwin` | `macos-15-intel` | Metal | Experimental; native headless CI only |
| macOS | macOS 14 Sonoma | ARM64 | `aarch64-apple-darwin` | `macos-15` | Metal | Experimental; native headless CI only |
| Linux | Ubuntu 24.04 LTS baseline; glibc 2.39+ | x86-64 | `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | Vulkan 1.1+ | Experimental; native headless CI only |
| Linux | Ubuntu 24.04 LTS baseline; glibc 2.39+ | ARM64 | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | Vulkan 1.1+ | Experimental; native headless CI only |
| Windows | Windows 11 24H2 (build 26100)+ | x86-64 | `x86_64-pc-windows-msvc` | `windows-2025` (native x64) | Direct3D 12 | Experimental; native headless CI only, not verified on client Windows 11 |
| Windows | Windows 11 24H2 (build 26100)+ | ARM64 | `aarch64-pc-windows-msvc` | `windows-11-arm` | Direct3D 12 | Experimental; native headless CI only |

Linux support is based on the glibc ABI and Vulkan requirement, not an Ubuntu-only restriction. Other distributions may work if they meet the ABI, driver, and dependency requirements, but are not individually certified by the baseline matrix.

## Graphics backend policy

- Candidate graphics abstraction: Rust `wgpu` + `winit`. Phase 1.5 currently evaluates `wgpu 30.0.1` (Rust 1.87.0 MSRV) and `winit 0.30.13` in an isolated, non-product spike; native multi-platform runtime validation and a decision ADR are still pending. These are not yet engine dependencies.
- Intended native mapping: Metal on macOS, Direct3D 12 on Windows, and Vulkan 1.1 or newer on Linux.
- OpenGL/software-renderer fallback is **not** part of the 1.0 promise unless the renderer spike demonstrates a maintainable fallback and it receives its own smoke-test coverage.
- The current headless kernel has no renderer. Consequently, no graphics backend is currently supported; backend names above are planned targets only.
- A machine without a compatible graphics adapter/driver must receive an actionable startup diagnostic. It must not silently switch to an untested backend.

## Compiler and build policy

- Engine/workspace edition: Rust 2024.
- Minimum Supported Rust Version (MSRV): Rust 1.87.0. CI tests this exact minimum using `Cargo.lock` and current stable separately.
- Contributions use stable Rust; update the MSRV only with an explicit compatibility review, a Cargo manifest/CI change, and release notes.
- Target triples in the table are the native build triples for engine tools. End-user exported games do not require users to install Rust.
- Lock dependency versions for repeatable CI and releases. A target is not certified by cross-compilation alone; run tests and eventual runtime/package smoke checks natively.

## CI contract

`.github/workflows/ci.yml` contains the executable six-row OS/architecture matrix. Its runner/target rows are canonical for native headless CI; keep this table in sync in the same change whenever a row changes. Each row must run formatting, Clippy, tests, and a release build for the listed native target. A green headless job proves only that Rust code builds/tests on that runner; it does not prove the graphics backend, minimum OS floor, or packaged game works.

Phase 4/8 must add real runtime and packaging checks. Minimum-OS versions that are newer than the hosted runner's compatibility surface require dedicated manual/device validation and recorded evidence before status can change to supported.

## Source references

- [GitHub-hosted runner labels and architectures](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [Rust 1.87 release](https://blog.rust-lang.org/2025/05/15/Rust-1.87.0.html)
- [`wgpu` supported platforms and backend selection](https://docs.rs/wgpu/latest/wgpu/struct.Backends.html)
