# Hycel Editor

`hycel-editor` is a keyboard-first native authoring shell over the same validation, inspection, preview, and transactional edit services used by `hycel` and `hycel-mcp`. It does not embed a separate serializer or execute project-provided code.

## Launch

```sh
cargo run -p hycel-editor -- ./examples/platformer-game
```

A windowed native renderer is required. The project is validated before the window opens. The editor uses a three-panel hierarchy / scene overview / inspector layout; the scene view is a deterministic colored-rectangle overview, not a material preview or live GPU capture. Short labels are drawn at 2x bitmap scale where panel width allows; longer diagnostics stay compact to avoid clipping. `F10` toggles between 1x and fit-aware 2x scale, but this remains a bitmap diagnostic font rather than general typography. A bounded 30-present smoke is available for CI/hardware triage:

```sh
cargo run -p hycel-editor -- --smoke ./examples/platformer-game
```

The smoke exits after 30 presented frames (or fails after render errors / 120 unpresented attempts); it is not a physical-device, usability, or accessibility certification.

## Controls

| Key | Action |
| --- | --- |
| `Tab` | Next scene (blocked while a change is staged). |
| `Up` / `Down` | Select previous/next entity. |
| `I` / `J` / `K` / `L` | Stage a 25 milli-unit vertical/horizontal move. |
| `Z` | Stage an empty scene at a fresh UUID-derived path. |
| `C` | Stage an empty entity with a fresh UUID. |
| `M` | Rename the current scene; type an ASCII/digit name, `Enter` stages, `Backspace` edits, `Escape` cancels. |
| `N` | Rename the selected entity with the same name-entry controls. |
| `G` | Toggle one lowercase alphanumeric tag on the selected entity; type letters/digits, `Enter` stages, `Backspace` edits, `Escape` cancels. |
| `T` | Cycle the selected authored animation resource. |
| `A` | Stage/upsert `hycel.animation` with the selected animation resource. |
| `X` | Stage deletion of the selected entity; parent entities with children are rejected. |
| `V` | Stage removal of the selected entity's first component. |
| `P` | Preview the typed operation; the bounded diff and source/candidate hashes are shown. |
| `S` | First press previews if needed; second press confirms and atomically applies the exact preview. |
| `R` | Discard staged data and reload from disk. |
| `F5` | Start/stop the compiled-in reference-game process in another window; while it runs, show bounded live tick/scene/player/input/Echo/gate/chime snapshots and recent output. |
| `F6` | Re-run the shared project check and display diagnostics. |
| `F7` | Run the bounded named reference-game suite; save a unique schema-1 JSON report under `.hycel/editor-previews/` (up to 64 retained reports) and open its report viewer. |
| Report viewer | `Up`/`Down` select a scenario, `Left`/`Right` page through long messages, `Escape` closes the viewer. |
| `F8` | Export the current authored-scene SVG to `.hycel/editor-previews/` without overwriting. |
| `F9` | Cycle through resources and show a bounded source hash, validated cached import fingerprints, and reimport decisions. |
| `F10` | Toggle editor diagnostic text between 1x and fit-aware 2x scale; default is 2x. |

Each successful edit to an existing file makes a non-overwriting source backup under `.hycel/backups/`; a newly created scene has no prior bytes to back up and is published without overwriting an existing path. A stale preview is rejected. The editor never overwrites the file silently, and edits require an explicit preview followed by a second save action.

## Asset and diagnostics feedback

F5 opts the child runtime into a versioned schema-1 JSON debug stream at 10 snapshots/second after gameplay starts. A 64-message nonblocking channel drops overflow instead of backpressuring simulation; each child-output line is capped at 4 KiB before forwarding, snapshots reject unknown fields, and UI summaries are truncated to fit their panels. The snapshot reports the global and per-scene ticks, scene identity, player position/velocity/grounding, action inputs, remaining Echo ticks, gate/chime/completion state. This is a read-only current-state view, not performance profiling, historical recording, arbitrary entity inspection, or replay control. The inspector lists registered component types, resource kind/source/descriptor, and direct resource dependency counts from paginated `hycel inspect` results. `F9` hashes the selected source (64 MiB limit) and inspects up to 64 strict cache records with bounded file counts; it reports cache fingerprints and compares source/settings/path using each record's captured importer version. The displayed decision is conditional on that recorded importer version, not an importer-freshness check. It does not infer newer importer releases, run import jobs, rewrite caches, or mutate import settings. `F6` and failed previews display the CLI's structured validation diagnostics. The current runtime process supports only the compiled-in two- or three-room reference-game profiles, just like `hycel run`; the editor never compiles `src/` or launches arbitrary project commands.

## Current scope limits

The first shell supports scene/entity browsing, keyboard selection and movement, staged scene creation/rename and entity creation/rename/deletion/tag toggles, registered animation-component upsert/removal with a selectable animation resource, preview/apply/discard, asset-reference and cached-import feedback, basic project diagnostics, authored-scene SVG export, reference-game play/stop, and bounded current-game tick/input/state telemetry with recent output summaries. It does not yet support mouse picking/gizmos, scene deletion, arbitrary registered component editing, texture/material import/reimport, arbitrary entity inspection, performance profiling, historical recording, or replay controls in the editor. It is keyboard-operable but has not received physical-DPI, screen-reader, or broad accessibility certification. The editor serializes report exports within its process; the 64-file cap is not compare-and-swap against another process changing `.hycel/editor-previews`. Root/source checks are not race-resistant against concurrent external filesystem replacement; keep projects quiescent while editing.

The editor's model tests cover validate/open, resource dependency/import-record display, staged edit preview/apply/backup, scene creation/rename, entity creation/deletion/rename/tag toggles, component upsert/removal with selectable animation resources, discard and collision handling, bounded versioned test reports exported without overwriting existing artifacts, a keyboard-navigable scenario/message viewer, visible failure summaries, non-mutating test diagnostics, safe screenshot export, and round-trip preservation through shared services. UI routing tests verify that `M` stages a scene rename, `G` stages a lowercase tag toggle, and dirty state prevents switching scenes. These do not substitute for physical window/device testing.
