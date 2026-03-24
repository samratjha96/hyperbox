# hyperbox

Plug-and-play secure execution runtime for AI agent harnesses.

Hyperbox gives you one control plane for running untrusted code with explicit network policy, reusable sessions, managed background processes, and transparent isolation behavior.

## Why Hyperbox

- Fast iterative loop: reusable sessions avoid recreate/destroy overhead.
- Security-first defaults: `network=none` unless you opt in.
- Explicit policy controls: `none`, `allowlist`, `full`.
- Managed process lifecycle: detach, list, stream logs, wait, cancel.
- Harness-ready interface: CLI + gRPC + stdio adapter mode.
- Clear policy introspection: `--explain` shows backend + enforcement state.

## Enterprise Fit

| Control area | Hyperbox behavior today |
| --- | --- |
| Network boundary | Locked by default (`network=none`) |
| Policy expansion | Explicit opt-in (`allowlist` / `full`) |
| Failure semantics | Invalid/unsupported policy combinations fail clearly |
| Isolation transparency | `--explain` prints effective backend and enforcement |
| Repeatability | Deterministic reusable sessions + snapshot lifecycle |
| Long-running tasks | First-class managed process commands (`ps/logs/wait/cancel`) |

Hyperbox is the execution/control layer. Org-specific approval, identity, and compliance workflows are typically layered by your host platform.

## Quickstart

```bash
cargo build -p hyperbox-cli
HB=./target/debug/hyperbox

# Locked networking by default
$HB run --cmd "python3 -c 'print(2 + 2)'"

# Reuse environment across commands
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"
```

## Core Workflows

### Reusable sandbox session

```bash
HB=./target/debug/hyperbox
$HB create --name demo --workspace "$PWD" --template auto
$HB run --name demo --cmd "python3 -V"
$HB destroy --name demo
```

### Managed background process

```bash
HB=./target/debug/hyperbox
OUT=$($HB run --cmd "sleep 30" --detach --json)
PROC_ID=$(python -c 'import json,sys;print(json.loads(sys.argv[1])["process"]["process_id"])' "$OUT")
$HB ps
$HB logs "$PROC_ID"
$HB wait "$PROC_ID" --json
$HB cancel "$PROC_ID"
```

### Network allowlist (exact hosts and wildcard subdomains)

```bash
HB=./target/debug/hyperbox
$HB run --network allowlist --allow example.com --cmd "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://example.com', timeout=8).status)\""
$HB run --network allowlist --allow '*.example.com' --cmd "python3 -c \"import socket; print(socket.gethostbyname('www.example.com'))\""
```

## Template Auto-Detection

- `--template auto` resolves runtime from command/workspace hints.
- `run`, `create`, and adapter mode default to `auto`.
- Explicit override is always available (`--template python:3.12`).

Built-in template families include Python, Node, Go, Rust, and Ubuntu base.

## Security Model

- Default profile is locked (`network=none`).
- Allowlist validates host entries and rejects invalid forms.
- Unsupported policy/backend combos fail instead of silently downgrading.
- Local backend is explicitly non-isolated dev mode.
- `--workspace` intentionally maps host files into sandbox context.

## Platform Matrix

| Platform | Backend | Status | Notes |
| --- | --- | --- | --- |
| macOS (Apple Silicon) | `apple` (`auto`) | Supported | Backend selected automatically; allowlist availability depends on runtime path |
| Linux (KVM-capable) | `firecracker` (`auto`) | Supported | VM backend with firewall-based policy controls |
| Any OS | `local` | Dev fallback | Non-isolated; intended for development/testing |

## CLI Surface

- `run`: execute command in sandbox (`--detach` for managed process mode)
- `create`: create persistent sandbox
- `destroy`: destroy by id or affinity name
- `list`: list active sandboxes
- `inspect`: inspect sandbox metadata
- `shell`: interactive shell attach/create
- `templates`: list templates
- `ps`: list managed processes
- `logs`: stream/read process logs
- `wait`: wait for process completion
- `cancel`: cancel managed process
- `snapshot`: `create`, `restore`, `list`
- `serve`, `probe`, `setup`: host/runtime operations

For command flags:

```bash
./target/debug/hyperbox --help
./target/debug/hyperbox run --help
./target/debug/hyperbox create --help
./target/debug/hyperbox ps --help
./target/debug/hyperbox snapshot --help
```

## Real E2E Validation

Run the real-user smoke matrix:

```bash
./scripts/e2e_real_smoke.sh
```

This validates:

- create/destroy and session reuse
- template detection (manifest + command hints)
- ensure-once behavior
- key edge failures (policy misconfiguration and invalid templates)

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

```bash
pip install -e hyperbox-py
```

```python
from hyperbox import HyperboxClient, SandboxSession

with HyperboxClient("127.0.0.1:50051") as client:
    with SandboxSession(client, template="python:3.12", workspace=".") as box:
        out = box.exec("python3 -c 'print(42)'")
        print(out.stdout)
```

## Troubleshooting

```bash
# Restart local control plane
pkill -f hyperbox || true
./target/debug/hyperbox list
```

```bash
# Start control plane manually
./target/debug/hyperbox serve --addr 127.0.0.1:50051
```

## Development

```bash
cargo fmt --all
cargo build --workspace
cargo test --workspace
```

## Further Reading

- `docs/QUICKSTART.md`
- `docs/ARCHITECTURE.md`
- `docs/APPLE_HELPER_PROTOCOL.md`
- `docs/AGENT_INTEGRATION_DECISION.md`

## License

MIT
