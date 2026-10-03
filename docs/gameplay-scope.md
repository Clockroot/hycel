# Reference platformer capability profile

This profile narrows the 1.0 sample game and Phase 5 implementation to a small, finishable single-player side-view platformer. It extends the product-level scale in [`product-scope.md`](product-scope.md) with explicit gameplay requirements and exclusions; it does not select runtime APIs or third-party backends.

## Required sample loop

- A title/start flow, responsive horizontal movement, and a single-player jump.
- At least two compact stages where feasible, with a side-view camera, a scrolling section, reusable assets/scenes, and solid platforms.
- Hazards or simple enemies, at least one collectible/objective, a clear success state, and failure/retry behavior.
- A checkpoint or level-transition mechanic and a completion/ending state.
- Basic audio feedback: short sound effects and simple background music. Audio-device absence must not prevent gameplay; audio timing is presentation-only and must not change authoritative simulation state.
- Persistent, local single-player progress (at minimum stage/checkpoint progress) that survives process restart. Save data is distinct from project/source assets and must be versioned, bounded, validated, and written atomically. Phase 5.6 selects per-user OS local data storage, strict schema-1 JSON, and one last-known-good backup with explicit recovery; see [ADR 0005](adr/0005-progress-save.md).

## Explicit exclusions

- No multiplayer, networking, cloud sync, cross-device account, or shared save service.
- No general-purpose 3D/spatial audio, audio authoring workstation, recording, voice chat, or user-scripted audio graph.
- No arbitrary mod/plugin persistence or arbitrary game-object serialization. Persist only the small, documented progress model needed by the sample.
- No general-purpose platformer physics sandbox, ragdolls, deformable terrain, or promise of cross-architecture bitwise simulation identity.
- No expansion of the product promise to genres beyond small single-player side-view platformers.

## Implementation ordering

Phase 5 should establish deterministic collision/contact behavior and gameplay actions before implementing the sample loop. Animation/scene transitions follow. The selected audio and save capabilities are included in the sample profile. Kira is selected for Phase 5.5 behind Hycel-owned APIs. Phase 5.6 initially used the `hycel-save` API for stable scene/checkpoint references, not arbitrary object state. Save schema 2 adds a bounded set of stable collected-item UUIDs for the Bellglass Courier; schema-1 saves migrate explicitly. The original two-room windowed project remains documented in [`platformer-vertical-slice.md`](platformer-vertical-slice.md), and the newer [Courier](game-design.md) adds Echo Flight, collectible persistence, and three rooms. Keep gameplay/save decisions inspectable through shared file-native/CLI workflows; do not make the visual editor a prerequisite.
