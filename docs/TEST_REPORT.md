# Hyperbox Test Report

Date: 2026-03-05 (America/New_York)
Scope: Validation phases 1-5 (baseline, local E2E, concurrency, network policy, template manifests)

## Full Repro Steps (Copy/Paste)

### Prerequisites

1. Build artifacts available:
```bash
cargo build --workspace
```

2. Python available for SDK/concurrency checks.

3. If your environment blocks localhost networking in sandboxed execution, run service/client commands with elevated permissions.

### Step A: Baseline Regression

```bash
cargo fmt --all -- --check
cargo test --workspace
```

Pass criteria:
- `fmt --check` exits 0
- workspace tests all pass

### Step B: Start Services (Terminal 1 + Terminal 2)

Terminal 1:
```bash
HYPERBOX_AGENT_ADDR=127.0.0.1:60061 \
HYPERBOX_AGENT_ROOT=/tmp/hyperbox-agentd-test \
cargo run -p hyperbox-agent --bin hyperbox-agentd
```

Terminal 2:
```bash
HYPERBOX_BACKEND=local \
HYPERBOX_SERVER_ADDR=127.0.0.1:50051 \
cargo run -p hyperbox-server
```

Pass criteria:
- Both processes start without crashing
- Agent logs show start on `127.0.0.1:60061`
- Server logs show start on `127.0.0.1:50051`

### Step C: Remote CLI + SDK Validation

Terminal 3:
```bash
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 templates
```

```bash
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 run \
  --template python:3.12 \
  --cmd "cat in.txt > out.txt" \
  --write in.txt=ok-remote \
  --read out.txt \
  --json
```

```bash
PYTHONPATH=hyperbox-py python3 - <<'PY'
from hyperbox import Sandbox
box = Sandbox(server_url='http://127.0.0.1:50051', hyperbox_bin='target/debug/hyperbox')
out = box.run_python("print(42)")
print({'exit_code': out.exit_code, 'stdout': out.stdout.strip()})
PY
```

```bash
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 bench \
  --template python:3.12 \
  --cmd "python3 -c 'print(1)'" \
  --runs 20 \
  --warmup 3 \
  --json
```

Pass criteria:
- template list returns expected templates
- file artifact flow returns `out.txt=ok-remote`
- SDK returns `exit_code=0` and `stdout=42`
- benchmark returns valid JSON with p50/p95/mean

### Step D: Concurrency Stress

```bash
python3 - <<'PY'
import concurrent.futures, json, subprocess, time

N=50
URL='http://127.0.0.1:50051'
CMD=['target/debug/hyperbox','--server-url',URL,'run','--template','python:3.12','--cmd','python3 -c "print(7*6)"','--json']

def one(_):
    p = subprocess.run(CMD, capture_output=True, text=True)
    if p.returncode != 0:
        return False
    payload=json.loads(p.stdout)
    return payload.get('exit_code')==0 and payload.get('stdout','').strip()=='42'

start=time.time()
with concurrent.futures.ThreadPoolExecutor(max_workers=12) as ex:
    results=list(ex.map(one, range(N)))
print({'ops':N,'ok':sum(results),'failed':N-sum(results),'wall_s':round(time.time()-start,2)})
PY
```

Pass criteria:
- `failed` should be `0`

### Step E: Network Policy Engine + Template Manifest Checks

```bash
cargo test -p hyperbox-network
cargo run -p hyperbox-cli -- templates --disk-root templates
```

Pass criteria:
- `hyperbox-network` tests pass
- manifest listing prints all expected templates

### Step F: Teardown

1. Stop Terminal 1 and 2 processes with `Ctrl+C`.
2. Optional cleanup:
```bash
rm -rf /tmp/hyperbox-agentd-test
```

## Environment Notes

- Host: macOS (local workspace)
- For localhost gRPC testing, commands required elevated execution because sandbox networking denied localhost bind/connect (`Operation not permitted`).

## Phase 1: Baseline Regression

### Command

```bash
cargo fmt --all -- --check
cargo test --workspace
```

### Result

- PASS
- All workspace unit/integration/doc tests passed.
- No formatting drift.

## Phase 2: Local End-to-End (gRPC + CLI + SDK)

### Services Started

```bash
HYPERBOX_AGENT_ADDR=127.0.0.1:60061 HYPERBOX_AGENT_ROOT=/tmp/hyperbox-agentd-test cargo run -p hyperbox-agent --bin hyperbox-agentd
HYPERBOX_BACKEND=local HYPERBOX_SERVER_ADDR=127.0.0.1:50051 cargo run -p hyperbox-server
```

### Validation Commands + Observed Outputs

```bash
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 templates
```

Observed templates include:
- `python:3.11`, `python:3.12`, `node:18`, `node:20`, `golang:1.22`, `rust:1.75`, `ubuntu:22.04`

```bash
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 run --template python:3.12 --cmd "cat in.txt > out.txt" --write in.txt=ok-remote --read out.txt --json
```

Observed:
- `{"exit_code":0,...,"artifacts":[["out.txt","ok-remote"]]}`

```bash
PYTHONPATH=hyperbox-py python3 - <<'PY'
from hyperbox import Sandbox
box = Sandbox(server_url='http://127.0.0.1:50051', hyperbox_bin='target/debug/hyperbox')
out = box.run_python("print(42)")
print({'exit_code': out.exit_code, 'stdout': out.stdout.strip()})
PY
```

Observed:
- `{'exit_code': 0, 'stdout': '42'}`

```bash
cargo run -p hyperbox-cli -- --server-url http://127.0.0.1:50051 bench --template python:3.12 --cmd "python3 -c 'print(1)'" --runs 20 --warmup 3 --json
```

Observed benchmark:
- `runs=20`, `p50_ms=31`, `p95_ms=32`, `mean_ms=31.25`

### Result

- PASS

## Phase 3: Concurrency / Stress

### Command

- 50 concurrent remote `run --json` operations via Python thread pool against `http://127.0.0.1:50051`.

### Observed Summary

- `ops=50`
- `ok=50`
- `failed=0`
- `wall_s=0.35`
- `mean_ms=54.06`
- `max_ms=68`

### Result

- PASS

## Phase 4: Network Policy Engine Validation

### Command

```bash
cargo test -p hyperbox-network
```

### Covered Assertions

- Wildcard allowlist matching behavior
- Network mode evaluator behavior (`none` / `allowlist` / `full`)
- Firewall plan contains allowlist ipset references
- DNS proxy returns NXDOMAIN for blocked domain

### Result

- PASS (4/4 tests passed)

## Phase 5: Template Manifest Validation

### Command

```bash
cargo run -p hyperbox-cli -- templates --disk-root templates
```

### Observed

Disk manifests were discovered and printed for all expected templates:
- `python:3.11`, `python:3.12`, `node:18`, `node:20`, `golang:1.22`, `rust:1.75`, `ubuntu:22.04`

### Result

- PASS

## Overall Verdict

- Phases 1-5: PASS
- Real remote E2E path (agentd + server + CLI + SDK) validated.
- Remaining advanced validation (Firecracker-on-KVM and privileged nftables/ipset enforcement on Linux host) requires a Linux environment with required virtualization/network privileges.
