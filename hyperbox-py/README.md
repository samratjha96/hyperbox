# hyperbox-py

Python wrapper around the `hyperbox` CLI.

## Install

```bash
pip install -e hyperbox-py
```

## Usage

```python
from hyperbox import Sandbox

with Sandbox(template="python:3.12", network=["pypi.org", "api.openai.com"]) as box:
    result = box.run_python("print('hello from hyperbox')")
    print(result.stdout)

# Optional remote server mode
with Sandbox(server_url="http://127.0.0.1:50051") as box:
    bench = box.bench("python3 -c 'print(1)'", runs=10, warmup=2)
    print(bench.p95_ms)
```
