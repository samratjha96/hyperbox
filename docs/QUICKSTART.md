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
cargo run -p hyperbox-cli -- run --template python:3.12 --cmd "python3 -c 'print(2 + 2)'"
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
