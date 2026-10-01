# Contributing

Mycel is early-stage. Please discuss significant architecture, file-format, or public API changes in an issue before opening a large implementation PR.

## Development

Requirements: Rust 1.87.0 MSRV; stable Rust is recommended. See [`docs/support-matrix.md`](docs/support-matrix.md) for native targets and [`docs/engineering-contracts.md`](docs/engineering-contracts.md) for architecture, data-safety, privacy, and dependency rules.

Fresh setup from a machine with [rustup](https://rustup.rs/) installed:

```sh
rustup toolchain install 1.87.0 --profile minimal --component clippy --component rustfmt
git clone https://github.com/aaf2tbz/mycel.git
cd mycel
cargo +1.87.0 test --workspace --all-targets --locked
cargo +1.87.0 run -p mycel-demo --locked
```

Install the dependency-audit tool before running `cargo deny check`:

```sh
cargo install cargo-deny --version 0.20.2 --locked
```

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo deny check
cargo run -p mycel-demo --locked
```

Keep platform APIs outside `mycel-core`. Add tests for behavior and regression cases. Document new public APIs and user-visible formats. Avoid introducing dependencies for functionality that can be expressed clearly with the standard library; when a dependency is justified, record the review checklist from `docs/engineering-contracts.md` and keep `deny.toml`/`Cargo.lock` in sync.

## Pull requests

Include the problem statement, design/tradeoffs, test evidence, exact platform coverage, and migration/API/security/privacy/dependency impacts. Use the repository pull-request template. Avoid unrelated formatting or refactors.
