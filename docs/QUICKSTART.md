# Quickstart

## 1) One-time setup (macOS)

```bash
cargo build -p hyperbox-cli
./target/debug/hyperbox setup
```

Use the built binary directly, or add it to `PATH`:

```bash
HB=./target/debug/hyperbox
```

## 2) Immediate value

Default secure mode blocks external network:

```bash
$HB run --cmd "python3 -c 'print(2 + 2)'"
```

Allow exactly one domain:

```bash
$HB run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; print(urllib.request.urlopen('https://example.com', timeout=10).status)\""
```

Show the inverse:

```bash
$HB run --network allowlist --allow example.com \
  --cmd "python3 -c \"import urllib.request; urllib.request.urlopen('https://github.com', timeout=8)\""
```

The second command should fail because `github.com` is not allowlisted.

## 3) Reuse state between runs

Install once inside the sandbox, then reuse it:

```bash
$HB run --profile full --cmd "python3 -m pip install pytest"
$HB run --profile full --cmd "pytest -q"
```

`run` reuses a deterministic session by default. Use `--ephemeral` for one-off create/execute/destroy behavior.

## 4) Delegate a long-running task

```bash
PROC=$($HB run --profile full \
  --cmd "python3 -c 'import time; print(\"start\"); time.sleep(30); print(\"done\")'" \
  --detach --json | jq -r '.process.process_id')

$HB logs "$PROC"
$HB wait "$PROC"
```

If you need to stop it:

```bash
$HB cancel "$PROC"
```

## 5) Keep an explicit named sandbox

```bash
$HB create --name myproj --workspace "$PWD" --profile full
$HB run --name myproj --cmd "python3 -V"
$HB run --name myproj --cmd "pytest -q" --detach
$HB list
$HB ps
$HB destroy --name myproj
```

If `myproj` is already busy when you call `run`, Hyperbox creates a fresh sandbox for that run and tells you that it did so.

## 6) Snapshots

```bash
SBX=$($HB create --workspace "$PWD")
SNAP=$($HB snapshot create --sandbox-id "$SBX")
$HB destroy --sandbox-id "$SBX"
$HB snapshot restore --snapshot-id "$SNAP"
```

## 7) Useful commands

```bash
$HB list
$HB ps
$HB logs <process-id>
$HB wait <process-id>
$HB cancel <process-id>
$HB probe
```

## Notes

- `run`, `run --detach`, `logs`, `wait`, and `cancel` all operate on the same managed-process model.
- One sandbox can have one managed foreground process at a time.
- Named sandboxes are useful when you want durable state; anonymous runs are better for one-offs.
- `--explain` shows the effective backend and policy behavior for your host.
