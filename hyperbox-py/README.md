# hyperbox-py

Python wrapper around the `hyperbox` CLI.

## Install

```bash
pip install -e hyperbox-py
```

## Usage

```python
from hyperbox import Sandbox

# Sandbox context now creates one persistent sandbox and reuses it
# across all commands. By default it maps to the current workspace dir.
with Sandbox(template="python:3.12", workspace=".") as box:
    result = box.exec("ls -la")
    print(result.stdout)
    print(box.sandbox_id)

# Optional remote server mode
with Sandbox(server_url="http://127.0.0.1:50051") as box:
    bench = box.bench("python3 -c 'print(1)'", runs=10, warmup=2)
    print(bench.p95_ms)
```
