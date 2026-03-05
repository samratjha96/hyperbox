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
```
