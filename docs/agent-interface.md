# Agent interface principles

Mycel should work with basic code-completion agents and advanced tool-using agents. Do not assume an agent has hidden project memory, a GUI, or a particular vendor integration.

## Three levels

### Level 1: file-native

A basic agent can read `README.md`, `AGENTS.md`, Rust source, and text project/scene files. The project builds through ordinary Cargo commands. Files are organized predictably; examples are small; diagnostics point to exact locations.

### Level 2: CLI-native

A tool-using agent can call `mycel --help`, `mycel check --json`, `mycel test --json`, `mycel inspect`, and `mycel replay`. JSON uses a versioned envelope and stable machine-readable diagnostic codes. Commands are composable and work headlessly in CI.

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

## Planned JSON result envelope

```json
{
  "schema_version": 1,
  "ok": false,
  "result": null,
  "diagnostics": [
    {
      "code": "MYCEL_SCENE_MISSING_RESOURCE",
      "severity": "error",
      "message": "Scene references an asset that does not exist.",
      "location": { "file": "scenes/main.mycel", "path": "entities[2].sprite" },
      "hint": "Import the asset or update the reference."
    }
  ]
}
```

This is a design example, not a released API. Freeze only after command/schema tests exist.
