# hyperbox-py

Python SDK for Hyperbox.

## Install

```bash
pip install -e hyperbox-py
```

## Usage

### gRPC SDK (recommended)

```python
from hyperbox import HyperboxClient

with HyperboxClient("127.0.0.1:50051") as client:
    result = client.run(
        template="python:3.12",
        command="python3 -c 'print(2 + 2)'",
    )
    print(result.process.status, result.stdout)

    started = client.start_run(
        create_config={"template": "python:3.12"},
        command="python3 -c 'import time; print(\"detached\"); time.sleep(1)'",
    )
    completed = client.wait_process(started.process.process_id, timeout_secs=5)
    stdout = client.read_process_log(started.process.process_id, "stdout")
    print(completed.status, stdout.contents)
```

### CLI wrapper

```python
from hyperbox import Sandbox

with Sandbox(server_url="http://127.0.0.1:50051") as box:
    result = box.exec("python3 -c 'print(1)'")
    print(result.exit_code)
```
