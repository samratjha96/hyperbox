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
