# hyperbox

Secure sandbox runtime for AI agent code execution.

hyperbox gives you a consistent control plane for running untrusted code in isolated sandboxes with explicit network policy, persistent sessions, and durable managed processes.

## TL;DR

**Problem:** Agent workflows need to run arbitrary shell/Python/build commands safely without destroying host environments or leaking credentials, and they also need to delegate work that may outlive a single terminal session.

**Solution:** Run commands through `hyperbox`, which provides isolated execution, network policy enforcement (`none`/`allowlist`/`full`), reusable sandbox sessions, managed process lifecycle (`run`, `--detach`, `logs`, `wait`, `cancel`), and snapshot restore workflows.

### Why hyperbox?

| Capability | What you get |
| --- | --- |
| Secure-by-default execution | `network=none` default policy with explicit opt-in to broader network access |
| Reusable stateful sessions | Install dependencies once, reuse environment across commands |
| Durable delegated work | Start a process, disconnect, and come back later for logs, status, and exit code |
| Agent + human friendly interfaces | CLI and gRPC control plane with the same process model |
| Isolation transparency | `--explain` shows backend selection and effective policy |
| Snapshot workflows | Save and restore sandbox state for fast recovery and reproducibility |

## Quickstart (2 Minutes)

### 1) Install and setup (macOS)

```bash
# Ensure `hyperbox` is installed and available on PATH.
# If developing from source, build it first:
# cargo build -p hyperbox-cli && export PATH="$PWD/target/debug:$PATH"

hyperbox --help
hyperbox setup
```

### 2) Run a command with default locked networking

```bash
HB=hyperbox
$HB run --cmd "python3 -c 'print(2 + 2)'"
```

### 3) Delegate a longer task and come back later

```bash
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"

PROC=$($HB run --profile full --cmd "python3 -c 'import time; print(\"start\"); time.sleep(30); print(\"done\")'" --detach --json | jq -r '.process.process_id')
$HB logs "$PROC"
$HB wait "$PROC"
```

### 4) Allow exactly one domain, and verify inverse blocking

Note: on some macOS hosts, `allowlist` may be unavailable. If Hyperbox reports that, use `network=none|full` for now and run `hyperbox setup` to check host prerequisites.

```bash
$HB run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://example.com', timeout=8).status)\""

$HB run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; urllib.request.urlopen('https://github.com', timeout=8)\""
```

The second command should fail because `github.com` is not allowlisted.

Tip: add `--explain` to see effective backend mode and enforcement details for your host.

### 5) Keep state between runs (install once, reuse)

```bash
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"
```

`run` reuses a deterministic session by default. Use `--ephemeral` for create/execute/destroy one-offs.

## Core Concepts

### Sandbox model

- `hyperbox run` without `--ephemeral` reuses a deterministic affinity session.
- `hyperbox create` creates an explicit persistent sandbox.
- `hyperbox destroy` tears down by sandbox id or affinity name.

### Process model

- `hyperbox run` starts a managed process inside a sandbox.
- `hyperbox run --detach` returns immediately with a process id.
- `hyperbox logs`, `hyperbox wait`, and `hyperbox cancel` operate on the same managed process.
- One sandbox can have one managed foreground process at a time. If a targeted sandbox is already busy, Hyperbox creates a fresh sandbox for the new run and tells you that it did so.

### Network model

- `none`: no external network access
- `allowlist`: explicit hostnames only (wildcards not supported)
- `full`: unrestricted network
- Allowlist availability can vary by host/runtime capabilities; `--explain` shows effective enforcement.

### Workspace model

- `--workspace <PATH>` bind-mounts/link-maps host workspace into sandbox context.
- Without `--workspace`, sandbox gets managed ephemeral workspace storage.

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│                          Clients                           │
│        hyperbox CLI | gRPC clients | Agent adapters       │
└─────────────────────────────┬──────────────────────────────┘
                              │ gRPC / stdio JSON-lines
┌─────────────────────────────▼──────────────────────────────┐
│                    hyperbox control plane                  │
│       runtime, sandboxes, processes, snapshots, policy     │
└─────────────────┬───────────────────────────┬──────────────┘
                  │                           │
        ┌─────────▼─────────┐       ┌────────▼──────────┐
        │   macOS backend   │       │   Linux backend   │
        │ host-isolated     │       │ VM-isolated       │
        │ runtime path      │       │ runtime path      │
        └─────────┬─────────┘       └────────┬──────────┘
                  │                           │
        ┌─────────▼─────────┐       ┌────────▼──────────┐
        │ Host primitives   │       │ Guest VM + agent  │
        │ (sandboxing/net)  │       │ execution service │
        └───────────────────┘       └───────────────────┘
```

Component responsibilities:

- Control plane: sandbox lifecycle, managed process lifecycle, policy resolution, snapshots, session reuse
- Backends: OS-specific isolation and execution implementation
- Network enforcement: evaluates and applies `none`/`allowlist`/`full` policy
- Agent execution service: guest or sidecar command and file operations where required by the backend
- User interfaces: CLI, gRPC clients, and adapter layers

## Platform Matrix

| Platform | Backend | Status | Notes |
| --- | --- | --- | --- |
| macOS (Apple Silicon) | Apple backend (`auto`) | Supported | Backend selected automatically; allowlist availability depends on host/runtime capabilities |
| Linux (KVM-capable) | Firecracker backend (`auto`) | Supported | Enforces firewall-based policies when firewall enforcement is enabled |
| Any OS | Local backend (`HYPERBOX_BACKEND=local`) | Dev-only fallback | Not isolated like VM backends; not recommended for production isolation |

## CLI Reference

Top-level commands:

- `run`: execute a command in sandbox
- `ps`: list managed processes
- `logs`: read managed process logs
- `wait`: wait for a managed process to finish
- `cancel`: cancel a managed process
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
HB=hyperbox
SBX=$($HB create --name myproj --workspace "$PWD" --profile locked)
$HB run --name myproj --cmd "python3 -V"
$HB run --name myproj --cmd "pytest -q" --detach
$HB ps
$HB list
$HB destroy --name myproj

# Snapshot lifecycle
SBX=$($HB create --workspace "$PWD")
SNAP=$($HB snapshot create --sandbox-id "$SBX")
$HB destroy --sandbox-id "$SBX"
$HB snapshot restore --snapshot-id "$SNAP"

# Managed process lifecycle
PROC=$($HB run --name myproj --cmd "pytest -q" --detach --json | jq -r '.process.process_id')
$HB logs "$PROC"
$HB wait "$PROC"
```

For complete command flags:

```bash
hyperbox --help
hyperbox run --help
hyperbox proxy --help
hyperbox snapshot --help
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
HB=hyperbox
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

## SDKs

The gRPC control plane is the stable integration surface.

Current in-repo examples and tooling focus on:

- CLI-driven human workflows
- gRPC client integrations
- adapter-style agent integrations

TypeScript SDK work is planned to expose the same managed-process model directly to external agent runtimes.

## Performance and Benchmarks

Benchmark tooling and historical reports are included in the repository. Treat reported numbers as environment-specific baselines and rerun benchmarks in your own setup before making decisions.

## Troubleshooting

### `server does not support 'list' yet (likely stale daemon)`

Restart the local control plane and retry:

```bash
pkill -f hyperbox || true
hyperbox list
```

### `failed to connect to hyperbox control plane`

Ensure server autostart is enabled (default) or start manually:

```bash
hyperbox serve --addr 127.0.0.1:50051
```

### `sandbox ... already has a running managed process`

If you target a busy sandbox directly through lower-level APIs, Hyperbox rejects the second managed process.

The CLI `run` path handles this for you by creating a fresh sandbox and telling you that it did so.

### `invalid allowlist ...`

Use explicit hostnames only:

- valid: `example.com`, `api.github.com`
- invalid: `*.example.com`, `https://example.com`, `example.com:443`

### `command not found` inside sandbox

Install tool in the persistent sandbox session first:

```bash
HB=hyperbox
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"
```

## Security Notes and Current Limits

- Allowlist entries are explicit hostnames only; wildcard patterns are rejected.
- On macOS, allowlist enforcement is currently DNS/domain-based (host-level direct-IP firewall blocking is not yet implemented).
- `--workspace` intentionally maps host files into sandbox context; writes there are host-visible by design.
- `HYPERBOX_BACKEND=local` is a non-isolated fallback intended for local development/testing only.

## Development

```bash
cargo fmt --all
cargo test --workspace
cargo build --workspace
```

## Further Reading

- `docs/` for extended quickstarts, architecture notes, protocol details, and integration guidance

## License

MIT
