# Performance baselines

These are reproducible starting measurements for the current implementation, not product performance promises or 1.0 targets. Report the runner/device, OS, architecture, Rust version, workload size, warmups, sample count, and median/P95 with every comparison. Do not compare results from unlike hardware as if they certify an OS/architecture target.

## CPU-side baseline

Run at the workspace root:

```sh
cargo run -p hycel-demo --example performance_baseline --release --locked
```

The command prints a Markdown report and writes `target/performance-baseline-cpu.md`. CI runs it on each native OS/architecture runner and retains one report artifact per target for 30 days. It measures 100 samples after 10 warmups using process-local wall time:

- **World query + position tick:** visits the deterministic `World`/`ComponentStorage` in entity order and increments each position; setup is excluded.
- **Source read + SHA-256:** invokes the bounded project-source hashing API on generated 4 KiB, 64 KiB, and 1 MiB files. Warmups intentionally make these local file-cache reads; this is not cold-disk throughput or image decoding.
- **Strict scene parse + validation:** parses the authored 577-byte empty-project scene with the animation component schema registered. The bytes and registry are prepared before timing.
- **Synthetic input event → tick snapshot:** applies a pair of synthetic normalized key events and creates one tick-indexed action frame. This is the agreed CPU pipeline proxy. It excludes OS event delivery, a physical input device, rendering/presentation, and input-to-photon latency.

Initial local CPU results (2026-10-03, Apple M4, macOS 27.2, `aarch64-apple-darwin`, rustc 1.96.1, 100 samples / 10 warmups):

| Workload | Size | Median (µs) | P95 (µs) |
|---|---:|---:|---:|
| World query + position tick | 100 entities | 0.25 | 0.29 |
| World query + position tick | 1,000 entities | 2.54 | 2.62 |
| World query + position tick | 10,000 entities | 28.67 | 29.04 |
| Source read + SHA-256 | 4 KiB | 63.92 | 66.83 |
| Source read + SHA-256 | 64 KiB | 272.21 | 279.54 |
| Source read + SHA-256 | 1 MiB | 3,591.46 | 3,648.75 |
| Strict scene parse + validation | 577 bytes | 2.67 | 5.62 |
| Synthetic input event → tick snapshot | 1 frame | 0.12 | 0.17 |

## Draw/presentation workload

The native render smoke additionally submits 100, 1,000, and 5,000 sprites after three warmups and records up to 20 `Presented` `render_scene` wall-time samples (median/P95) to the CI log; skipped outcomes are excluded. Each line reports the observed count and a `complete` flag. Treat any result with fewer than 20 samples—or no-presented-frames—as incomplete evidence. This includes CPU scene preparation, backend submission, and surface presentation. It is **not** isolated CPU/GPU execution time; presentation pacing can dominate. The local Apple M4/Metal run at 1600×900 reported 20 samples for each workload:

| Sprites | Median (µs) | P95 (µs) |
|---:|---:|---:|
| 100 | 16,681.42 | 17,384.54 |
| 1,000 | 16,634.25 | 16,943.33 |
| 5,000 | 16,697.33 | 17,182.38 |

The approximately 16.7 ms floor is consistent with display pacing and must not be interpreted as the renderer's maximum capacity. Hosted render smoke adapters are software/virtual feasibility environments; their samples are diagnostic only and do not certify physical GPU performance, minimum OS versions, packaging, or support. The smoke asserts successful presentation, not pixel-perfect output. Hosted CI run [37094437531](https://github.com/Clockroot/hycel/actions/runs/37094437531) recorded complete 20-sample workloads on the Linux x86-64 llvmpipe/Vulkan adapter, macOS ARM64 Apple Paravirtual/Metal adapter, and Windows x86-64 Microsoft Basic Render Driver/D3D12 adapter:

| Hosted smoke adapter | 100 sprites median/P95 (µs) | 1,000 sprites median/P95 (µs) | 5,000 sprites median/P95 (µs) |
|---|---:|---:|---:|
| Linux x86-64, llvmpipe/Vulkan | 603.54 / 665.28 | 710.19 / 766.20 | 1,728.97 / 1,769.16 |
| macOS ARM64, Apple Paravirtual/Metal | 16,827.50 / 19,357.21 | 16,652.96 / 19,877.92 | 18,287.08 / 22,876.46 |
| Windows x86-64, Microsoft Basic Render Driver/D3D12 | 15,844.20 / 16,096.20 | 31,499.30 / 33,333.40 | 142,493.30 / 163,833.30 |

These are hosted software/virtual adapters, captured from one CI run. They are feasibility/diagnostic samples, not comparisons across device classes or user hardware performance claims. Each CPU-side matrix target also uploaded a Markdown report as a run artifact.

## Interpretation and limits

- Use these measurements to catch large regressions under a repeated, identified workload; rerun multiple times before drawing conclusions from small changes.
- These workloads do not benchmark the full game loop, physics, audio, asset decoding/import, cold storage, or user-perceived input latency.
- No cross-platform performance equality or deterministic frame-time guarantee is claimed.
- Physical-device input-to-photon, physical GPU, minimum-OS, packaged-game, and Windows mixed-DPI validation remain separate pre-1.0 evidence gates.
