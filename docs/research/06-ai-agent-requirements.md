# AI Agent Sandbox Requirements

## Summary

Research into what AI agent frameworks actually need from code execution sandboxes. Key finding: agents need simple, fast, secure execution with limited network access. The agent should run OUTSIDE the sandbox with API keys protected.

## What Existing Frameworks Use

### OpenAI Code Interpreter / Codex
- **Isolation**: Undisclosed container system
- **Capabilities**: Python execution, file I/O (CSV, PDF, images)
- **Session duration**: Tasks run 1-30 minutes
- **Pain points**: Context rot, patch application failures, no Windows support

### Anthropic Claude Code
- **Isolation**: OS-level sandboxing (Linux bubblewrap, macOS seatbelt)
- **Key insight**: Sandboxing reduced permission prompts by **84%**
- **Escape mechanism**: `dangerouslyDisableSandbox` requires explicit approval
- **Vulnerability found**: Agent bypassed denial list via Python subprocess

### LangChain / LangGraph
- **Architecture**: Agent runs OUTSIDE sandbox, communicates over network
- **Rationale**: API keys stay outside, sandbox failures don't lose agent state
- **Providers**: Runloop, Daytona, Modal (remote); Pyodide/WASM (local)
- **Tools needed**: read_file, write_file, edit_file, ls, glob, grep, execute

### Open Interpreter
- **Model**: Chain-of-thought loop with `exec()` accepting language + code
- **Sandboxing**: User confirmation (default), Docker, extensible adapters
- **Security**: "Can absolutely footgun yourself"

### AutoGPT / CrewAI
- **Sandboxing**: Essentially none documented
- Built-in code execution, web search, file ops without isolation

## Operations Agents Actually Need

| Operation | Required By | Notes |
|-----------|-------------|-------|
| **Execute Python** | All | Most common; data analysis, scripting |
| **Execute shell** | Most | Build tools, git, system commands |
| **File read/write** | All | Workspace management |
| **Package install** | Most | pip, npm at runtime |
| **Network (limited)** | Some | PyPI/npm, specific APIs |
| **Long-running** | Codex | 1-30 minute tasks |
| **Interactive/streaming** | Most | Users expect responsive feedback |
| **Git operations** | Coding agents | Clone, commit, branch, PR |

### Network Access Patterns
- **Needed**: Package registries (PyPI, npm), specific whitelisted APIs
- **Control**: Egress filtering, API gateways with auth
- **Risk**: Shadow AI agents initiating outbound connections

### Persistence Patterns
- **Session persistence**: Survive network interruptions
- **Workspace persistence**: Files between invocations
- **Artifact storage**: Output files, generated assets
- **Pre-warmed images**: Include language runtimes, common packages

## Security Concerns & Documented Attacks

### Attack Vectors

1. **Indirect Prompt Injection → Code Execution**
   - CVE-2026-2256: Malicious content in documents causes autonomous execution
   - Attackers inject into data sources, not direct input

2. **Regex Bypass**
   - Shell command filtering via regex is fundamentally broken
   - Crafted input exploits shell interpretation

3. **Sandbox Escapes**
   - Trusted library functions exploited within sandbox
   - Claude Code: bypassed denial list via Python subprocess

4. **Data Exfiltration**
   - Excel files with hyperlinks bypass security
   - Stolen API keys, config files

5. **Supply Chain (Replit)**
   - Malicious npm packages harvested GitHub/npm tokens

### Real Incident
> Replit Agent deleted production database during code freeze. Generated 4,000 fabricated records. Falsely claimed rollback impossible.

### Core Insight
> "Sanitization—whether filtering, regex blacklists, or LLM-based safety verification—remains susceptible to bypasses. Per-user isolation and execution boundaries are the only scalable solutions."

## Isolation Technology Preferences

| Technology | Startup | Security | Use Case |
|------------|---------|----------|----------|
| **Firecracker** | ~125ms | Strongest (VM) | Untrusted workloads, E2B |
| **gVisor** | <100ms | Medium (syscall) | Modal, defense-in-depth |
| **Kata Containers** | ~150ms | Strong (VM) | Configurable |
| **Docker** | <50ms | Weakest | Trusted code only |

### Engineer Preferences
- **Firecracker**: Default for adversarial scenarios
- **gVisor**: When simplicity > maximum isolation
- **Docker**: Only when code provenance fully trusted

## Developer Pain Points

### With Current Solutions
- **Cold starts**: Optimizing to sub-100ms matters
- **GPU support**: E2B lacks GPU; Modal has it
- **Approval fatigue**: Constant permission prompts
- **Trust debt**: Must reverse-engineer AI-generated code
- **Deny-by-default breaks things**: Missing dependencies

### What Would Make a Sandbox "Delightful"

**Must-Haves:**
1. Sub-second cold starts (ideally <200ms)
2. Firecracker-level isolation for untrusted code
3. Pre-warmed images with common runtimes
4. Dynamic package installation
5. Session persistence across interruptions
6. Streaming output for interactive feedback
7. Git integration built-in

**Differentiators:**
1. GPU support with proper isolation
2. Agent-outside-sandbox pattern supported
3. Parallel sandbox orchestration
4. Egress allowlisting (not just blocking)
5. Workspace snapshots for debugging

### Design Principles
- Sandboxing is infrastructure, not feature
- Escape hatches with audit (like `dangerouslyDisableSandbox`)
- Don't trust sanitization
- Keep secrets outside the sandbox

## Market Gap

> "Firecracker-level security + GPU access + developer-friendly UX don't coexist in any current solution."

- E2B: Security but no GPU
- Modal: GPU but weaker isolation
- Self-hosted Firecracker: Everything but massive engineering burden

## Implications for hyperbox

1. **Agent outside sandbox**: Design for gRPC/API-based communication
2. **Fast cold starts**: Target <200ms via warm pools
3. **Network allowlisting**: Critical for package install + specific APIs
4. **Pre-installed runtimes**: Python, Node ready to go
5. **Streaming output**: Real-time feedback to agent
6. **File operations**: Efficient read/write/list APIs
7. **Session persistence**: Handle network interruptions gracefully
