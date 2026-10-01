# Contributing

Mycel is early-stage. Please discuss significant architecture, file-format, or public API changes in an issue before opening a large implementation PR.

## Development

Requirements: stable Rust 1.85 or newer.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p mycel-demo
```

Keep platform APIs outside `mycel-core`. Add tests for behavior and regression cases. Document new public APIs and user-visible formats. Avoid introducing dependencies for functionality that can be expressed clearly with the standard library; when a dependency is justified, document license, maintenance, and target support.

## Pull requests

Include: problem statement, design/tradeoffs, test evidence, platform coverage, and any migration or compatibility impact. Avoid unrelated formatting or refactors.
