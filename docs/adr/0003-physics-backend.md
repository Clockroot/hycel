# ADR 0003: 2D physics backend

- **Status:** Accepted for Phase 5.2; backend adapter remains experimental.
- **Decision date:** 2026-10-02.
- **Decision owner:** User selected Rapier2D behind a Hycel-owned adapter.

## Context

Hycel targets small, single-player side-view platformers. `hycel-core` has fixed-point math and a serial tick-indexed schedule, but no collision solver. Physics must be replaceable, avoid renderer/window types in the core, and expose deterministic contact ordering. The first scope is bounded boxes and platformer collisions, not general 2D/3D physics.

Rapier is a maintained standalone Rust 2D/3D physics library. Its Rust user guide documents same-machine local determinism by default and a cross-platform mode when `enhanced-determinism`, consistent initialization/order, compliant IEEE-754 platforms, and compatible math functions are used. Avian2D is Bevy-ECS-oriented and would couple Hycel to an ECS framework it does not use. A custom solver would be smaller, but would make Hycel responsible for the solver's long-term collision correctness and edge cases.

## Decision

Add `hycel-physics` as an adapter over pinned `rapier2d 0.32.0`; do not add Rapier dependencies or types to `hycel-core`. Enable only `dim2`, `f32`, and `enhanced-determinism`; do not enable Rapier parallelism or SIMD features. This version avoids Rapier 0.36's transitive `nalgebra 0.35` (which requires Rust 1.89) and resolves to `nalgebra 0.34.x` (Rust 1.87 MSRV; the current lockfile's 0.34.2 build passes the exact 1.87 toolchain). Rapier 0.32 declares Rust 1.86 and is Apache-2.0 licensed.

Expose only Hycel-owned identifiers, fixed-point vectors/states, box descriptions, body kinds, configuration, errors, and contact events. The initial adapter:

- accepts 1–1,000 fixed ticks per second (default 60), advances only contiguous tick IDs beginning at zero, and never samples a clock;
- accepts at most 1,024 bodies and bounded coordinate/extent/velocity/gravity values (absolute 10,000,000 milli-world-units);
- supports axis-aligned box colliders and fixed, dynamic, and velocity-driven kinematic bodies; CCD is explicitly available for dynamic bodies;
- quantizes observed Rapier state back into `SimScalar` milli-world-units and rejects invalid/out-of-range values;
- turns active-contact set differences into contact-start/contact-stop transitions sorted by canonical Hycel body-ID pair and kind; and
- can be cloned for enclosing simulation rollback. The clone copies Rapier's simulation structures and resets only the pipeline's disposable work buffers; a regression test compares subsequent body snapshots and contacts after cloning.

## Guarantees and limitations

Rapier's `enhanced-determinism` feature is enabled, but Hycel does **not** claim cross-architecture physics/replay identity. The backend uses floating-point state internally, and end-to-end determinism depends on game initialization/order, all inputs, the pinned backend/compiler, and platform math. Broader guarantees require integrated game replay/hash fixtures on the claimed native target pairs.

This adapter is not a stable 1.0 API. It does not yet provide arbitrary shapes, joints, sensors, collision filtering/material profiles, a standalone character-controller policy, scene/project serialization, a versioned canonical hash of Rapier's internal solver state, or asset/editor integration. Those needs must be evaluated against the reference game before expanding the surface. Physics types remain outside `hycel-core` even if later runtime services integrate them into authoritative simulation.

## Validation gates

Phase 5.2 tests cover contiguous ticks, invalid tick rates/geometry/ranges/body kinds, high-speed CCD against a wall, body removal/contact transitions, stable simultaneous-contact order, and same-process clone-and-step parity. The full workspace stable/MSRV tests, release build, strict Clippy, docs, dependency audit, and local six-target compile checks have passed. The complete hosted six-target native CI passed in [run 36964450531](https://github.com/Clockroot/hycel/actions/runs/36964450531). Hardware support and cross-platform determinism claims remain separate gates.

## References

- [Rapier Rust user guide: determinism](https://rapier.rs/docs/user_guides/rust/determinism/)
- [Rapier 2D crate 0.32.0](https://docs.rs/rapier2d/0.32.0/rapier2d/)
- [Rapier repository](https://github.com/dimforge/rapier)
