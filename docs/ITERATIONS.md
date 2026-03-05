# Implementation Iterations

1. Bootstrap workspace and crate skeleton.
2. Add core config/types/error primitives.
3. Define backend trait contract.
4. Implement template registry and validation.
5. Add structured execution trace/process models.
6. Define shared agent protocol and codecs.
7. Implement network policy parser/evaluator.
8. Add Linux capability probe for Firecracker prerequisites.
9. Add macOS capability probe and backend selection hints.
10. Implement local backend for sandbox lifecycle + command execution.
11. Add warm pool manager for pre-provisioned sandbox reuse.
12. Add server runtime API for sandbox lifecycle/exec.
13. Build CLI (`run`, `templates`, `probe`).
14. Add metrics collection with p50/p95 execution latency.
15. Add snapshot abstractions and in-memory snapshot store.
16. Add file read/write server APIs and CLI artifact flow.
17. Add Python SDK + CLI JSON response mode.
18. Add CLI integration tests.
19. Add architecture and quickstart documentation.
20. Final polish: fix serde protocol path type, fix percentile math, tighten Linux probe shell checks, format, and full workspace tests.

21. Add shared protobuf contracts and `hyperbox-proto` crate.
22. Implement `hyperbox-server` gRPC control service and binary.
23. Implement `hyperbox-agentd` gRPC daemon with exec/file APIs.
24. Add reusable gRPC control client wrapper.
25. Add CLI remote mode (`--server-url`) over gRPC.
26. Add Firecracker Unix-socket API client with typed endpoint calls.
27. Add Firecracker VM process lifecycle manager and snapshot hooks.
28. Implement Firecracker backend wired to agent gRPC operations.
29. Add nftables/ipset firewall planning + executor abstraction.
30. Add DNS allowlist proxy with NXDOMAIN enforcement.
31. Integrate network apply/teardown hooks in Firecracker backend.
32. Add Apple backend scaffolding with gRPC agent path.
33. Add backend auto-selection (local/firecracker/apple).
34. Move snapshot create/restore into runtime API.
35. Add warm-pool auto-refill + restore fallback hooks.
36. Add CLI benchmark command with p50/p95/mean metrics.
37. Extend Python SDK with remote mode and benchmark support.
38. Add end-to-end gRPC server/client integration tests.
39. Add disk template manifests and loader validation.
40. Final integration polish: Rust 2024 unsafe-env fixes, enum normalization, and green workspace test suite.
