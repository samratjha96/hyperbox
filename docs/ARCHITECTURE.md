# Architecture

hyperbox is organized around a simple execution model:

1. create or reuse a sandbox
2. start a managed process inside it
3. read logs, wait, cancel, or reconnect later

This applies to humans, agents, and SDKs.

## Core Objects

### Sandbox

- persistent execution environment
- reusable filesystem and installed tools
- explicit network and resource policy

### Process

- one managed foreground process per sandbox
- durable id and persisted status
- stdout and stderr spooled to files
- visible after client disconnects and server restarts

## Control Plane Responsibilities

The control plane is responsible for:

- sandbox lifecycle
- managed process lifecycle
- session and affinity reuse
- snapshot metadata and restore flows
- network policy resolution
- exposing one consistent CLI and gRPC contract

## Backend Responsibilities

Backends are responsible for:

- creating and destroying isolated sandboxes
- executing commands and file operations inside a sandbox
- enforcing the available isolation model on the host OS

The control plane uses those backend primitives to supervise managed processes rather than requiring every backend to expose a separate long-running process API.

## Execution Flow

```text
client
  |
  v
hyperbox control plane
  |- resolve or create sandbox
  |- start managed process
  |- persist process metadata
  |- write stdout/stderr to spool files
  |- serve logs/status/wait/cancel
  v
backend-specific sandbox runtime
```

## Lifecycle Rules

- `hyperbox run` starts a managed process and waits by default.
- `hyperbox run --detach` starts the same managed process but returns immediately.
- `hyperbox logs`, `hyperbox wait`, and `hyperbox cancel` operate on that process id.
- One sandbox can have one managed foreground process at a time.
- If a run targets a busy sandbox, the control plane creates a fresh sandbox for that run and reports the overflow explicitly.

## Durability Model

Persisted state includes:

- active sandboxes
- affinity bindings
- snapshots
- managed process records

This allows:

- reconnect after client disconnect
- status lookup after command completion
- log retrieval after the original caller exits

## What hyperbox is not

hyperbox is not trying to be:

- a cluster scheduler
- a multi-process orchestrator inside one sandbox
- a separate artifact platform

The product goal is narrower: durable isolated execution with an obvious operator and agent interface.
