# hyperbox

Secure, cross-platform sandbox runtime for AI-agent code execution.

This repository implements the architecture in `DESIGN.md` as a modular Rust workspace with:

- shared sandbox traits and domain models
- gRPC control plane (`hyperbox-server`) and gRPC agent daemon (`hyperbox-agentd`)
- Firecracker backend lifecycle + API client + snapshot hooks
- Apple backend scaffolding for virtualization workflows
- local execution backend fallback
- Linux/macOS capability probes
- warm pool and metrics infrastructure
- DNS allowlist + nftables/ipset planning modules
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
cargo run -p hyperbox-cli -- serve --addr 127.0.0.1:50051
cargo run -p hyperbox-cli -- templates
cargo run -p hyperbox-cli -- probe
cargo run -p hyperbox-cli -- run --template python:3.12 --workspace "$PWD" --cmd "echo hello"
cargo run -p hyperbox-cli -- create --workspace "$PWD" --json
cargo run -p hyperbox-cli -- run --sandbox-id <sandbox-id> --cmd "ls -la"
cargo run -p hyperbox-cli -- destroy --sandbox-id <sandbox-id>
cargo run -p hyperbox-cli -- proxy --workspace "$PWD"
cargo run -p hyperbox-server
cargo run -p hyperbox-agent --bin hyperbox-agentd
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 bench --template python:3.12 --cmd "python3 -c 'print(1)'" --json
```

See `docs/ARCHITECTURE.md` and `docs/QUICKSTART.md` for more detail.

## Network Policy Enforcement

- `network=allowlist` is treated as an enforce-or-fail feature.
- Local backend rejects networked modes by default.
- Firecracker requires firewall enforcement enabled (`HYPERBOX_NETWORK_DRY_RUN=0`).
