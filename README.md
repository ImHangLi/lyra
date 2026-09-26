# Lyra

**Lyra is an agent-native, fully customizable control plane for your development workflow.**

See it, keep it, reuse it. Humans use a terminal workbench; agents use a structured CLI. Both drive the same project, the same host, and the same plugins and actions. The Core runs no models and knows nothing about your business. Your existing agent does setup and extension through skills.

> Status: under active development. Track progress in [the tracking issue](https://github.com/ImHangLi/lyra/issues/19).

## What works today

```sh
cargo build --locked
target/debug/lyra --version
target/debug/lyra schema plugin            # JSON Schema generated from the Rust wire types
target/debug/lyra validate examples/playground/.lyra
scripts/check-contract.sh                  # schema drift + fixtures through the real validator
```

## Layout

- `crates/lyra-protocol`: IDs, wire DTOs, validated domain types, errors, schemas.
- `crates/lyra-cli`: the `lyra` binary.
- `examples/playground`: a small workspace with command, PTY, and structured plugins.
- `fixtures/invalid`: manifests that must be rejected.
- `schemas`: generated JSON Schemas (checked for drift).

Contributor and agent rules: [AGENTS.md](AGENTS.md).
