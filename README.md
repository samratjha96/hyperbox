# hyperbox

Secure sandbox runtime for AI agent code execution.

hyperbox gives you a consistent control plane for running untrusted code in isolated sandboxes with explicit network policy, persistent sessions, and reproducible lifecycle operations.

## TL;DR

**Problem:** Agent workflows need to run arbitrary shell/Python/build commands safely without destroying host environments or leaking credentials.

**Solution:** Run commands through `hyperbox`, which provides isolated execution, network policy enforcement (`none`/`allowlist`/`full`), reusable sandbox sessions, and snapshot restore workflows.

### Why hyperbox?

| Capability | What you get |
| --- | --- |
| Secure-by-default execution | `network=none` default policy with explicit opt-in to broader network access |
| Reusable stateful sessions | Install dependencies once, reuse environment across commands |
| Agent + human friendly interfaces | CLI, gRPC control plane, stdio proxy mode, Python SDK |
| Isolation transparency | `--explain` shows backend selection and effective policy |
| Snapshot workflows | Save and restore sandbox state for fast recovery and reproducibility |

## Quickstart (2 Minutes)

### 1) Build and setup (macOS)

```bash
cargo build -p hyperbox-cli
./target/debug/hyperbox setup
```

### 2) Run a command with default locked networking

```bash
HB=./target/debug/hyperbox
$HB run --cmd "python3 -c 'print(2 + 2)'"
```

### 3) Allow exactly one domain, and verify inverse blocking

```bash
$HB run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://example.com', timeout=8).status)\""

$HB run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; urllib.request.urlopen('https://github.com', timeout=8)\""
```

The second command should fail because `github.com` is not allowlisted.

### 4) Keep state between runs (install once, reuse)

```bash
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"
```

`run` reuses a deterministic session by default. Use `--ephemeral` for create/execute/destroy one-offs.

## Core Concepts

### Session model

- `hyperbox run` without `--ephemeral` reuses a deterministic affinity session.
- `hyperbox create` creates an explicit persistent sandbox.
- `hyperbox destroy` tears down by sandbox id or affinity name.

### Network model

- `none`: no external network access
- `allowlist`: explicit hostnames only (wildcards not supported)
- `full`: unrestricted network

### Workspace model

- `--workspace <PATH>` bind-mounts/link-maps host workspace into sandbox context.
- Without `--workspace`, sandbox gets managed ephemeral workspace storage.

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│                         Clients                             │
│   hyperbox CLI    Python SDK    Agent Adapter (Proxy)      │
└───────────────┬─────────────────────────────────────────────┘
                │ gRPC / stdio JSON-lines
┌───────────────▼─────────────────────────────────────────────┐
│                 hyperbox control plane                       │
│          crates/hyperbox-server (runtime + snapshots)       │
└───────────────┬─────────────────────────────┬───────────────┘
                │                             │
      ┌─────────▼─────────┐         ┌────────▼──────────┐
      │ macOS backend      │         │ Linux backend      │
      │ crates/hyperbox-   │         │ crates/hyperbox-   │
      │ apple              │         │ firecracker        │
      └─────────┬──────────┘         └────────┬───────────┘
                │                               │
      ┌─────────▼──────────┐          ┌────────▼──────────┐
      │ Apple helper /      │          │ Firecracker VM +  │
      │ container runtime   │          │ hyperbox-agentd   │
      └─────────────────────┘          └───────────────────┘
```

Additional crate roles:

- `crates/hyperbox-core`: shared types, backend traits, templates, snapshots
- `crates/hyperbox-network`: network/firewall planning and evaluators
- `crates/hyperbox-agent`: guest/sidecar agent daemon protocol and service
- `crates/hyperbox-cli`: end-user CLI entrypoint
- `hyperbox-py`: Python SDK wrappers (gRPC + compatibility wrapper)

## Platform Matrix

| Platform | Backend | Status | Notes |
| --- | --- | --- | --- |
| macOS (Apple Silicon) | Apple backend (`auto`) | Supported | Uses helper-managed runtime; allowlist currently DNS/domain-based |
| Linux (KVM-capable) | Firecracker backend (`auto`) | Supported | Enforces firewall-based policies when firewall enforcement is enabled |
| Any OS | Local backend (`HYPERBOX_BACKEND=local`) | Dev-only fallback | Not isolated like VM backends; not recommended for production isolation |

## CLI Reference

Top-level commands:

- `run`: execute a command in sandbox
- `create`: create persistent sandbox
- `destroy`: destroy by id/name
- `list`: list active sandboxes
- `inspect`: inspect sandbox metadata
- `shell`: interactive shell attach/create
- `templates`: list templates
- `probe`: host capability probe
- `setup`: host prerequisite setup
- `proxy`: stdio JSON-lines adapter mode
- `snapshot`: `create`, `restore`, `list`

Common flows:

```bash
# Create, reuse, destroy
HB=./target/debug/hyperbox
SBX=$($HB create --name myproj --workspace "$PWD" --profile locked)
$HB run --name myproj --cmd "python3 -V"
$HB list
$HB destroy --name myproj

# Snapshot lifecycle
SBX=$($HB create --workspace "$PWD")
SNAP=$($HB snapshot create --sandbox-id "$SBX")
$HB destroy --sandbox-id "$SBX"
$HB snapshot restore --snapshot-id "$SNAP"

# Proxy mode for agent adapters
$HB proxy --workspace "$PWD"
```

For complete command flags:

```bash
./target/debug/hyperbox --help
./target/debug/hyperbox run --help
./target/debug/hyperbox proxy --help
./target/debug/hyperbox snapshot --help
```

## Profiles and Configuration

Built-in profiles:

- `locked` -> `network=none`
- `web` -> `network=allowlist` (must provide allowlist domains)
- `full` -> `network=full`

Custom profile config path:

- Default: `~/.hyperbox/profiles.toml`
- Override: `--profile-config <path>`

Example:

```toml
[profiles.team_web]
network = "allowlist"
allow = ["github.com", "pypi.org", "files.pythonhosted.org"]
```

```bash
HB=./target/debug/hyperbox
$HB run --profile team_web --cmd "python3 -m pip install requests"
```

## Environment Variables

| Variable | Purpose |
| --- | --- |
| `HYPERBOX_BACKEND` | Force backend (`auto`, `apple`, `firecracker`, `local`) |
| `HYPERBOX_APPLE_RUNTIME` | macOS runtime preference (`containerization` or `virtualization`) |
| `HYPERBOX_APPLE_HELPER` | Override helper command |
| `HYPERBOX_AGENT_ENDPOINT` | Agent endpoint for Firecracker/agent-stream paths |
| `HYPERBOX_AGENT_AUTOSTART` | Disable auto-start sidecar when set to `0`/`false` |
| `HYPERBOX_NETWORK_DRY_RUN` | Firewall dry-run behavior for Firecracker network enforcement |
| `HYPERBOX_LOCAL_ALLOW_UNENFORCED_NETWORK` | Allow non-`none` network in local backend (dev-only) |

## Python SDK

Install:

```bash
pip install -e hyperbox-py
```

gRPC SDK usage:

```python
from hyperbox import HyperboxClient, SandboxSession

with HyperboxClient("127.0.0.1:50051") as client:
    with SandboxSession(client, template="python:3.12", workspace=".") as box:
        out = box.exec("python3 -c 'print(42)'")
        print(out.stdout)
```

## Performance and Benchmarks

Benchmark harnesses are in:

- `scripts/benchmark_apples_to_apples.py`
- `scripts/benchmark_python_sdk_vs_opensandbox.py`

Recorded benchmark outputs:

- `benchmarks/apples_to_apples.json`
- `benchmarks/python_sdk_vs_opensandbox.json`
- `benchmarks/snapshot_lifecycle_bench.json`

Use these as reproducible baselines and rerun in your own environment for decision-grade numbers.

## Troubleshooting

### `server does not support 'list' yet (likely stale daemon)`

Restart the local control plane and retry:

```bash
pkill -f hyperbox || true
./target/debug/hyperbox list
```

### `failed to connect to hyperbox control plane`

Ensure server autostart is enabled (default) or start manually:

```bash
./target/debug/hyperbox serve --addr 127.0.0.1:50051
```

### `invalid allowlist ...`

Use explicit hostnames only:

- valid: `example.com`, `api.github.com`
- invalid: `*.example.com`, `https://example.com`, `example.com:443`

### `command not found` inside sandbox

Install tool in the persistent sandbox session first:

```bash
HB=./target/debug/hyperbox
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"
```

## Security Notes and Current Limits

- Allowlist entries are explicit hostnames only; wildcard patterns are rejected.
- On macOS built-in helper path, allowlist enforcement is currently DNS/domain-based (host-level direct-IP firewall blocking is not yet implemented).
- `--workspace` intentionally maps host files into sandbox context; writes there are host-visible by design.
- `HYPERBOX_BACKEND=local` is a non-isolated fallback intended for local development/testing only.

## Development

```bash
cargo fmt --all
cargo test --workspace
cargo build --workspace
```

## Further Reading

- `docs/QUICKSTART.md`
- `docs/ARCHITECTURE.md`
- `docs/APPLE_HELPER_PROTOCOL.md`
- `docs/AGENT_INTEGRATION_DECISION.md`

## License

MIT
