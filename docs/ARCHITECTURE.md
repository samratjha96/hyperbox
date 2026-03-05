# Architecture

hyperbox is organized as a Rust workspace with trait-driven runtime abstractions.

## Components

- `hyperbox-core`: canonical types and traits (`SandboxBackend`, `SnapshotStore`, config/models)
- `hyperbox-network`: allowlist parser/evaluator for domain-level policy checks
- `hyperbox-firecracker`: Linux capability probe for `/dev/kvm`, nftables, ipset readiness
- `hyperbox-apple`: macOS capability probe for Virtualization/Containerization framework support
- `hyperbox-agent`: shared agent protocol request/response schemas
- `hyperbox-server`: runtime façade, local backend MVP, warm pool manager, metrics, snapshot metadata store
- `hyperbox-cli`: operational CLI (`run`, `templates`, `probe`)
- `hyperbox-py`: Python SDK that shells out to the CLI

## Current Execution Model

MVP execution is implemented by `LocalBackend`:

1. Create sandbox record with isolated working directory under temp storage
2. Execute commands with timeout and scoped environment
3. Support file write/read APIs for artifacts
4. Destroy sandbox and cleanup directory

This keeps API compatibility while Firecracker and Apple VM backends mature.

## Design Alignment

- Network modes implemented in API: `none`, `allowlist`, `full`
- Template strategy: pre-baked template registry with validation
- Warm pool strategy: `WarmPoolManager` pre-provisions reusable sandboxes
- Metrics strategy: create/destroy/exec counters and p50/p95 exec latency
- Snapshot strategy: trait abstraction + in-memory metadata store
