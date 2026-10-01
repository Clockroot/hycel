# Agent interface principles

Hycel should work with basic code-completion agents and advanced tool-using agents. Do not assume an agent has hidden project memory, a GUI, or a particular vendor integration.

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
