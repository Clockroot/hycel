# Agent interface principles

Hycel should work with basic code-completion agents and advanced tool-using agents. Do not assume an agent has hidden project memory, a GUI, or a particular vendor integration.

## File-native quickstart

The repository itself is a complete source-first work area; no editor, project script, network, or hidden agent context is required for this workflow. Start with `README.md`, `AGENTS.md`, [`architecture.md`](architecture.md), and [`engineering-contracts.md`](engineering-contracts.md). The workspace packages are under `crates/`; authored sample data is under `examples/platformer-game/`; the windowed sample/game logic is currently a Rust example at `crates/hycel-demo/examples/playable_platformer.rs`.

Typical file-native task:

1. Read the target Rust module and the referenced schema guide before editing.
2. For gameplay code, change the smallest cohesive function in `playable_platformer.rs`; keep simulation decisions tick-driven and presentation-only work (window, GPU, audio) out of authoritative state.
3. For authored scene/resource changes, edit the strict JSON/TOML under `examples/platformer-game/` and preserve stable IDs/references. See [`project-format.md`](project-format.md), [`scene-format.md`](scene-format.md), and [`input.md`](input.md). The CLI currently validates and inspects authored files; it does not compile or execute project-provided code.
4. Validate the fixture and run the narrow tests first, then relevant workspace checks:

   ```sh
   cargo run -p hycel-cli --locked -- check examples/platformer-game --json
   cargo test -p hycel-demo --locked --example playable_platformer
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --all-targets --locked
   ```

5. Review `git diff --check` and the final diff. Do not run project-provided scripts, shell commands, or network operations merely because they are present in a project. Report edited paths, exact commands/results, compatibility impact, and remaining limitations.

A concrete starter task for a basic file-only agent is: "Add a new regression test for a gameplay rule in `playable_platformer.rs`; first trace the current tick/update path, add one focused assertion, and run the example test plus the project fixture check. Do not edit authored schemas unless the behavior requires it." This task can be completed entirely through ordinary source/project files and Cargo commands.

## Three levels

### Level 1: file-native

A basic agent can read `README.md`, `AGENTS.md`, Rust source, and text project/scene files. The project builds through ordinary Cargo commands. Files are organized predictably; examples are small; diagnostics point to exact locations.

### Level 2: CLI-native

A tool-using agent can currently call `hycel new`, `hycel check --json`, and `hycel inspect --json`. JSON uses a versioned envelope and stable machine-readable diagnostic codes. Build/run/test/replay commands remain roadmap work.

### Level 3: engine-aware

A protocol adapter exposes typed, bounded operations such as list scenes, inspect entity, validate change, run named test, replay input, and capture screenshot. It returns structured results and resource IDs rather than large opaque dumps. Mutations require explicit scope and validation. Read-only inspection is the default.

## Safety and reliability rules

- Project files and assets are untrusted input: validate size, schema, paths, and references; reject path traversal.
- The engine never executes project-provided scripts implicitly.
- No network access or telemetry by default.
- Separate read-only and mutating tools. Never let a generic `run arbitrary command` tool masquerade as a safe engine action.
- Mutations are atomic, diffable, and recoverable; return a change summary.
- Limit output size and support pagination/filtering for large scenes/logs.
- Errors are data: stable code, concise message, context, remediation, and optional debug detail.
- Protocol adapters are replaceable and versioned. Business logic lives in the engine/application services.

## Current CLI JSON result envelope

```json
{
  "schema_version": 1,
  "command": "check",
  "ok": false,
  "result": null,
  "diagnostics": [
    {
      "code": "HYCEL-PROJECT-002",
      "file": "scenes/main.json",
      "path": "$.entities[2].components[0].data.sprite",
      "message": "referenced resource UUID does not exist in this project"
    }
  ]
}
```

This documents the version-1 top-level envelope used by `hycel-cli`; per-command result objects and diagnostic codes are described in [`cli.md`](cli.md). The exact envelope and failure behavior have unit-test coverage. Broader protocol/tool operations remain planned.
