# Project Ideas: Rust + Low-Level + Distributed Systems

Ideas for impressive portfolio projects targeting roles like OpenAI Codex.
Each demonstrates: Rust, systems programming, distributed systems, and relevance to AI infrastructure.

---

## Evaluation Criteria

| Criterion | Weight | What it means |
|-----------|--------|---------------|
| **Relevance to AI infra** | High | Directly useful for LLM serving, training, agents |
| **Low-level depth** | High | Kernel, memory, networking, not just CRUD |
| **Distributed systems** | High | Consensus, scheduling, coordination |
| **Feasibility** | Medium | Can ship MVP in 2-4 months |
| **Differentiation** | Medium | Not already well-solved |

---

## Tier 1: High Impact, Directly Relevant

### 1. Inference Request Batcher
**The problem:** vLLM does continuous batching within a process, but no good solution for batching across processes/nodes. Small orgs waste GPU memory with tiny batches.

**What to build:**
```
Requests → Batcher → Batch Queue → vLLM/TensorRT
              ↓
        Fair scheduler (latency SLOs)
              ↓
        Dynamic batch sizing per model/GPU
```

**Technical depth:**
- Lock-free queue for request aggregation
- Deadline-based scheduling (urgent vs background)
- Backpressure handling when GPUs saturate
- Integration with vLLM's continuous batching

**Why impressive:**
- Clear metrics: GPU utilization, p99 latency
- Production-relevant (this is a real pain point)
- Shows understanding of inference systems

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐ Medium | 2-3 months | High (no good OSS) |

---

### 2. Model Artifact CDN
**The problem:** Distributing 10-100GB model weights across regions is slow. No OSS solution handles deduplication, delta sync, or GPU-memory prefetching.

**What to build:**
```
Model Registry (content-addressed)
        ↓
Delta Sync (fine-tune shares 95% with base)
        ↓
Regional Edge Cache
        ↓
GPU-Memory Prefetch (load while previous request finishes)
```

**Technical depth:**
- Content-addressed storage (like git but for tensors)
- Delta compression between model versions
- P2P distribution within clusters (BitTorrent-style)
- CUDA memory mapping for zero-copy load

**Why impressive:**
- Directly relevant to OpenAI's scale
- Shows storage + networking + GPU knowledge
- Novel: nobody has done this well

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐⭐ High | 3-4 months | Very High |

---

### 3. LLM Observability Collector
**The problem:** 26% of inference incidents go undetected until customer reports. Existing APM doesn't understand tokens, KV cache, or batch efficiency.

**What to build:**
```
vLLM/TensorRT ─→ OTel Collector ─→ Metrics Store
                      ↓
              Inference-specific metrics:
              - Time-to-first-token
              - Tokens/second
              - KV cache hit rate
              - Batch utilization
              - Cost per request
```

**Technical depth:**
- Zero-overhead instrumentation (eBPF or compile-time)
- Token-level tracing (input → attention → output)
- Anomaly detection per-model (not just per-endpoint)
- Cost attribution (GPU-seconds per customer)

**Why impressive:**
- Underserved market (observability vendors don't understand LLMs)
- Can build on OpenTelemetry (credibility)
- Clear demo: show cost savings from optimization

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐ Medium | 6-8 weeks | High |

---

### 4. GPU-Aware Request Router
**The problem:** Multi-region inference needs real-time GPU capacity awareness. Current solutions use static routing.

**What to build:**
```
┌─────────────────────────────────────────────────┐
│              GPU-Aware Router                   │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐        │
│  │ Agent A │  │ Agent B │  │ Agent C │        │
│  │ GPU: 80%│  │ GPU: 20%│  │ GPU: 95%│        │
│  └────┬────┘  └────┬────┘  └────┬────┘        │
│       └───────────┬┴───────────┘              │
│                   ▼                            │
│            Routing Decision                    │
│     (capacity + latency + cost)               │
└─────────────────────────────────────────────────┘
```

**Technical depth:**
- Gossip protocol for capacity propagation (SWIM)
- Connection draining for long-running requests
- Cost-aware routing (spot vs on-demand)
- Automatic failover with health checks

**Why impressive:**
- Shows distributed systems knowledge
- Production-critical at scale
- Can demo with real metrics

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐ Medium-High | 2-3 months | High |

---

## Tier 2: Deep Technical Expertise

### 5. sched_ext BPF Scheduler for AI Workloads
**The problem:** Default Linux schedulers don't understand AI workload patterns (bursty inference, long training jobs, interactive compilation).

**What to build:**
- Custom BPF scheduler using sched_ext (Linux 6.12+)
- Optimize for: inference latency, GPU-adjacent placement, deadline enforcement
- Export scheduling decisions as metrics

**Why impressive:**
- Shows deep kernel knowledge
- sched_ext is cutting-edge (mainlined late 2024)
- Directly applicable to Codex's scale

**Reference:** scx_rusty runs at Meta scale

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐⭐ High | 2-3 months | Very High |

---

### 6. io_uring Async Runtime for Sandboxes
**The problem:** Sandbox I/O (file watches, network isolation, process communication) is syscall-heavy. Traditional async runtimes use epoll.

**What to build:**
- Rust async runtime built on io_uring
- Optimized for sandbox use cases: file watching, subprocess I/O, vsock
- Batched syscall submission (reduced context switches)
- Zero-copy where possible

**Reference:** Glommio, Monoio, tokio-uring

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐ Medium | 2 months | Medium |

---

### 7. eBPF Security Monitor for Code Execution
**The problem:** Detecting malicious behavior in sandboxed code without overhead.

**What to build:**
- eBPF programs tracking syscall patterns
- Anomaly detection (cryptomining, data exfil attempts)
- Low-overhead profiling without instrumentation
- Real-time alerts + Prometheus metrics

**Why impressive:**
- Directly relevant to Codex's security needs
- Shows eBPF expertise
- Can demo with attack simulations

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐ Medium | 2 months | High |

---

### 8. Fast Checkpoint I/O for Training
**The problem:** 18TB checkpoints take 3+ minutes to write. GPUs idle. $100k/hr clusters waste money.

**What to build:**
- Async checkpoint writes (don't block training)
- Incremental checkpoints (only changed tensors)
- Distributed coordination (which node writes what)
- Integration with PyTorch/JAX

**Why impressive:**
- Real pain point at scale ($$ impact)
- Shows understanding of training infrastructure
- Novel: most teams DIY this poorly

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐⭐ High | 3-4 months | Very High |

---

## Tier 3: Ambitious / Long-term

### 9. GPU Memory Multiplexer
**The problem:** Multiple small models on one GPU waste memory. MIG is inflexible.

**What to build:**
- Unified memory pool across models
- Dynamic allocation based on request patterns
- Preemption (pause low-priority model for high-priority)
- CUDA abstraction layer

**Why impressive:** Shows deep GPU systems knowledge

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐⭐⭐ Very High | 6+ months | Very High |

---

### 10. Self-Hosted Modal Clone
**The problem:** Modal is great but proprietary. Ray is powerful but complex.

**What to build:**
- Python decorator → container (like `@app.function(gpu="A100")`)
- Kubernetes operator for GPU scheduling
- Built-in model caching + warm pools
- Local dev mode with GPU simulation

| Complexity | Timeline | Differentiation |
|------------|----------|-----------------|
| ⭐⭐⭐⭐ High | 4-5 months | High |

---

## Comparison Matrix

| Project | Rust | Low-Level | Distributed | AI-Relevant | Timeline |
|---------|------|-----------|-------------|-------------|----------|
| hyperbox (sandbox) | ✅ | ✅ | ⚠️ | ✅ | 3 months |
| Inference Batcher | ✅ | ⚠️ | ✅ | ✅ | 2-3 months |
| Model CDN | ✅ | ✅ | ✅ | ✅ | 3-4 months |
| LLM Observability | ✅ | ✅ | ⚠️ | ✅ | 6-8 weeks |
| GPU Router | ✅ | ⚠️ | ✅ | ✅ | 2-3 months |
| sched_ext Scheduler | ✅ | ✅✅ | ⚠️ | ✅ | 2-3 months |
| io_uring Runtime | ✅ | ✅✅ | ❌ | ⚠️ | 2 months |
| eBPF Monitor | ✅ | ✅✅ | ❌ | ✅ | 2 months |
| Checkpoint I/O | ✅ | ✅ | ✅ | ✅ | 3-4 months |
| GPU Multiplexer | ✅ | ✅✅ | ❌ | ✅ | 6+ months |

---

## Recommended Portfolio (2-3 projects)

**Option A: Breadth**
1. hyperbox (sandbox) - VM isolation, security
2. LLM Observability - monitoring, metrics
3. GPU Router - distributed coordination

**Option B: Depth (low-level focus)**
1. hyperbox (sandbox) - VM isolation
2. sched_ext Scheduler - kernel expertise
3. eBPF Monitor - security monitoring

**Option C: AI Infrastructure Focus**
1. hyperbox (sandbox) - code execution
2. Model CDN - artifact distribution
3. Inference Batcher - serving optimization

---

## Next Steps

Pick 1-2 additional projects to flesh out at the same level as hyperbox:
- Research validation
- Competitive analysis
- Architecture design
- Implementation plan
