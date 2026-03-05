# hyperbox

Secure, cross-platform sandbox runtime for AI-agent code execution.

This repository contains the initial implementation of the architecture in `DESIGN.md`, with a modular Rust workspace and a minimal Python SDK wrapper.

## Workspace

- `crates/hyperbox-core`: shared types, traits, and policies
- `crates/hyperbox-network`: network policy parsing/matching
- `crates/hyperbox-agent`: agent protocol models
- `crates/hyperbox-firecracker`: Linux backend capability scaffolding
- `crates/hyperbox-apple`: macOS backend capability scaffolding
- `crates/hyperbox-server`: runtime manager and pool
- `crates/hyperbox-cli`: user CLI (`hyperbox`)
- `hyperbox-py`: Python SDK shelling out to CLI

## Current Status

The codebase ships an MVP architecture with a local backend, policy-aware execution API, template registry, capability probes, warm pool manager, metrics, and tests.

## Build

```bash
cargo build --workspace
cargo test --workspace
```
