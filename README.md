# hyperbox

Secure, cross-platform sandbox runtime for AI-agent code execution.

This repository implements the architecture in `DESIGN.md` as a modular Rust workspace with:

- shared sandbox traits and domain models
- local execution backend for MVP behavior
- Linux/macOS capability probes for VM backends
- warm pool and metrics infrastructure
- CLI and Python SDK

## Layout

- `crates/hyperbox-core`: config, types, traits, templates, snapshots
- `crates/hyperbox-network`: network allowlist parsing and matching
- `crates/hyperbox-agent`: protocol schema for exec/file operations
- `crates/hyperbox-firecracker`: Linux capability probe
- `crates/hyperbox-apple`: macOS capability probe
- `crates/hyperbox-server`: runtime manager, local backend, pool, metrics
- `crates/hyperbox-cli`: `hyperbox` CLI
- `hyperbox-py`: Python SDK wrapper

## Commands

```bash
cargo build --workspace
cargo test --workspace
cargo run -p hyperbox-cli -- templates
cargo run -p hyperbox-cli -- probe
cargo run -p hyperbox-cli -- run --template python:3.12 --cmd "echo hello"
```

See `docs/ARCHITECTURE.md` and `docs/QUICKSTART.md` for more detail.
