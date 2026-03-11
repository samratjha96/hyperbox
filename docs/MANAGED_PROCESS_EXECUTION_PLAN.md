# Managed Process Execution Plan

Date: 2026-03-11
Status: In Progress

Implemented so far:

- durable managed process state and persistence
- managed process runtime lifecycle (`start`, `logs`, `wait`, `cancel`)
- gRPC process APIs
- CLI process commands (`run --detach`, `ps`, `logs`, `wait`, `cancel`)
- busy-sandbox overflow handling in the CLI `run` path

## Goal

Make hyperbox a durable sandbox that can run:

- quick commands that feel immediate
- detached long-running tasks that can run for hours or days
- the same workflow for humans, agents, and SDKs

The primary product promise is:

1. create or reuse a sandbox
2. run a managed process inside it
3. stream or fetch logs later
4. wait, cancel, inspect, and reconnect after disconnects

## North Star

`hyperbox run "pytest -q"` and `hyperbox run --detach "python train.py"` should use the same underlying lifecycle.

Quick commands stay simple.

Long-running work becomes durable and reconnectable.

## Non-Goals

- No backward compatibility work for the current unary `exec` model.
- No general scheduler, queueing, priorities, or fair sharing.
- No first-class artifact object separate from the filesystem.
- No multi-process orchestration inside one sandbox.
- No automatic package installation or hidden environment mutation.

## Core Model

Hyperbox should expose two durable objects:

1. `Sandbox`
- persistent environment
- reusable filesystem and installed tools
- explicit network and resource policy

2. `Process`
- one managed foreground process per sandbox
- durable id
- persisted status, timestamps, exit code
- stdout and stderr spooled to files
- can be observed after client or server restart

This is intentionally narrower than a general job platform.

## Invariants

1. A sandbox can have at most one managed foreground process at a time.
2. All managed process output is file-backed.
3. `run` is a convenience wrapper over the managed process API.
4. Files remain the artifact model.
5. If a targeted sandbox is busy, hyperbox creates a new sandbox and reports that explicitly.

## Why One Managed Process Per Sandbox

This keeps the model operationally simple:

- no ambiguity about which process owns logs, exit code, or cancellation
- no interference between concurrent managed workloads in the same mutable environment
- clearer resource accounting
- simpler agent and SDK behavior

Scale should come from more sandboxes, not from packing more managed workloads into one sandbox.

## Busy Sandbox Behavior

If a caller targets a sandbox that already has a running managed process, the command should still succeed.

Behavior:

1. hyperbox creates a fresh sandbox
2. starts the new managed process there
3. returns both the requested and resolved sandbox ids
4. marks the disposition as `created_due_to_busy`

This avoids failed runs while making the overflow explicit.

## Lifecycle

### Sandbox lifecycle

- `ready`
- `busy`
- `stopped`
- `failed`

`busy` means a managed foreground process is running.

### Process lifecycle

- `starting`
- `running`
- `succeeded`
- `failed`
- `cancelled`
- `lost`

`lost` is for restart recovery when hyperbox expected a running process but could not reconcile it with backend state.

### State transitions

Allowed transitions:

- `starting -> running`
- `starting -> failed`
- `running -> succeeded`
- `running -> failed`
- `running -> cancelled`
- `running -> lost`

Disallowed transitions should fail loudly and be covered by tests.

## Output Model

Managed process output is written to spool files:

- `stdout`
- `stderr`

Required behavior:

- append-only writes
- fetch by byte offset
- follow mode for live output
- recoverable after client disconnect
- recoverable after server restart

There is no separate logging subsystem in this phase.

## TTL Policy

TTL cleanup exists from day one.

### Never clean up while active

- running managed processes
- busy sandboxes

### Process record TTL

- completed process records expire automatically
- default retention: 7 days
- includes metadata and spool logs

### Sandbox TTL

1. Explicitly named persistent sandboxes
- never auto-delete the sandbox itself
- completed process records inside them still expire

2. Automatically created overflow sandboxes
- eligible for deletion after process completion and TTL expiry

3. Anonymous one-off sandboxes
- delete immediately after completion in sync flows unless caller asks to keep them

This keeps the common path clean without deleting user-owned environments.

## API Shape

The current unary `exec` contract should stop being the primary interface.

### Primary API

1. `StartProcess`
- input:
  - command
  - target sandbox id or affinity name, optional
  - sandbox config for create-if-needed
  - detach flag
  - timeout policy
  - keep sandbox policy for anonymous sandboxes
- output:
  - process id
  - resolved sandbox id
  - requested sandbox id, optional
  - sandbox disposition
  - process status
  - started at

2. `GetProcess`
- input:
  - process id
- output:
  - full process info

3. `ListProcesses`
- input:
  - sandbox id, optional
  - status filter, optional
  - include expired soon, optional
- output:
  - process summaries

4. `ReadProcessLogs`
- input:
  - process id
  - stream: stdout or stderr or both
  - offset
  - follow flag
- output:
  - chunks
  - next offset
  - eof

5. `WaitProcess`
- input:
  - process id
  - timeout, optional
- output:
  - terminal process info

6. `CancelProcess`
- input:
  - process id
- output:
  - final or current process info

### Required response fields

All process-facing responses should expose:

- `process_id`
- `sandbox_id`
- `status`
- `exit_code`, when terminal
- `started_at`
- `finished_at`, when terminal
- `sandbox_disposition`

### Sandbox disposition values

- `reused_existing`
- `created_new`
- `created_due_to_busy`

These values should be structured fields, not strings buried in human-readable output.

## CLI Shape

The CLI should stay extremely obvious.

### Primary commands

1. `hyperbox run <command>`
- synchronous
- streams logs live
- exits with child exit code

2. `hyperbox run --detach <command>`
- starts process
- prints process id and sandbox id

3. `hyperbox ps`
- list managed processes

4. `hyperbox logs <process-id>`
- read logs

5. `hyperbox logs <process-id> --follow`
- follow logs live

6. `hyperbox wait <process-id>`
- wait for completion

7. `hyperbox cancel <process-id>`
- cancel process

### Required CLI UX rules

1. Quick commands must not feel like batch jobs.
2. If a new sandbox was created because the requested sandbox was busy, print that explicitly.
3. Default output should be compact and readable.
4. `--json` should expose the full structured contract for agents and scripts.
5. Help text must explain sync vs detach in plain language.

## SDK Shape

The TypeScript SDK should be an early deliverable because it forces the right contract.

### Minimum SDK surface

- `run(options)`
- `start(options)`
- `getProcess(processId)`
- `listProcesses(filters?)`
- `logs(processId, options)`
- `wait(processId, options)`
- `cancel(processId)`
- `getSandbox(sandboxId)`
- `listSandboxes()`

### SDK rules

1. `run` wraps `start + stream + wait`
2. detached and synchronous flows must share one underlying process model
3. `AbortSignal` support is required
4. busy-sandbox overflow must be exposed as structured metadata

## Backend Responsibilities

The backend implementation must provide:

1. spawn managed process
2. capture stdout and stderr to spool files
3. persist process metadata before returning success
4. reconcile running processes after restart
5. update terminal state exactly once
6. cancel the managed process tree, not just a parent pid

This is the hardest part of the redesign.

## Restart Recovery

On server startup:

1. hydrate persisted sandboxes
2. hydrate persisted non-terminal processes
3. reconcile each running process with backend reality
4. if process is still alive:
- mark `running`
- resume log reads from spool files
5. if process is gone and final state is known:
- mark terminal state
6. if process cannot be reconciled cleanly:
- mark `lost`
- preserve logs and metadata

The system must prefer loud, explicit recovery states over silent guesses.

## Data Model

Minimum persisted process fields:

- `process_id`
- `sandbox_id`
- `requested_sandbox_id`, optional
- `sandbox_disposition`
- `command`
- `status`
- `pid` or backend process handle metadata
- `stdout_path`
- `stderr_path`
- `exit_code`, optional
- `started_at`
- `finished_at`, optional
- `expires_at`

No artifact table is needed in this phase.

## Migration Plan

### Phase 0: Spec lock

Deliverables:

- this design doc
- process state machine
- CLI and SDK contract

Tests:

- none yet

Exit criteria:

- no ambiguity about busy sandbox overflow
- no ambiguity about TTL
- no ambiguity about sync vs detach

### Phase 1: Core process model

Deliverables:

- process ids, states, metadata
- persisted process store
- one-managed-process-per-sandbox enforcement

Tests:

- creating a process record persists correctly
- invalid state transitions are rejected
- terminal state is persisted
- second managed process in the same sandbox is rejected at the core layer so higher layers can handle overflow explicitly

Exit criteria:

- durable process state exists independently of CLI concerns

### Phase 2: Backend supervisor

Deliverables:

- managed process spawn
- spool file output
- completion updates
- cancellation
- restart reconciliation

Tests:

- process transitions from `starting` to `running`
- stdout and stderr are written incrementally
- terminal exit code is persisted
- cancellation terminates the real workload
- restart during a running process preserves observability

Exit criteria:

- no in-memory whole-output buffering
- long-running process survives client disconnect

### Phase 3: API and CLI cutover

Deliverables:

- primary process APIs
- `run`, `ps`, `logs`, `wait`, `cancel`
- JSON output for structured consumers

Tests:

- `run` exits with child exit code
- `run --detach` prints stable identifiers
- `logs --follow` works after reconnect
- `wait` works from a new client process
- busy sandbox creates a new sandbox and reports `created_due_to_busy`

Exit criteria:

- humans can use the new model without learning internals

### Phase 4: TypeScript SDK

Deliverables:

- minimal TS SDK over the primary API
- typed process states and sandbox disposition
- synchronous and detached helpers

Tests:

- sync run end to end
- detached run end to end
- reconnect and continue log reading
- cancellation via `AbortSignal`

Exit criteria:

- an external TS consumer can integrate without shelling out

### Phase 5: Cleanup

Deliverables:

- old unary `exec` path removed or demoted from primary interface
- docs and quickstarts updated
- stale proxy patterns removed if they reinforce the old model

Tests:

- no README or help text points users at the legacy model

Exit criteria:

- hyperbox has one execution mental model

## Milestone Review Checklist

After each phase:

1. confirm the work still serves quick commands and long-running tasks with one model
2. remove duplicate concepts and temporary glue
3. keep names user-facing and implementation-agnostic
4. verify no scheduler or artifact-platform scope creep entered the design
5. verify help text and JSON output still match the actual contract
6. commit the phase as a logical unit

## Open Questions

1. Should sync `run` on an anonymous sandbox delete that sandbox immediately on success by default, or keep it for a short grace period to aid debugging?
2. Should `logs` support merged timestamped output as an option, or only separate stdout and stderr in the first phase?
3. Should named persistent sandboxes expose a `pin` or `protect` bit from day one, or is the name itself enough to suppress sandbox TTL?
