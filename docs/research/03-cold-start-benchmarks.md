# Cold Start Benchmarks

## Summary

True cold start under 100ms is not achievable with Linux kernel boot. However, snapshot restore can achieve 4-30ms. Realistic target for a new project: **200-300ms P50 cold start** with warm pools.

## Measured Cold Start Times by Technology

### Firecracker MicroVMs

| Scenario | Time | Notes |
|----------|------|-------|
| Host process startup | ~4ms | Just the Firecracker process |
| **Fresh boot (optimized)** | **125ms** | To guest init on i3.metal |
| Fresh boot (typical) | 160-180ms | Including API overhead |
| Fresh boot (unoptimized) | ~279ms | With more services enabled |
| **Snapshot restore (best)** | **4-10ms** | Optimized, page cache warm |
| Snapshot restore (typical) | 20-30ms | Varies by hardware/kernel |

**Key insight:** Firecracker's ~125ms fresh boot is dominated by Linux kernel boot (~100ms). The VMM itself is negligible (~4ms).

### E2B Sandboxes (Firecracker-based)

| Metric | Time | Notes |
|--------|------|-------|
| MicroVM layer | 125-180ms | Firecracker boot |
| **P50 (median)** | **410ms** | Full API round-trip |
| P95 | 580ms | Including network |
| Same-region | <200ms | Official claim |

**Reality check:** E2B's "150ms" marketing is microVM boot only. Real API calls are 400-600ms.

### CodeSandbox (Firecracker-based)

| Scenario | Time | Notes |
|----------|------|-------|
| **True cold start** | **2-2.7s (P95)** | New environment |
| Snapshot resume | 1.4-2s | "Hibernated" environment |
| Marketing "500ms" | ~500ms | Snapshot resume, best case |

**Reality check:** Their "500ms" is snapshot resume, not cold start.

### gVisor

| Scenario | Time | Notes |
|----------|------|-------|
| Native Docker container | ~50ms | Baseline |
| **gVisor container** | **~2.2s** | 20-50% overhead typical |
| Syscall overhead | 2.8-72× | Depends on operation |
| Memory per container | ~30MB | vs 5MB for Firecracker |

**Verdict:** gVisor is too slow for fast code execution. Good for security, bad for latency.

### Docker Containers (Native)

| Component | Time |
|-----------|------|
| Namespace creation | 8-10ms |
| Cgroup + OverlayFS setup | ~540-560ms |
| **Total (cached image, SSD)** | **554-568ms** |
| Total (HDD) | ~1,157ms |

**Key insight:** Image size is nearly irrelevant. Runtime overhead dominates.

## Time Breakdown Analysis

### Firecracker Fresh Boot (~160ms)
```
Host process startup:     ~4ms    (3%)
Kernel boot:             ~100ms   (56%)  ← bottleneck
Init/userspace ready:    ~20-50ms (28%)
API/network overhead:    ~20-30ms (13%)
─────────────────────────────────────────
Total:                   ~160ms
```

### Firecracker Snapshot Restore (~10-30ms)
```
Snapshot metadata load:   ~1-2ms
Memory mapping (lazy):    ~2-5ms   ← pages demand-paged
vCPU state restore:       ~1-2ms
Resume execution:         ~1ms
─────────────────────────────────────────
Total (optimized):        4-10ms
Total (typical):          20-30ms
```

## Why Sub-100ms Cold Boot is Impossible

The Linux kernel boot process is fundamentally ~100ms:
- Kernel decompression (if using bzImage)
- Hardware initialization
- Driver probing
- Init system startup

**No amount of optimization can significantly reduce this.** The path to fast starts is **snapshot restore**, not fresh boot.

## Snapshot Restore Deep Dive

### What Affects Restore Time
1. **Memory size**: Lazy loading mitigates, but large VMs slower
2. **Host kernel version**: 5.4+ has cgroups v1 issues, use v2
3. **Storage speed**: Snapshot file I/O
4. **Page cache**: Repeated restores faster (7-10ms vs 20-30ms)

### Best Practices
- Use cgroups v2
- Warm the page cache with repeated restores
- Keep snapshot files on fast storage (NVMe)
- Use CoW filesystems for snapshot cloning

## Warm Pool Strategies

### How Providers Achieve Fast Allocation

**AWS Lambda:**
1. Pre-allocate Firecracker microVM pools
2. Assign from pool on demand
3. SnapStart: Pre-snapshot initialized runtimes
4. Provisioned Concurrency: User-paid warm pools

**E2B:**
1. Pool of pre-booted Firecracker VMs
2. Clone from pool on request
3. Same-region deployment for latency

### Memory vs Latency Tradeoffs

| Strategy | Memory Cost | Latency |
|----------|-------------|---------|
| No pool | 0 | 2-3s cold boot |
| Snapshot pool | Storage only | 4-30ms restore |
| Running VM pool | Full VM memory | ~instant |
| Clone-from-template | Base + CoW | ~100-200ms |

**Cost example:** 100 VMs × 512MB RAM = 50GB RAM ≈ $180/month idle

## Realistic Targets

| Target | Time | Difficulty | Method |
|--------|------|------------|--------|
| "Fast enough" | 1-2s | Trivial | Docker with caching |
| Competitive | 400-600ms | Easy | Firecracker fresh boot |
| **Impressive demo** | **200-300ms** | Medium | Warm pool + Firecracker |
| State-of-art | <50ms | Hard | Snapshot restore + pre-warm |

## Recommendations for hyperbox

### Achievable Targets
- **P50 cold start: 200-300ms** (warm pool + Firecracker)
- **P50 warm start: 10-30ms** (snapshot restore)
- **P95 cold start: <500ms** (consistent performance)

### Implementation Strategy
1. **Fresh boot path**: ~150ms with optimized kernel
2. **Snapshot restore path**: 10-30ms for "warm" VMs
3. **Warm pool**: Pre-boot 10-50 VMs based on demand
4. **Pre-initialize runtimes**: Python/Node ready before assignment

### What Would Be Impressive
| Achievement | Why Impressive |
|-------------|----------------|
| P50 < 200ms cold | Matches E2B, beats CodeSandbox |
| P95 < 500ms | Shows consistent performance |
| Snapshot restore < 20ms | Approaches AWS Lambda quality |
