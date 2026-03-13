# hyperbox

Run code in an isolated sandbox with reusable state, managed processes, and explicit network policy.

Use `hyperbox` when you need a real execution environment for agents, SDKs, or humans without putting dependencies, files, and network access on the host.

Use it for:

- agents that should run code without touching the host
- long-running jobs you want to detach from and inspect later
- reusable sandboxes where installs, files, and state persist across commands
- teams that want one control plane for CLI, SDKs, and agent integrations

## Why Use It

| Need | What you get |
| --- | --- |
| Safe default execution | isolated sandbox with `network=none` unless you opt in |
| Reuse across runs | deterministic sessions and named sandboxes |
| Long-running delegation | `run --detach`, `logs`, `wait`, `cancel` |
| Clear network control | `none`, `allowlist`, `full` with `--explain` |
| Portable control plane | same model for humans, SDKs, and agent tooling |
| Fast recovery | snapshot create / restore workflows |

## Benchmark Snapshot

Hyperbox has been run against the real [ComputeSDK benchmarks](https://github.com/computesdk/benchmarks) flow: create a fresh sandbox, run the first command, destroy it.

| Provider | Median TTI | Score | Position |
| --- | ---: | ---: | --- |
| Daytona | 0.11s | 98.00 | leader |
| E2B | 0.24s | 95.31 | leader |
| Blaxel | 0.48s | 94.28 | leader |
| Hyperbox | 0.92s | 89.2 | near Runloop / Hopx tier |
| Runloop | 0.89s | 90.61 | nearby |
| Hopx | 0.88s | 89.53 | nearby |

Full notes are in [Performance and Benchmarks](#performance-and-benchmarks).

## How It Works

```
Clients
  hyperbox CLI | SDKs | Agent adapters
          |
          v   gRPC / stdio JSON-lines
Control plane
  runtime, sandboxes, processes, snapshots, policy
          |
          +--> macOS backend
          |      host-isolated runtime path
          |      -> host primitives (sandboxing/net)
          |
          +--> Linux backend
                 VM-isolated runtime path
                 -> guest VM + agent execution service
```

- the control plane owns sandbox lifecycle, managed processes, snapshots, session reuse, and policy
- macOS and Linux enforce isolation differently, but expose the same sandbox and process model
- the CLI, SDKs, and agent integrations all talk to the same server contract

## Common Workflows

### One-off isolated command

```bash
hyperbox run --cmd "python3 -c 'print(2 + 2)'"
```

Use this for one-off commands, untrusted scripts, or quick checks where no state needs to survive.

### Reusable environment with installed tools and files

```bash
hyperbox create --name demo --workspace "$PWD" --profile full
hyperbox run --name demo --cmd "python3 -m pip install pytest"
hyperbox run --name demo --cmd "pytest -q"
hyperbox run --name demo --cmd "python3 -c 'open(\"build.txt\", \"w\").write(\"sandbox state\")'"
hyperbox run --name demo --cmd "cat build.txt"
```

Use this when installs, generated files, and working state should still be there on the next command.

### Detach from work and come back later

```bash
PROC=$(hyperbox run --name demo \
  --cmd "python3 -c 'import time; print(\"start\"); time.sleep(30); print(\"done\")'" \
  --detach --json | jq -r '.process.process_id')

hyperbox ps
hyperbox logs "$PROC"
hyperbox wait "$PROC"
```

Use this when the command should outlive the terminal session or API call that started it.

### Restrict outbound network access

On some macOS hosts, `allowlist` may be unavailable. If that happens, use `network=none|full` for now and run `hyperbox setup`.

```bash
hyperbox run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://example.com', timeout=8).status)\""

hyperbox run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; urllib.request.urlopen('https://github.com', timeout=8)\""
```

The second command should fail because `github.com` is not on the allowlist.

Use `--explain` to see what the host is actually enforcing.

### Allow subdomains without allowing the apex domain

```bash
hyperbox run --network allowlist --allow '*.example.com' \
  --cmd "python3 -c \"import socket; print(socket.gethostbyname('www.example.com'))\""

hyperbox run --network allowlist --allow '*.example.com' \
  --cmd "python3 -c \"import socket; print(socket.gethostbyname('example.com'))\""
```

The first command should succeed. The second should fail because `*.example.com` means subdomains only.

## Core Concepts

- sessions and affinity: `hyperbox run` reuses a deterministic session unless you pass `--ephemeral`; `create --name` gives you a stable sandbox you can target directly
- managed processes: `run` starts a tracked process; `run --detach`, `logs`, `wait`, and `cancel` are the normal control surface for long work
- templates: each sandbox starts from a template such as `python:3.12`; run `hyperbox templates` to see what is available
- network policy: `none`, `allowlist`, and `full` are part of sandbox policy; `--explain` shows what the current host can enforce
- workspace behavior: `--workspace <PATH>` mounts an existing host tree; without it, Hyperbox creates sandbox-managed storage

## Setup

### macOS

```bash
hyperbox setup
```

### From source

```bash
cargo build -p hyperbox-cli
export PATH="$PWD/target/debug:$PATH"
hyperbox --help
```

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
- `snapshot`: `create`, `restore`, `list`

Common flows:

```bash
# Create, reuse, destroy
hyperbox create --name myproj --workspace "$PWD" --profile locked
hyperbox run --name myproj --cmd "python3 -V"
hyperbox run --name myproj --cmd "pytest -q" --detach
hyperbox ps
hyperbox list
hyperbox destroy --name myproj

# Snapshot lifecycle
hyperbox create --name snapdemo --workspace "$PWD"
hyperbox run --name snapdemo --cmd "echo snapshot-state > snap.txt"
SNAP=$(hyperbox snapshot create --name snapdemo --json | jq -r '.snapshot_id')
hyperbox destroy --name snapdemo
hyperbox snapshot restore --snapshot-id "$SNAP"

# Managed process lifecycle
PROC=$(hyperbox run --name myproj --cmd "pytest -q" --detach --json | jq -r '.process.process_id')
hyperbox logs "$PROC"
hyperbox wait "$PROC"

# Cancel a long-running task
PROC=$(hyperbox run --name myproj --cmd "python3 -c 'import time; time.sleep(300)'" --detach --json | jq -r '.process.process_id')
hyperbox cancel "$PROC"
```

For complete command flags:

```bash
hyperbox --help
hyperbox run --help
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
hyperbox run --profile team_web --cmd "python3 -m pip install requests"
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

The gRPC control plane is the integration surface.

In-repo clients:

- the `hyperbox` CLI
- Python SDK in [`hyperbox-py`](hyperbox-py)
- TypeScript SDK in [`hyperbox-ts`](hyperbox-ts)

Both SDKs use the same managed-process control plane as the CLI.

TypeScript example:

```ts
import { HyperboxClient } from "@hyperbox/sdk";

const client = new HyperboxClient("127.0.0.1:50051");
const result = await client.run({
  template: "python:3.12",
  command: "python3 -c 'print(2 + 2)'",
});

console.log(result.status, result.stdout);

await client.close();
```

Python example:

```python
from hyperbox import HyperboxClient

with HyperboxClient("127.0.0.1:50051") as client:
    result = client.run(
        template="python:3.12",
        command="python3 -c 'print(6 * 7)'",
        ephemeral=True,
    )
    print(result.process.status, result.stdout)
```

## Performance and Benchmarks

Hyperbox was run against the [ComputeSDK benchmarks](https://github.com/computesdk/benchmarks) using the real benchmark flow:

1. create a fresh sandbox
2. run `node -v`
3. destroy the sandbox

No session reuse or benchmark-only shortcuts were used.

This was the local macOS result used during development:

| Provider | Median TTI | Score | Position |
| --- | ---: | ---: | --- |
| Daytona | 0.11s | 98.00 | leader |
| E2B | 0.24s | 95.31 | leader |
| Blaxel | 0.48s | 94.28 | leader |
| Hyperbox (local macOS) | 0.92s | 89.2 | near Runloop / Hopx tier |
| Runloop | 0.89s | 90.61 | nearby |
| Hopx | 0.88s | 89.53 | nearby |

Context:

- Hyperbox was measured locally on macOS with a small sample during development
- the published ComputeSDK leaderboard was measured on their own infrastructure with larger samples
- treat this as directional positioning, not a claimed official rank

Takeaway:

- Hyperbox is not the fastest cold-start sandbox in that benchmark today
- it is in the next competitive tier rather than far off the board
- the remaining gap is mostly backend startup cost, not obvious control-plane waste

Benchmark tooling and historical reports are included in the repository. Rerun them in your own environment before making decisions.

## Troubleshooting

### CLI says the control plane is stale or unreachable

If you just rebuilt `hyperbox` or changed versions, an older local daemon may still be running.

Restart it and retry:

```bash
pkill -f hyperbox || true
hyperbox list
```

If you run the control plane manually:

```bash
hyperbox serve --addr 127.0.0.1:50051
```

### Allowlist mode is unavailable or behaving unexpectedly

Check what Hyperbox is enforcing on this host:

```bash
hyperbox run --network allowlist --allow example.com --cmd "true" --explain
```

On macOS, `hyperbox setup` installs and verifies the runtime prerequisites used by the built-in helper:

```bash
hyperbox setup
```

If the host cannot enforce allowlists yet, use `network=none` or `network=full`.

### `invalid allowlist ...`

Use hostnames or strict wildcard subdomain patterns:

- valid: `example.com`, `api.github.com`, `*.example.com`
- invalid: `*example.com`, `https://example.com`, `example.com:443`

### `command not found` inside sandbox

Install tool in the persistent sandbox session first:

```bash
hyperbox run --profile full --cmd "python3 -m pip install pytest"
hyperbox run --profile full --cmd "pytest -q"
```

## Security Notes and Current Limits

- On macOS helper-managed networking, allowlist entries may include wildcard subdomain patterns like `*.example.com`; the apex domain must be listed separately if needed.
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
