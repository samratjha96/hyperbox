# SlicerVM Gap Analysis for hyperbox

Date: 2026-03-05

## Target Architecture (Slicer-style)

1. VM-based execution on Linux/macOS with low startup latency.
2. Persistent control plane + guest agent API (`exec`, file ops, forwarding).
3. Enforced network isolation modes:
   - `none`
   - `allowlist` (domain/IP policy, default deny)
   - `full`
4. Per-sandbox isolation with strong cleanup/reconciliation.

## Current Status

### Implemented

1. Persistent sandbox lifecycle (`create`, `run --sandbox-id`, `destroy`) in CLI/SDK.
2. Workspace-aware execution (`workspace_dir`) wired through config + gRPC.
3. Firecracker/Apple backends scaffolded with shared control API shape.
4. Enforce-or-fail network gating:
   - Local backend rejects `allowlist/full` by default.
   - Apple backend rejects networked modes.
   - Firecracker rejects networked modes when firewall is dry-run.
5. Apple helper bridge protocol implemented in backend + docs.
6. Built-in helper executable (`hyperbox apple-helper`) implemented for macOS `container` CLI:
   - persistent session lifecycle (`create`, `exec`, `read`, `write`, `destroy`)
   - workspace bind mount to `/workspace`
   - `--network none` enforced for current backend policy
7. Apple backend now fails fast if helper is missing (`HYPERBOX_APPLE_HELPER` required).
8. Auto backend selection on macOS only chooses Apple backend when helper is configured.
9. Helper protocol integration test added (`hyperbox-cli/tests/apple_helper_integration.rs`).

### Missing / Partial

1. Firecracker allowlist data path (domain -> IP set population and refresh) is incomplete.
2. DNS allowlist proxy is not yet wired into VM runtime path.
3. Per-sandbox namespace lifecycle + reconciliation logic is not complete end-to-end.
4. Apple `virtualization` runtime helper implementation is missing (current built-in helper supports `containerization` runtime only).
5. Cross-platform parity: macOS network policy backend is not implemented.
6. Auditability: policy decision/audit event stream is not yet explicit.

## Execution Plan

1. Implement Firecracker allowlist data path:
   - resolve exact allowlist domains at create time,
   - populate per-VM `ipset`,
   - reject wildcard entries until DNS proxy-driven dynamic updates are added.
2. Wire DNS proxy + dynamic set refresh:
   - force DNS path through host proxy,
   - update nft/ipset with TTL-aware entries.
3. Add reconciliation loop:
   - remove orphan tables/sets/netns,
   - ensure policy teardown after crashes.
4. Implement macOS network policy backend (PF/NetworkExtension strategy).
5. Add E2E tests:
   - allowlisted destination succeeds,
   - non-allowlisted destination fails.
