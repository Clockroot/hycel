# ADR 0004: Game audio backend

- **Status:** Accepted for Phase 5.5 (implementation under validation)
- **Decision owners:** Hycel maintainers
- **Scope:** Basic sound effects and simple looping background music for the small single-player platformer profile

## Context

Phase 5.1 requires basic audio feedback but explicitly excludes advanced audio tooling. Missing output devices must not prevent gameplay, audio timing must not affect authoritative simulation state, and backend types must stay behind Hycel-owned APIs. The target build matrix remains macOS, Linux, and Windows on x86-64 and ARM64; a native compile is not evidence of a working physical audio device.

## Decision

Use Kira 0.12.5 through the `hycel-audio` crate. The Kira version and feature set are pinned in the workspace. Hycel exposes bounded, owned clip data, a single looping music voice, up to 32 overlapping effect voices, explicit output status, best-effort skipped-play outcomes, and a method for draining asynchronous decoder errors. Kira types never appear in Hycel's public API. Audio callbacks/device activity do not mutate `hycel-core` state or advance its clock.

Kira's CPAL output startup failure creates an inspectable silent service. Requests made when output is unavailable (or the effect-voice cap is full) return a skipped-play outcome rather than failing gameplay. Runtime playback errors remain typed presentation diagnostics.

The selected Kira features are `cpal`, `flac`, `mp3`, `ogg`, `vorbis`, `wav`, and `pcm`; Kira defaults are disabled to avoid enabling unused format/realtime features. Encoded clip input is capped at 64 MiB and fed to Kira's streaming decoder. Streaming avoids eagerly decoding a potentially long music track into one allocation; source loading remains the responsibility of the caller and must use the project's validated path boundary.

## Dependency review

- **Purpose/boundary:** Kira provides game-oriented sound playback, looping, and mixer/device lifecycle behind the replaceable `hycel-audio` boundary. Alternatives considered were Rodio (less game-oriented controls) and direct CPAL integration (would require Hycel to own more playback/mixing behavior). The user explicitly selected Kira.
- **Maintenance:** Kira is published by Andrew Minnich at `tesselode/kira`; 0.12.5 is the selected crates.io release. Its documentation describes desktop support, with most testing on Windows and successful use on macOS/Linux. Hycel's six-target CI validates builds, not actual output on every architecture.
- **License:** Kira is `MIT OR Apache-2.0`; CPAL is Apache-2.0. The selected Kira decoding features require Symphonia 0.6.1 and Kira requires `triple_buffer` 9.0.0; both and the included Symphonia decoder subcrates declare MPL-2.0. The user approved this license trade-off. `deny.toml` grants MPL-2.0 only to the enumerated Symphonia packages and `triple_buffer`, not globally. Before distributing binaries, include the required third-party notices and make the applicable MPL-covered source available as required by MPL-2.0; re-review the exact obligations for the release packaging model.
- **MSRV:** Kira does not declare `rust-version` in the selected manifest. CPAL and Symphonia declare Rust 1.85. The locked Kira audio crate and its tests passed locally on Hycel MSRV Rust 1.87.0; the full workspace MSRV CI is required before phase completion.
- **Transitive footprint:** Adding Kira 0.12.5 adds CPAL, Symphonia with only WAV/FLAC/MP3/Ogg-Vorbis/PCM support, and their platform/codec dependencies. The lockfile records the complete graph; `cargo deny check` audits advisories, licenses, sources, and duplicate growth.
- **Native/system requirements:** CPAL uses OS audio backends; Linux builds need ALSA development headers (`libasound2-dev` in CI). macOS uses CoreAudio and Windows uses its native audio backend through CPAL. No additional Hycel unsafe code or C library source is added.
- **Platform evidence:** The existing six-target native CI matrix builds and tests the wrapper on all six OS/architecture rows. Hosted and local unit tests do not establish physical speaker/device behavior, minimum-OS runtime support, audio latency, or output-device recovery; those remain explicit manual/runtime validation gates.
- **Security/resource bounds:** Hycel caps encoded clip bytes and active short-effect voices, uses streamed decoding for long playback, and surfaces asynchronous decoder errors. Malformed/unsupported clip headers are rejected. Audio is non-authoritative and must remain outside replay state hashes.

## Consequences and limitations

- The audio API is best-effort and independent of authoritative simulation; gameplay proceeds in silent mode when devices are unavailable.
- Only the explicitly enabled formats are supported by the initial decoder feature set; no audio import executor or authoring UI is added here.
- The voice limits and simple music looping cover the sample scope only. Spatial audio, arbitrary mixer graphs, recording, voice chat, and cross-device synchronization remain out of scope.
- Physical audio-device tests, runtime error handling, and packaging/license notices must be added to release validation before making a user-facing audio support claim.
