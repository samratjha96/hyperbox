# Quickstart

## Build

```bash
cargo build --workspace
```

## Run tests

```bash
cargo test --workspace
```

## Run a command in sandbox

```bash
cargo run -p hyperbox-cli -- run --template python:3.12 --workspace "$PWD" --cmd "python3 -c 'print(2 + 2)'"
```

Default behavior:
- `hyperbox run/create/proxy` now go through the control-plane server path by default (not direct local backend calls).
- Backend selection is `auto` by default.
- On macOS, `auto` mode requires VM-capable backend. If Apple runtime prerequisites are missing, commands fail fast (no silent fallback).
- On Linux, Firecracker backend now auto-starts an embedded `hyperbox-agent` sidecar by default when the configured agent endpoint is unavailable.
- Overrides remain available for power users through `HYPERBOX_BACKEND`, `HYPERBOX_APPLE_RUNTIME`, and `HYPERBOX_APPLE_HELPER`.

## One-time runtime setup (macOS)

```bash
cargo run -p hyperbox-cli -- setup
```

This installs Apple container runtime prerequisites (when missing) and starts `container system`.

## Keep a persistent sandbox for an agent workflow

```bash
# Starts local control plane automatically if needed and returns sandbox id.
SANDBOX_ID=$(cargo run -p hyperbox-cli -- create --workspace "$PWD")

# Reuse the same sandbox across many commands.
cargo run -p hyperbox-cli -- run --sandbox-id "$SANDBOX_ID" --cmd "ls -la"
cargo run -p hyperbox-cli -- run --sandbox-id "$SANDBOX_ID" --cmd "pytest -q"

# Cleanup when done.
cargo run -p hyperbox-cli -- destroy --sandbox-id "$SANDBOX_ID"
```

## Open an interactive shell

```bash
# Ephemeral shell sandbox (auto-create + auto-destroy, workspace defaults to $PWD).
cargo run -p hyperbox-cli -- shell

# Optional: customize template/network/workspace for ephemeral shell.
cargo run -p hyperbox-cli -- shell --template python:3.12 --workspace "$PWD"

# Attach to an existing sandbox.
SANDBOX_ID=$(cargo run -p hyperbox-cli -- create --workspace "$PWD")
cargo run -p hyperbox-cli -- shell --sandbox-id "$SANDBOX_ID"
```

Notes:
- `shell` without `--sandbox-id` creates a temporary sandbox and destroys it after the shell exits.
- `shell` is supported for Apple backend sandboxes (built-in helper path) and Firecracker backend sandboxes (agent stream path).

## Run in proxy mode for agent adapters

```bash
# Starts a persistent sandbox and serves JSON-lines protocol on stdio.
cargo run -p hyperbox-cli -- proxy --workspace "$PWD"
```

## Write and read artifacts

```bash
cargo run -p hyperbox-cli -- run \
  --template python:3.12 \
  --cmd "cat input.txt > output.txt" \
  --write input.txt=hello \
  --read output.txt
```

## Capability probe

```bash
cargo run -p hyperbox-cli -- probe
```

## Python SDK

```bash
pip install -e hyperbox-py
python -c "from hyperbox import Sandbox; print(Sandbox().run_python('print(42)').stdout)"
```

## Caveats

- Network allowlist is currently policy-evaluated in libraries; host firewall enforcement is planned for Linux/macOS backend integration.
- Workspace mode (`--workspace`) maps the sandbox working directory to an existing host directory (for agent-style repo workflows).
- Firecracker backend maps each sandbox to `HYPERBOX_AGENT_ROOT/<sandbox-id>` for exec/file I/O. With `--workspace`, this path is linked to the provided workspace directory so agent operations run against the repo directly.
- `network=allowlist` and `network=full` are rejected by LocalBackend and Apple backend unless explicitly bypassed for local dev (`HYPERBOX_LOCAL_ALLOW_UNENFORCED_NETWORK=1`). This prevents false-positive security behavior.
- Firecracker agent auto-start can be disabled via `HYPERBOX_AGENT_AUTOSTART=0` (then `HYPERBOX_AGENT_ENDPOINT` must already be serving).
- On macOS, backend runtime selection can be forced with `HYPERBOX_APPLE_RUNTIME=containerization|virtualization`; auto mode prefers containerization only when available on host.
- Apple backend supports a helper bridge command (`HYPERBOX_APPLE_HELPER`) for native runtime integration; protocol is documented in `docs/APPLE_HELPER_PROTOCOL.md`.
- Built-in helper command: `export HYPERBOX_APPLE_HELPER=\"hyperbox apple-helper\"` (uses Apple `container` CLI and bind-mounts `--workspace` to `/workspace`).
- Apple backend is helper-only now: if `HYPERBOX_BACKEND=apple` and helper is not configured, sandbox creation fails fast. In auto mode on macOS, backend selection uses Apple runtime only when helper is configured.
