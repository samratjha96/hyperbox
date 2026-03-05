# macOS Virtualization Options

## Summary

macOS offers Virtualization.framework for running Linux VMs with good performance on Apple Silicon. Apple Containerization Framework (macOS 26+) provides native container support via lightweight VMs.

## Virtualization.framework (macOS 11+)

### Capabilities
- **Linux guests**: ARM64 Linux runs natively on Apple Silicon with excellent performance
- **Boot time**: Sub-second with optimized kernel + minimal rootfs
- **Nested virtualization**: Supported on M3+ chips (macOS 15+)
- **x86 guests**: Only via emulation (slow) - no hardware x86 virtualization

### Key Features
| Feature | Status | Notes |
|---------|--------|-------|
| Linux VMs | ✅ Supported | ARM64 native, excellent performance |
| VirtioFS | ✅ Supported | File sharing between host and guest |
| vsock | ✅ Supported | Fast host↔guest communication |
| NAT Networking | ✅ Supported | `VZNATNetworkDeviceAttachment` |
| Bridged Networking | ⚠️ Restricted | Requires Apple-approved entitlement |
| Snapshots | ⚠️ Partial | No native API; Tart uses APFS cloning |
| GPU Passthrough | ❌ Not supported | No Metal/GPU in VMs |

### Entitlements Required
| Entitlement | Purpose | Restriction |
|-------------|---------|-------------|
| `com.apple.security.virtualization` | Basic VM creation | Required for all use |
| `com.apple.vm.networking` | Virtual network interfaces | **Restricted** (Apple approval) |
| `com.apple.private.virtualization` | Advanced device access | Requires disabling AMFI |

### Network Device Attachments
| Type | Use Case | Filtering Possible? |
|------|----------|---------------------|
| `VZNATNetworkDeviceAttachment` | Simple NAT networking | ❌ No |
| `VZBridgedNetworkDeviceAttachment` | Bridge to physical NIC | ❌ Requires restricted entitlement |
| `VZFileHandleNetworkDeviceAttachment` | Userspace network stack | ✅ Yes (full control) |

**Key insight:** `VZFileHandleNetworkDeviceAttachment` routes all traffic through a file descriptor, enabling userspace filtering. This is how Lima implements networking.

## Apple Containerization Framework (macOS 26+)

### Status
- **Shipped**: macOS 26 (Tahoe) released September 2025
- **Open source**: github.com/apple/containerization
- **Requirements**: Apple Silicon only, macOS 26+

### Architecture
```
┌─────────────────────────────────────────────────────────┐
│ macOS Host (Swift)                                      │
│  ContainerManager → VZVirtualMachineManager             │
│                          │ vsock                        │
└──────────────────────────┼──────────────────────────────┘
                           ▼
┌──────────────────────────────────────────────────────────┐
│ Linux VM (lightweight)                                   │
│  vminitd (PID 1) → gRPC API → runc → Container          │
└──────────────────────────────────────────────────────────┘
```

### Key Features
- **VM-per-container**: Each container runs in its own lightweight VM
- **OCI compatible**: Pulls from Docker Hub, ghcr.io, etc.
- **Sub-second boot**: Optimized Linux kernel + minimal rootfs
- **Communication**: gRPC over vsock (same pattern as Firecracker)

### Swift APIs
```swift
// Core types
LinuxContainer           // Main container abstraction
ContainerManager         // High-level orchestration
LinuxProcess             // Process within container

// Key operations
container.create()
container.start()
container.exec(...)
container.copyIn(from:to:)
container.copyOut(from:to:)
container.stop()
```

### CLI Tool
```bash
container image pull alpine:latest
container run -t -i alpine:latest sh
container build -t myapp .
container images ls
```

## Hypervisor.framework (Lower Level)

### When to Use
- Building custom hypervisor/emulator (like QEMU)
- Need direct CPU/MMU control
- Implementing non-Virtio device emulation

### Comparison to Virtualization.framework
| Aspect | Hypervisor.framework | Virtualization.framework |
|--------|---------------------|-------------------------|
| Level | Low (CPU/MMU) | High (Virtio devices) |
| Effort | Very high | Medium |
| Use case | Custom VMM | Standard VMs |

## Existing Tools Comparison

| Tool | Framework | Boot Time | Memory | Best For |
|------|-----------|-----------|--------|----------|
| OrbStack | Virtualization.framework | Milliseconds | <1GB | Docker replacement (fastest) |
| Lima/Colima | Virtualization.framework or QEMU | 15-20s | Configurable | Free/OSS Docker replacement |
| UTM | QEMU + Hypervisor.framework | Varies | Varies | General VMs, legacy OS |
| Tart | Virtualization.framework | Fast | Configurable | macOS VM automation |

### Why OrbStack is Fast
- Rust-based native macOS app (not Electron)
- Custom VirtioFS implementation (2-10x faster file I/O)
- Dynamic memory allocation (releases unused RAM)
- Single optimized Linux VM (not micro-VMs)
- Idles at 0.1% CPU

## Network Isolation on macOS

### The Problem
macOS has no kernel-level equivalent to Linux's nftables+ipset. PF (packet filter) exists but:
- VZ NAT traffic bypasses PF
- No dynamic IP sets like ipset
- No per-process/VM filtering

### Solution: Userspace Filtering
Use `VZFileHandleNetworkDeviceAttachment` + userspace network stack (gvisor-tap-vsock):

```
VM Traffic → File Handle → Userspace Stack → Filter → Real Network
```

This allows:
- DNS filtering (allowlist domains)
- IP filtering (only allow resolved IPs)
- Full packet inspection

### Implementation Architecture
```
┌─────────────────────────────────────────────────────────────────────┐
│ macOS Host                                                          │
│  ┌────────────────┐      ┌──────────────────────────────────────┐  │
│  │ VM (VZ.framework)│◄────│ Network Gateway (gvisor-tap-vsock)   │  │
│  │                │  FH  │  ┌────────────┐  ┌────────────────┐  │  │
│  │                │      │  │ DNS Filter │  │ Connection     │  │  │
│  │                │      │  │ (allowlist)│  │ Filter         │  │  │
│  └────────────────┘      │  └────────────┘  └────────────────┘  │  │
│                          └──────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

## Implications for hyperbox

1. **macOS 26+**: Use Apple Containerization Framework (ideal fit)
2. **macOS 13-15**: Use Virtualization.framework directly
3. **Network isolation**: Requires forking gvisor-tap-vsock for filtering
4. **vsock**: Available on both frameworks (same IPC pattern as Firecracker)
5. **No x86 support**: Apple Silicon only for good performance
