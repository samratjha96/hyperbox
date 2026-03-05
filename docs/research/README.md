# Research Index

This directory contains validated research informing hyperbox's design decisions.

## Documents

| # | Topic | Key Finding |
|---|-------|-------------|
| [01](01-firecracker-platform-requirements.md) | Firecracker Platform | Requires KVM; works on bare metal, EC2 .metal, GCP nested virt |
| [02](02-macos-virtualization-options.md) | macOS Virtualization | Use Apple Containerization (macOS 26+) or Virtualization.framework |
| [03](03-cold-start-benchmarks.md) | Cold Start Times | Fresh boot ~150ms; snapshot restore <30ms; target <200ms |
| [04](04-oci-image-handling.md) | OCI Images | Pre-baked templates (like E2B); add lazy pull later if needed |
| [05](05-network-isolation.md) | Network Isolation | Linux: nftables+ipset; macOS: userspace via VZFileHandle |
| [06](06-ai-agent-requirements.md) | Agent Requirements | Fast exec, file I/O, limited network, agent-outside-sandbox |
| [07](07-competitive-analysis.md) | Competition | OpenSandbox ships runc only; we ship real VM isolation |

## Key Decisions from Research

### Platform Support
- **Linux**: Firecracker on KVM (bare metal, EC2 .metal, GCP with nested virt)
- **macOS**: Apple Containerization Framework (macOS 26+), Virtualization.framework fallback

### Performance Strategy
- Pre-baked templates for fast cold starts
- Snapshot restore for <30ms warm starts
- Warm pools for instant allocation

### Image Strategy
- Start with pre-baked templates (python, node, go, rust, ubuntu)
- NOT arbitrary Docker images initially
- Add OCI pull later if demand exists

### Network Strategy
- Three modes: none (default), allowlist, full
- DNS proxy + IP tracking for allowlist
- Platform-specific filtering (nftables vs userspace)

### Why We Beat OpenSandbox
1. VM isolation (Firecracker) vs container (runc)
2. Direct exec vs Jupyter overhead
3. Rust agent (<5MB) vs Go daemon (~50MB)
4. Native macOS vs Docker Desktop
