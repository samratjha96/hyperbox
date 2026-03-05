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
- On macOS, Apple backend is auto-selected only when the selected helper can run on this host (today: built-in helper requires Containerization framework support); otherwise auto falls back to local backend.
- Overrides remain available for power users through `HYPERBOX_BACKEND`, `HYPERBOX_APPLE_RUNTIME`, and `HYPERBOX_APPLE_HELPER`.

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

## Attach an interactive shell to a running sandbox

```bash
SANDBOX_ID=$(cargo run -p hyperbox-cli -- create --workspace "$PWD")
cargo run -p hyperbox-cli -- shell --sandbox-id "$SANDBOX_ID"
```

Notes:
- `shell` is supported for Apple backend sandboxes (built-in helper path), Firecracker backend sandboxes (agent stream path), and local backend sandboxes.
- If Apple backend is requested but not runnable on the host (for example missing helper/runtime support), server falls back to local backend automatically.

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

- Firecracker VM runtime is not yet integrated; this repository currently uses a local backend for execution.
- Network allowlist is currently policy-evaluated in libraries; host firewall enforcement is planned for Linux/macOS backend integration.
- Workspace mode (`--workspace`) maps the sandbox working directory to an existing host directory (for agent-style repo workflows).
- `network=allowlist` and `network=full` are rejected by LocalBackend and Apple backend unless explicitly bypassed for local dev (`HYPERBOX_LOCAL_ALLOW_UNENFORCED_NETWORK=1`). This prevents false-positive security behavior.
- On macOS, backend runtime selection can be forced with `HYPERBOX_APPLE_RUNTIME=containerization|virtualization`; auto mode prefers containerization only when available on host.
- Apple backend supports a helper bridge command (`HYPERBOX_APPLE_HELPER`) for native runtime integration; protocol is documented in `docs/APPLE_HELPER_PROTOCOL.md`.
- Built-in helper command: `export HYPERBOX_APPLE_HELPER=\"hyperbox apple-helper\"` (uses Apple `container` CLI and bind-mounts `--workspace` to `/workspace`).
- Apple backend is helper-only now: if `HYPERBOX_BACKEND=apple` and helper is not configured, sandbox creation fails fast. In auto mode on macOS, backend selection uses Apple runtime only when helper is configured.
