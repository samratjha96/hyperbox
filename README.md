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
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 bench exec --template python:3.12 --cmd "python3 -c 'print(1)'" --json
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 bench snapshot --template python:3.12 --runs 5 --warmup 1 --timeout 240 --json
```

See `docs/ARCHITECTURE.md` and `docs/QUICKSTART.md` for more detail.

## Benchmarks

Measured on **March 6, 2026** on **macOS 26.3 (Apple Silicon)** with:
- `target/release/hyperbox`
- Apple backend (`HYPERBOX_BACKEND=auto`, `HYPERBOX_APPLE_RUNTIME=containerization`)
- image: `python:3.12` on both sides
- command: `true` on both sides (`/bin/sh -lc true` for Docker)
- same workspace mount (`$PWD -> /workspace`) on both sides
- `network none` on both sides
- 20 measured runs, 5 warmup runs

### Cold End-To-End (new sandbox/container each run)

| Command | mean | p50 | p95 |
| --- | ---:| ---:| ---:|
| `hyperbox run --template python:3.12 --workspace "$PWD" --cmd "true"` | 847.03 ms | 856.75 ms | 913.00 ms |
| `docker run --rm --pull=never --network none --workdir /workspace -v "$PWD:/workspace" python:3.12 /bin/sh -lc true` | 112.03 ms | 110.94 ms | 122.18 ms |

### Warm Exec (reused sandbox/container)

| Command | mean | p50 | p95 |
| --- | ---:| ---:| ---:|
| `hyperbox run --sandbox-id <id> --cmd "true"` | 54.20 ms | 53.80 ms | 59.92 ms |
| `docker exec <container> /bin/sh -lc true` | 44.41 ms | 44.84 ms | 47.96 ms |

### Reproduce

```bash
python scripts/benchmark_apples_to_apples.py \
  --runs 20 \
  --warmup 5 \
  --hyperbox-bin ./target/release/hyperbox \
  --output benchmarks/apples_to_apples.json
```

Raw output is stored at `benchmarks/apples_to_apples.json`.

### Snapshot Lifecycle Benchmark

Benchmark full affinity snapshot flow:
- `create`
- mutate state
- `snapshot create`
- destroy
- restore via affinity (`run --name ...`)
- verify + destroy

```bash
./target/release/hyperbox --server-url http://127.0.0.1:50051 \
  bench snapshot \
  --template python:3.12 \
  --runs 5 \
  --warmup 1 \
  --timeout 240 \
  --json > benchmarks/snapshot_lifecycle_bench.json
```

Observed on March 6, 2026 (Apple backend, containerization runtime, 5 runs):

| Stage | p50 |
| --- | ---: |
| create | 663.95 ms |
| mutate | 50.93 ms |
| snapshot create | 27.71 s |
| destroy (initial) | 290.96 ms |
| restore + verify | 7.03 s |
| destroy (restored) | 942.70 ms |
| total | 36.85 s |

## Network Policy Enforcement

- `network=allowlist` is treated as an enforce-or-fail feature.
- Local backend rejects networked modes by default.
- Apple backend supports `network=none` and `network=full`; `network=allowlist` is rejected until enforcement is implemented.
- Firecracker requires firewall enforcement enabled (`HYPERBOX_NETWORK_DRY_RUN=0`).
