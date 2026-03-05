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
