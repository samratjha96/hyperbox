# Competitive Analysis: Existing Sandbox Solutions

## Summary

The market has no solution that combines: self-hosted + cross-platform + fast + secure + simple. This is the gap hyperbox fills.

## Comparison Matrix

| Solution | Type | Open Source | Self-Host | Isolation | Cold Start | macOS | Network Isolation |
|----------|------|-------------|-----------|-----------|------------|-------|-------------------|
| **hyperbox** | Self-hosted | Yes | Yes | VM (Firecracker/Apple) | <200ms | Yes | Yes (allowlist) |
| **E2B** | Cloud | Yes | Complex | Firecracker | ~400ms | No | Unknown |
| **OpenSandbox** | Self-hosted | Yes | Yes | Docker (runc) | TBD | Via Docker | Yes |
| **Modal** | Cloud | No | No | gVisor | <1s | No | Yes |
| **CodeSandbox** | Cloud | No | No | Firecracker | 2-2.7s | No | Partial |
| **Kata Containers** | Self-hosted | Yes | Yes | microVM | 150-300ms | No | Via CNI |
| **gVisor** | Self-hosted | Yes | Yes | Syscall | ~2.2s | No | Via runtime |
| **Docker** | Self-hosted | Yes | Yes | Namespace | ~550ms | Yes | No isolation |

## Detailed Analysis

### E2B (e2b.dev)
- **Strengths**: Open source, Firecracker-based, good SDK
- **Weaknesses**: Cloud-first, self-hosting complex, no macOS
- **Pricing**: $0.05-0.08/hr, $100 free credits
- **Cold start**: ~400ms (API), ~150ms (microVM only)

### OpenSandbox (Alibaba) - Primary Competitor
- **Strengths**: Open source, multi-language SDKs, K8s native
- **Weaknesses**: Docker/runc only (NOT Firecracker), new/unproven
- **Isolation**: Claims gVisor/Firecracker support but ships runc only
- **Key gap**: No real VM isolation, just containers

### Modal
- **Strengths**: gVisor isolation, GPU support, mature
- **Weaknesses**: Proprietary, no self-hosting, vendor lock-in
- **Pricing**: $30/mo free credits, consumption-based

### CodeSandbox
- **Strengths**: Firecracker-based, mature product
- **Weaknesses**: Slow (2-2.7s cold), proprietary, dev-focused
- **Not designed for**: AI agent code execution

## Why Build hyperbox?

### Gap Analysis

| Need | E2B | OpenSandbox | Modal | hyperbox |
|------|-----|-------------|-------|----------|
| Self-hosted | ⚠️ Complex | ✅ | ❌ | ✅ |
| VM isolation | ✅ | ❌ runc only | ⚠️ gVisor | ✅ Firecracker |
| macOS native | ❌ | ❌ | ❌ | ✅ Apple VZ |
| Fast cold start | ✅ | ❓ Unknown | ✅ | ✅ |
| Network allowlist | ❓ | ✅ | ✅ | ✅ |
| Simple setup | ❌ | ✅ | ✅ | ✅ |

### The Real Gap
> "A simple, self-hosted, cross-platform sandbox with VM-level isolation"

**Nobody has this:**
- E2B: Cloud-first, complex self-hosting
- OpenSandbox: runc only, no real VM isolation
- Modal: Proprietary, no self-hosting
- Kata/gVisor: Complex, Linux-only

## OpenSandbox Deep Dive (Primary Competitor)

### What They Claim
- Multi-language SDKs (Python, Kotlin, TypeScript, C#)
- Kubernetes operator with warm pools
- Network egress control
- "Secure runtime" support (gVisor, Firecracker)

### What They Actually Ship
- **Docker/runc isolation only** - NOT Firecracker
- Secure runtimes are "implementing" (not shipped)
- Jupyter-based code execution (adds latency)
- Go-based execution daemon (~50MB)

### Their Architecture
```
SDK → Sandbox Server (FastAPI) → Docker/K8s
                                      ↓
                              Container (runc)
                              - execd (Go daemon)
                              - Jupyter Server
```

### Their Benchmarks
| Metric | OpenSandbox |
|--------|-------------|
| Code execution | 50-200ms |
| Exec daemon memory | ~50MB |
| 100 sandbox allocation | 0.92s (warm pool) |
| DNS proxy latency | <1ms |

## hyperbox vs OpenSandbox

| Aspect | OpenSandbox | hyperbox | Winner |
|--------|-------------|----------|--------|
| **Isolation** | Docker/runc | Firecracker VM | hyperbox |
| **Security** | Container escape possible | Hardware VM | hyperbox |
| **Cold start** | Unknown | <200ms (snapshot) | hyperbox |
| **Exec latency** | 50-200ms (Jupyter) | <20ms (direct) | hyperbox |
| **Agent size** | ~50MB (Go) | <5MB (Rust) | hyperbox |
| **macOS** | Via Docker Desktop | Native Apple VZ | hyperbox |
| **Network** | DNS + nftables | DNS + ipset/userspace | Tie |
| **Maturity** | New (2026) | New | Tie |
| **K8s operator** | Yes | No (future) | OpenSandbox |

### Key Differentiators

1. **Real VM isolation** - OpenSandbox uses runc, we use Firecracker
2. **No Jupyter overhead** - Direct exec, not via kernel protocol
3. **Native macOS** - Apple Containerization, not Docker Desktop
4. **Rust agent** - 10x smaller, faster startup
5. **Snapshot restore** - Sub-100ms warm starts

## Target Positioning

**hyperbox**: "The secure, cross-platform sandbox for AI agents"

- For teams who need **real isolation** (not just containers)
- For developers on **macOS** who want native performance
- For orgs who need **self-hosted** (compliance, cost, control)
- For anyone who wants **simplicity** (one binary, not K8s required)
