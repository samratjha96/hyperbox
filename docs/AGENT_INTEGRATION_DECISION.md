# Agent Integration Decision

Date: 2026-03-05
Status: In Progress

## Problem

We want coding agents (Claude/Codex style) to feel like they are running normal commands (`grep`, `git`, `pytest`, file edits) while execution is isolated by hyperbox.

Key tension:
- Best UX is "just run normal tools on the repo."
- Best security is "strong boundary between agent process/secrets and untrusted execution."

## Decision

Use a **control-plane-first architecture** as the default:

1. Agent runs outside hyperbox.
2. Agent tool calls are routed to a persistent hyperbox sandbox session.
3. Commands become managed processes inside that sandbox.
4. Sandbox uses workspace-aware execution (`workspace_dir`) so commands run against the repo context.

Also support **agent-in-sandbox compatibility mode** for tools we cannot instrument.

## Why this is the right default

### 1) Matches successful agent products

- Claude Code emphasizes OS-enforced sandboxing around bash commands, with explicit filesystem/network boundaries and an escape hatch that can be policy-disabled.
- Codex CLI exposes explicit sandbox policies (`read-only`, `workspace-write`, `danger-full-access`) and approval modes.
- Copilot coding agent runs in a restricted sandbox environment with controlled internet and governance controls.

This industry pattern is: **control plane outside + constrained execution environment inside**.

### 2) Preserves secrets and control-plane reliability

If the full coding agent runs inside sandbox, model/API credentials and orchestration state must enter the sandbox boundary. That increases blast radius and operational fragility.

### 3) Preserves "normal command" ergonomics

With persistent sessions, workspace mapping, and managed processes, commands still behave like normal shell usage (`pwd`, `ls`, `grep`, `git`, etc.), without per-command sandbox creation overhead.

## Modes

### Mode A (default): Tool Proxy

- Agent process on host.
- Hyperbox session per task/branch.
- Commands/files routed to sandbox via control API.
- `workspace_dir` set to repo path (or mirrored copy mode).

Best for:
- Security + maintainability + enterprise policy enforcement.

### Mode B (compatibility): Agent In Sandbox

- Launch the agent process itself inside hyperbox.
- Mount workspace into sandbox.
- Pass only minimal required credentials.

Best for:
- Closed agents where command/file hooks cannot be redirected.
- Quick "yolo but isolated" workflows.

## Workspace strategy

Support three workspace modes:

1. `shared` (fastest DX): sandbox reads/writes directly to mounted repo path.
2. `overlay` (safer default for autonomous runs): sandbox writes to copy-on-write layer; explicit `commit` exports patch/changes.
3. `mirror` (strongest separation): rsync/clone into sandbox; sync back at checkpoints.

Default recommendation:
- Interactive local agent: `shared`.
- Autonomous long-run / high-risk tasks: `overlay`.

## Required control-plane contract

Hyperbox integration interface should expose:

- `create_session(template, workspace_mode, workspace_path, network_policy, ttl)`
- `run(session_id, command)` for synchronous workflows
- `start_process(session_id, command)` for detached workflows
- `logs(process_id)` / `wait(process_id)` / `cancel(process_id)`
- `read_file` / `write_file` / `apply_patch`
- `snapshot` / `restore`
- `destroy_session`

Behavior:
- Persistent sandbox state.
- Durable process ids for delegated work.
- Deterministic exit codes.
- Reconnectable logs and status.
- Structured audit log of commands + approvals + policy denials.

## Security model

- Default deny egress; allow explicit domains.
- Treat allowlist as enforce-or-fail: do not silently accept policies that are not enforced by the active backend.
- Enforce filesystem boundaries at sandbox layer.
- Keep escape hatch (`unsandboxed`) disabled by default in managed mode.
- Keep credentials outside sandbox in Mode A.
- Explicit policy profiles:
  - `read-only`
  - `workspace-write`
  - `danger-full-access` (requires manual approval and audit event)

## Implementation plan (incremental)

1. Add adapters:
   - MCP server adapter (for MCP-capable agents),
   - CLI wrapper adapter (for non-MCP agents).
2. Keep `hyperbox proxy` only as an adapter helper, not as the primary execution model.
3. Add workspace modes (`shared`, `overlay`, `mirror`) on top of existing `workspace_dir`.
4. Add policy profiles + audit events.
5. Add `hyperbox run-agent` for Mode B compatibility if needed.

## Success criteria

- Agent can complete multi-step coding tasks without noticing sandbox plumbing.
- No per-command sandbox recreation in normal runs.
- Long-running commands can be delegated and reattached later.
- Repositories are accessible end-to-end (`git`, build tools, tests).
- Policy denials are clear and recoverable.
- Security posture remains strict by default.
