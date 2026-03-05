# Apple Helper Bridge Protocol

Date: 2026-03-05

The Apple backend can launch a per-sandbox helper process and communicate over JSON-lines stdio.

Configure helper command:

```bash
export HYPERBOX_BACKEND=apple
export HYPERBOX_APPLE_RUNTIME=containerization   # or virtualization
export HYPERBOX_APPLE_HELPER="/path/to/hyperbox-apple-helper"
```

Each request is one JSON object per line written to helper stdin.
Each response is one JSON object per line written to helper stdout.

## Requests

### Create

```json
{"op":"create","sandbox_id":"...","template":"python:3.12","workspace_dir":"/path","runtime":"containerization"}
```

### Exec

```json
{"op":"exec","sandbox_id":"...","command":["/bin/sh","-lc","echo ok"],"timeout_secs":60}
```

### Read

```json
{"op":"read","sandbox_id":"...","path":"file.txt"}
```

### Write

```json
{"op":"write","sandbox_id":"...","path":"file.txt","bytes_b64":"aGVsbG8="}
```

### Destroy

```json
{"op":"destroy","sandbox_id":"..."}
```

## Responses

### Ack

```json
{"op":"ack"}
```

### Exec result

```json
{"op":"exec","exit_code":0,"stdout":"ok\n","stderr":"","duration_ms":12}
```

### Read result

```json
{"op":"read","bytes_b64":"aGVsbG8="}
```

### Error

```json
{"op":"error","message":"description"}
```

## Contract Notes

1. Helper is the runtime boundary for Apple VM/container lifecycle.
2. Helper must return JSON for every request.
3. On protocol violation, backend returns `execution failed`.
4. Backend sends `destroy` and then terminates helper process on sandbox teardown.
