# hyperbox-py

Python SDK for Hyperbox.

## Install

```bash
pip install -e hyperbox-py
```

## Usage

### gRPC SDK (recommended)

```python
from hyperbox import HyperboxClient, SandboxSession

with HyperboxClient("127.0.0.1:50051") as client:
    with SandboxSession(client, template="python:3.12", workspace=".") as box:
        result = box.exec("ls -la")
        print(result.stdout)
        print(box.sandbox_id)
```

### CLI wrapper (backward compatible)

```python
from hyperbox import Sandbox

with Sandbox(server_url="http://127.0.0.1:50051") as box:
    result = box.exec("python3 -c 'print(1)'")
    print(result.exit_code)
```
