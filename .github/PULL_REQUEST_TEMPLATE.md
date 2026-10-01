## What problem does this solve?

<!-- User problem and relevant issue/roadmap item -->

## What changed?

<!-- Briefly describe the implementation and major tradeoffs -->

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-targets --locked`
- [ ] `cargo deny check` (when dependency/configuration changes)
- [ ] Platform(s)/architecture(s) exercised: <!-- list; say not tested if not -->

## Compatibility and safety

- [ ] No user data is silently deleted, overwritten, or migrated.
- [ ] Project/save/CLI/agent schema or API changes are documented and tested.
- [ ] Dependency, license, source, MSRV, and security impact is recorded if dependencies changed.
- [ ] Privacy/network/agent-permission impact is stated.
- [ ] User-facing docs, diagnostics, examples, and release notes are updated as needed.

## Remaining limitations

<!-- Known gaps, follow-up issues, manual tests not run; write “None” if none -->
