# Quickstart

## 1) One-time setup (macOS)

```bash
cargo build -p hyperbox-cli
./target/debug/hyperbox setup
```

`setup` installs runtime prerequisites when needed and starts Apple `container` system services.

## 2) Immediate value (2 minutes)

```bash
HB=./target/debug/hyperbox
```

Default secure mode (network blocked):

```bash
$HB run --cmd "python3 -c 'print(2 + 2)'"
```

Allow only one domain:

```bash
$HB run --network allowlist --allow example.com --cmd "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://example.com', timeout=10).status)\""
```

Show the inverse (blocked domain):

```bash
$HB run --network allowlist --allow example.com --cmd "python3 -c \"import urllib.request; urllib.request.urlopen('https://github.com', timeout=8)\""
```

The last command should fail with DNS/connection error because `github.com` is not allowlisted.

## 3) Stateful speed (install once, reuse many times)

Install dependencies once inside the sandbox session, then reuse them on later runs:

```bash
$HB run \
  --profile full \
  --cmd "python3 -m pip install pytest"

$HB run \
  --profile full \
  --cmd "pytest -q"
```

`run` reuses a deterministic session by default (faster repeats, no create/destroy per call). Use `--ephemeral` for one-off execution.

## 4) Scale patterns (teams and long-running workflows)

Team profiles (`~/.hyperbox/profiles.toml` by default):

```toml
[profiles.team_web]
network = "allowlist"
allow = ["github.com", "pypi.org", "files.pythonhosted.org"]
```

```bash
$HB run --profile team_web --cmd "python3 -m pip install requests"
```

Long-running named workspace sandbox:

```bash
$HB create --name myproj --workspace "$PWD" --profile team_web
$HB run --name myproj --cmd "pytest -q"
$HB destroy --name myproj
```

The same commands work in CI and automation. Point `HB` at the installed binary path and run the same profile/name pattern.

## Use built-in and custom profiles

```bash
# Built-ins: locked, web, full
$HB run --profile locked --cmd "python3 -V"

# Mix and match: profile defaults + explicit overrides
$HB run --profile locked --network allowlist --allow github.com --cmd "curl -I https://github.com"

# Inverse check: non-allowlisted domains fail
$HB run --network allowlist --allow example.com --cmd "python3 -c \"import urllib.request; urllib.request.urlopen('https://github.com', timeout=8)\""
```

Custom profiles are loaded from `~/.hyperbox/profiles.toml` by default (or from `--profile-config <path>`):

```toml
[profiles.team_web]
network = "allowlist"
allow = ["github.com", "pypi.org", "files.pythonhosted.org"]
```

```bash
$HB run --profile team_web --cmd "python3 -m pip install requests"
```

Default behavior:
- `hyperbox run/create/proxy` now go through the control-plane server path by default (not direct local backend calls).
- Backend selection is `auto` by default.
- On macOS, `auto` mode requires VM-capable backend. If Apple runtime prerequisites are missing, commands fail fast (no silent fallback).
- On Linux, Firecracker backend now auto-starts an embedded `hyperbox-agent` sidecar by default when the configured agent endpoint is unavailable.
- Overrides remain available for power users through `HYPERBOX_BACKEND`, `HYPERBOX_APPLE_RUNTIME`, and `HYPERBOX_APPLE_HELPER`.

## Keep a persistent sandbox for an agent workflow

```bash
# Starts local control plane automatically if needed and returns sandbox id.
SANDBOX_ID=$($HB create --workspace "$PWD")

# Reuse the same sandbox across many commands.
$HB run --sandbox-id "$SANDBOX_ID" --cmd "ls -la"
$HB run --sandbox-id "$SANDBOX_ID" --cmd "pytest -q"

# Cleanup when done.
$HB destroy --sandbox-id "$SANDBOX_ID"
```

## Use Affinity + Snapshot Restore

```bash
# Create a named sandbox bound to this workspace.
$HB create --name myproj --workspace "$PWD"

# Reuse by name.
$HB run --name myproj --cmd "python3 -V"

# Save a snapshot for later restore.
SNAPSHOT_ID=$($HB snapshot create --name myproj)
echo "$SNAPSHOT_ID"

# Tear down the active sandbox.
$HB destroy --name myproj

# Restore from snapshot automatically when name has no active sandbox.
$HB run --name myproj --cmd "pwd"

# You can also restore explicitly by id.
$HB snapshot restore --snapshot-id "$SNAPSHOT_ID"
```

## Open an interactive shell

```bash
# Ephemeral shell sandbox (auto-create + auto-destroy, workspace defaults to $PWD).
$HB shell

# Optional: customize template/network/workspace for ephemeral shell.
$HB shell --template python:3.12 --workspace "$PWD"

# Attach to an existing sandbox.
SANDBOX_ID=$($HB create --workspace "$PWD")
$HB shell --sandbox-id "$SANDBOX_ID"
```

Notes:
- `shell` without `--sandbox-id` creates a temporary sandbox and destroys it after the shell exits.
- `shell` is supported for Apple backend sandboxes (built-in helper path) and Firecracker backend sandboxes (agent stream path).

## Run in proxy mode for agent adapters

```bash
# Starts a persistent sandbox and serves JSON-lines protocol on stdio.
$HB proxy --workspace "$PWD"
```

## Write and read artifacts

```bash
$HB run \
  --template python:3.12 \
  --cmd "cat input.txt > output.txt" \
  --write input.txt=hello \
  --read output.txt
```

## Capability probe

```bash
$HB probe
```

## Python SDK

```bash
pip install -e hyperbox-py
python -c "from hyperbox import Sandbox; print(Sandbox().run_python('print(42)').stdout)"
```

## Caveats

- Apple built-in helper enforces `network=allowlist` via sandbox-scoped DNS allowlist sidecar (domain-based enforcement). Host-level direct-IP firewall enforcement is planned.
- Workspace mode (`--workspace`) maps the sandbox working directory to an existing host directory (for agent-style repo workflows).
- Firecracker backend maps each sandbox to `HYPERBOX_AGENT_ROOT/<sandbox-id>` for exec/file I/O. With `--workspace`, this path is linked to the provided workspace directory so agent operations run against the repo directly.
- LocalBackend rejects `network=allowlist` and `network=full` unless explicitly bypassed for local dev (`HYPERBOX_LOCAL_ALLOW_UNENFORCED_NETWORK=1`).
- Apple backend supports `network=none`, `network=allowlist`, and `network=full` when helper-managed runtime is active.
- Firecracker agent auto-start can be disabled via `HYPERBOX_AGENT_AUTOSTART=0` (then `HYPERBOX_AGENT_ENDPOINT` must already be serving).
- On macOS, backend runtime selection can be forced with `HYPERBOX_APPLE_RUNTIME=containerization|virtualization`; auto mode prefers containerization only when available on host.
- Apple backend supports a helper bridge command (`HYPERBOX_APPLE_HELPER`) for native runtime integration; protocol is documented in `docs/APPLE_HELPER_PROTOCOL.md`.
- Built-in helper command: `export HYPERBOX_APPLE_HELPER=\"hyperbox apple-helper\"` (uses Apple `container` CLI and bind-mounts `--workspace` to `/workspace`).
- Apple backend is helper-only now: if `HYPERBOX_BACKEND=apple` and helper is not configured, sandbox creation fails fast. In auto mode on macOS, backend selection uses Apple runtime only when helper is configured.
