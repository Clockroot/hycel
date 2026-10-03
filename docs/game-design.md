# Phase 8 sample game: The Bellglass Courier

## Scope decision (Phase 8.1)

Build **The Bellglass Courier**, a compact, original single-player 2D puzzle-platformer. A moth courier carries three lost chimes through a suspended night city to relight its dawn beacon. The defining mechanic is **Echo Flight**: replay the courier's recent tick-indexed movement as a temporary spectral double. The echo can keep a pressure plate held while the player crosses a gate or reaches a separate route.

The game should take roughly 10–20 minutes to finish on a first playthrough. Its mood is quiet and wondrous rather than combat-focused; no enemies, online features, or procedural levels are required.

## Gameplay contract

- **Controls:** A/D or Left/Right arrows move; Space jumps; E launches Echo; R restarts at the latest checkpoint. Actions use named project bindings while retaining stable numeric replay IDs, and regression tests verify the authored physical bindings match the HUD.
- **Echo puzzle:** retain a bounded history of the courier's tick-indexed positions. Pressing Echo replays up to the last 120 simulation ticks as a derived gameplay actor (not a second physics body); it may activate authored pressure plates. Gates open only while their plate is held. The player can record a route, launch the echo, then take a different path before it expires.
- **Three hand-authored rooms:** Mosslight Roofs (teach movement and the echo), Brass Moonworks (combine a moving gust lane with a plate/gate), and Bellglass Spire (final multi-step route and beacon). Each contains authored geometry, one chime, one checkpoint, at least one safe reset route, and clear door/exit links. The final room requires the courier and echo to hold two separated plates together.
- **Failure and progress:** falling into a moonpool returns the player to the current checkpoint; collected chimes and opened rooms persist across recovery. Progress remains in per-user save data, separate from the read-only project tree.
- **Presentation:** cohesive night-sky palette, a deterministic parallax skyline and starlight field with warm window accents, original low-resolution RGBA moth art and a small synthesized chime sound. HUD text is scaled for readability and names controls and progress; Echo availability/expiry and locked gates remain visible without color alone.
- **Replay/tests:** fixed-tick logic must support a recorded input replay. Named tests cover the echo capture window, deterministic playback, plate/gate interaction, hazard/checkpoint recovery, chime persistence, room transitions, and beacon completion.

## Implementation boundaries

Use only Hycel-owned runtime APIs and repository-authored files. No external engine, network service, generated build-machine state, or project-provided code execution. Keep the simulation tick-indexed and bounded; the echo must never mutate the authoritative input history or physics state. Because it changes gate state, its position, playback cursor, and gate interaction belong in authoritative/replay-hashed game state. Prefer existing renderer, physics, audio, save, input, project, and CLI interfaces; add narrowly-scoped engine features only when required by a named gameplay test. Cross-platform replay identity, packaged-game certification, and physical-device behavior remain separate evidence gates.

## Acceptance evidence

- A valid project under `examples/bellglass-courier/` with three connected scenes, authored resources, named input bindings, and stable IDs.
- Headless scenarios demonstrate all gameplay progression and failure paths without a window or audio device.
- A deterministic replay reproduces the same same-target final state hash; do not claim cross-target identity.
- A windowed build is playable and visually communicates the echo puzzle and completion.
- Native packaging is tested through clean extraction and named scenarios on available CI targets; only targets with completed native CI evidence may be claimed.

This document freezes product intent, not completion evidence. Phases 8.2–8.5 remain open until implementation, testing, packaging, and learning-path validation satisfy their gates.
