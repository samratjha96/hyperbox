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

## Benchmarks

Measured on **March 5, 2026** on **macOS 26.3 (Apple Silicon)** with:
- `target/release/hyperbox`
- Apple backend (`HYPERBOX_BACKEND=auto`, `HYPERBOX_APPLE_RUNTIME=containerization`)
- pre-pulled `alpine:3.20` for Docker comparison
- 30 measured runs, 5 warmup runs

### End-to-end startup (new sandbox/container each run)

| Command | mean | p50 | p95 |
| --- | ---:| ---:| ---:|
| `hyperbox shell --shell /bin/true --workspace "$PWD"` | 981.66 ms | 971.65 ms | 1084.58 ms |
| `docker run --rm --pull=never alpine:3.20 true` | 204.92 ms | 199.03 ms | 272.46 ms |

### Steady-state exec (reused sandbox/container)

| Command | mean | p50 | p95 |
| --- | ---:| ---:| ---:|
| `hyperbox shell --sandbox-id <id> --shell /bin/true` | 75.57 ms | 71.31 ms | 100.88 ms |
| `docker exec <container> true` | 45.57 ms | 44.67 ms | 52.48 ms |

### Reproduce

```bash
# Hyperbox (ephemeral)
./target/release/hyperbox shell --shell /bin/true --workspace "$PWD"

# Docker baseline (smallest practical image)
docker run --rm --pull=never alpine:3.20 true
```

## Network Policy Enforcement

- `network=allowlist` is treated as an enforce-or-fail feature.
- Local backend rejects networked modes by default.
- Firecracker requires firewall enforcement enabled (`HYPERBOX_NETWORK_DRY_RUN=0`).
