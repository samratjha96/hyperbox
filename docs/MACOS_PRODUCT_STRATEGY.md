# Hyperbox macOS Obvious-Solution Spec

## Purpose

Define the smallest set of product and UX changes that make Hyperbox the obvious default for local agent/code sandboxing on macOS.

This spec intentionally avoids feature bloat. It prioritizes:

- clear security benefit by default
- minimal cognitive load
- fast path to first success

## Product Positioning

Hyperbox on macOS should feel like:

- "safe by default"
- "works with one command"
- "easy to understand when something is blocked"

Not:

- a policy language users must learn
- a large surface area with many expert-only switches

## North Star User Experience

User can run:

```bash
hyperbox run --cmd "pytest -q"
```

and immediately see:

- that execution is VM-isolated
- that network is blocked by default
- what writable scope exists
- exactly how to relax constraints for this run

## Scope (Minimal Feature Set)

### 1) Secure-by-default runtime summary

Every `run`, `shell`, and `create` prints a short "Effective Isolation" summary before execution:

- backend + isolation class (VM/local)
- network mode + enforcement status
- writable paths
- timeout

This must be concise and deterministic.

### 2) Apple allowlist enforcement completion

On Apple backend, `network=allowlist` must be truly enforced (not parsed-only, not best-effort).

If enforcement is unavailable, command must fail closed with a clear error and next step.

### 3) Golden-path CLI simplification

Primary commands remain:

- `hyperbox run`
- `hyperbox shell`
- `hyperbox create`

Behavior goals:

- zero-config first run succeeds on supported macOS
- server lifecycle is implicit for normal usage
- advanced backend/runtime flags remain available but not required

### 4) Explainability mode

Add `--explain` (or `hyperbox explain <operation>`) to print:

- backend selection decision and reason
- effective policy knobs (network, mounts, env pass-through)
- enforcement status ("enforced" vs "not enforced")

Explain output must be machine-readable with `--json`.

### 5) Opinionated defaults + custom profiles

Ship three first-class built-ins that map to existing controls:

- `locked` (offline/strict default)
- `web` (allowlist egress only)
- `full` (explicitly permissive, clearly marked)

Also allow user-defined profiles in config (`~/.hyperbox/profiles.toml` by default), while keeping runtime behavior simple:

- profiles provide defaults
- explicit flags (`--network`, `--allow`) can override profile defaults
- no new policy DSL or backend-specific branching

### 6) Stateful developer flow via snapshots

Make a simple, discoverable path for stateful use:

- named sandbox + snapshot create/restore
- one-line guidance in command output when a sandbox is reused

No new snapshot primitives required; focus on flow and discoverability.

## Non-Goals

- no new general policy DSL
- no plugin ecosystem
- no large matrix of preset profiles
- no backend expansion work unrelated to macOS clarity/usability

## CLI UX Contract

### Default contract

If user does not specify network flags:

- network mode is `none`
- output explicitly states this

### Relaxation contract

When a command fails due to network restrictions, output must include one safe escalation path:

- for temporary full access: `--network full`
- for scoped access: `--network allowlist --allow <domain>`

### Unsafe-mode contract

When user selects permissive settings, output must clearly mark it as reduced isolation.

## Acceptance Criteria

### A. First-run clarity

1. `hyperbox run --cmd "echo ok"` prints an effective-isolation summary.
2. Summary includes backend, network mode, and writable scope.
3. Output is visible by default and parseable with `--json`.

### B. Default security posture

1. With no network flags, outbound network is blocked.
2. Blocking behavior is consistent across `run`, `shell`, and `create`-then-exec flows.

### C. Apple allowlist correctness

1. `--network allowlist --allow example.com` allows `example.com`.
2. Non-allowlisted domains are blocked.
3. If allowlist cannot be enforced on host/runtime, command fails closed with explicit reason.

### D. Golden-path simplicity

1. A new user can execute common commands using only `run/shell/create`.
2. No required manual daemon management for normal local workflows.

### E. Explainability

1. `--explain` shows backend selection reason and enforcement status.
2. `--explain --json` provides stable fields for automation/tests.

### F. Profile usability

1. `--profile <name>` works for `run`, `create`, and `shell`.
2. Built-ins (`locked|web|full`) and custom config profiles are both supported.
3. Profile defaults and explicit override behavior are documented and test-covered.

### G. Stateful flow discoverability

1. Snapshot create/restore path is discoverable from normal CLI output/help.
2. A user can persist and restore a named environment without reading deep docs.

## Implementation Notes (Minimal Churn)

- prefer aliasing existing flags over introducing new control planes
- keep profile implementation in CLI layer where possible
- avoid backend-specific behavior drift in user-facing messaging
- add integration tests that validate visible UX text and enforcement behavior

## Suggested Rollout Order

1. Effective-isolation summary + `--explain`
2. Apple allowlist enforcement
3. Profile aliases (`locked`, `web`, `full`)
4. Snapshot flow discoverability improvements

## Definition of Done

Hyperbox is "obviously better" on macOS when:

1. default behavior is visibly safer than host-level wrappers
2. the common path requires almost no decisions
3. users can immediately understand what is enforced and why
