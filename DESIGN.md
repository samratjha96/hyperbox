# hyperbox

**Secure, cross-platform sandbox for AI agent code execution**

Ship what OpenSandbox only proposes. Beat them on every metric that matters.

---

## Why hyperbox?

| | OpenSandbox | hyperbox |
|---|-------------|----------|
| **Isolation** | Docker/runc (container) | Firecracker VM (hardware) |
| **Security** | Container escapes possible | VM-level isolation |
| **Exec latency** | 50-200ms (via Jupyter) | <20ms (direct) |
| **Agent size** | ~50MB (Go) | <5MB (Rust) |
| **macOS** | Via Docker Desktop | Native Apple Virtualization |
| **Cold start** | Unknown | <200ms (snapshot restore) |

**The gap we fill:** OpenSandbox claims Firecracker/gVisor support but ships Docker/runc only. We ship real VM isolation from day one.

---

## The Demo

```python
from hyperbox import Sandbox

# Create isolated sandbox with network allowlist
with Sandbox(
    template="python:3.12",
    memory_mb=512,
    network=["api.openai.com", "pypi.org"]
) as box:
    
    # Install packages (pypi.org allowed)
    box.exec("pip install requests")
    
    # Run untrusted code safely
    result = box.run_python("""
import requests
r = requests.get("https://api.openai.com/v1/models",
                 headers={"Authorization": "Bearer sk-..."})
print(r.status_code)
    """)
    
    print(result.stdout)         # "200"
    print(f"{result.duration_ms}ms")  # "<20ms"
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     hyperbox-server                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │ Pool Manager│  │  Snapshots  │  │  Metrics    │             │
│  └─────────────┘  └─────────────┘  └─────────────┘             │
│                          │                                      │
│            ┌─────────────┴─────────────┐                       │
│            ▼                           ▼                       │
│  ┌──────────────────┐       ┌──────────────────┐               │
│  │ Linux: Firecracker│       │ macOS: Apple VZ  │               │
│  │ - KVM isolation   │       │ - Containerization│               │
│  │ - Snapshots       │       │ - Framework       │               │
│  └────────┬─────────┘       └────────┬─────────┘               │
└───────────┼──────────────────────────┼──────────────────────────┘
            │ vsock                    │ vsock
            ▼                          ▼
┌───────────────────────────────────────────────────────────────┐
│                    hyperbox-agent (<5MB)                       │
│  gRPC server • Process exec • File I/O • Metrics              │
└───────────────────────────────────────────────────────────────┘
```

### Platform Strategy

| Platform | Isolation Technology | Status |
|----------|---------------------|--------|
| Linux (bare metal, EC2 .metal, GCP nested) | Firecracker microVM | Primary |
| macOS (Apple Silicon, macOS 26+) | Apple Containerization Framework | Primary |
| macOS (older) | Virtualization.framework | Fallback |

### Network Isolation

Three modes:
- `none` - Air-gapped (default, most secure)
- `allowlist` - Only specified domains (DNS filtering + IP tracking)
- `full` - Unrestricted (use with caution)

```
Linux: nftables + ipset + DNS proxy
macOS: Userspace filtering via VZFileHandleNetworkDeviceAttachment
```

---

## Benchmarks (Targets)

| Metric | OpenSandbox | hyperbox Target |
|--------|-------------|-----------------|
| Exec latency (warm) | 50-200ms | **<20ms** |
| Cold start | Unknown | **<200ms** |
| Snapshot restore | N/A | **<30ms** |
| Agent memory | ~50MB | **<5MB** |
| 100 sandbox alloc | 0.92s | **<800ms** |

---

## Implementation

### Crate Structure

```
hyperbox/
├── crates/
│   ├── hyperbox-core/         # Traits, types, config
│   ├── hyperbox-firecracker/  # Linux Firecracker backend
│   ├── hyperbox-apple/        # macOS Apple VZ backend
│   ├── hyperbox-agent/        # In-VM agent (Rust, <5MB)
│   ├── hyperbox-server/       # gRPC API, pool management
│   └── hyperbox-network/      # DNS proxy, firewall
├── hyperbox-py/               # Python SDK (uvx hyperbox)
├── templates/                 # Pre-built images
│   ├── python-3.12/
│   ├── node-20/
│   └── ...
└── docs/research/             # Research documentation
```

### Key Design Decisions

1. **Pre-baked templates** (like E2B) - not arbitrary Docker images
   - Covers 90% of use cases
   - Fast cold starts via snapshots
   - Add lazy OCI pull later if needed

2. **Unified agent protocol** - same gRPC over vsock on both platforms
   - Firecracker vsock
   - Apple Containerization vsock
   - Same agent binary

3. **Snapshot-based fast starts**
   - Fresh boot: ~150ms (kernel boot dominates)
   - Snapshot restore: <30ms
   - Warm pools for instant allocation

---

## Supported Templates

| Template | Contents |
|----------|----------|
| `python:3.11` | Python 3.11, pip, common packages |
| `python:3.12` | Python 3.12, pip, common packages |
| `node:18` | Node.js 18, npm |
| `node:20` | Node.js 20, npm |
| `golang:1.22` | Go 1.22 |
| `rust:1.75` | Rust toolchain |
| `ubuntu:22.04` | Basic Ubuntu |

---

## Phases

### Phase 1: Linux MVP (4 weeks)
- [ ] hyperbox-core traits
- [ ] hyperbox-firecracker backend
- [ ] hyperbox-agent (gRPC, exec, file I/O)
- [ ] Basic templates (python, node)
- [ ] CLI for testing
- **Exit**: `hyperbox run python:3.12 -c "print('hello')"` works

### Phase 2: Snapshots + Network (3 weeks)
- [ ] Snapshot create/restore
- [ ] Network isolation (none/allowlist/full)
- [ ] DNS proxy + firewall
- **Exit**: <200ms cold start, network allowlist works

### Phase 3: macOS + SDK (3 weeks)
- [ ] hyperbox-apple (Containerization Framework)
- [ ] hyperbox-server (gRPC API)
- [ ] Python SDK (hyperbox-py)
- **Exit**: Same code works on Linux and macOS

### Phase 4: Polish (2 weeks)
- [ ] Benchmarks vs OpenSandbox
- [ ] Documentation
- [ ] Warm pool manager
- **Exit**: Published, benchmarks show wins

---

## Risks

| Risk | Mitigation |
|------|------------|
| Firecracker requires KVM | Document supported platforms clearly |
| Apple Containerization is macOS 26+ | Virtualization.framework fallback |
| Cold start > 200ms | Snapshot restore, warm pools |
| Network isolation complexity | Start with none/full, add allowlist |

---

## Research

Detailed research in `docs/research/`:
- [01-firecracker-platform-requirements.md](docs/research/01-firecracker-platform-requirements.md)
- [02-macos-virtualization-options.md](docs/research/02-macos-virtualization-options.md)
- [03-cold-start-benchmarks.md](docs/research/03-cold-start-benchmarks.md)
- [04-oci-image-handling.md](docs/research/04-oci-image-handling.md)
- [05-network-isolation.md](docs/research/05-network-isolation.md)
- [06-ai-agent-requirements.md](docs/research/06-ai-agent-requirements.md)
- [07-competitive-analysis.md](docs/research/07-competitive-analysis.md)

---

## Quick Reference

```bash
# Development
cargo build --workspace
cargo test --workspace

# Build agent (static, for Linux guest)
cross build --release --target x86_64-unknown-linux-musl -p hyperbox-agent

# Run server
cargo run -p hyperbox-server

# Python SDK
cd hyperbox-py && pip install -e .

# Use
hyperbox run python:3.12 -c "print('hello')"
```
