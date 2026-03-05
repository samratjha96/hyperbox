# Firecracker Platform Requirements

## Summary

Firecracker requires KVM, which significantly limits where it can run. Most developers cannot run Firecracker locally without bare metal hardware or specific cloud configurations.

## Linux Kernel Requirements

### Host Kernel
| Page Size | Kernel Version | Min Firecracker | End of Support |
|-----------|----------------|-----------------|----------------|
| 4K        | v5.10          | v1.0.0          | 2024-01-31     |
| 4K        | v6.1           | v1.5.0          | 2025-10-12     |

### Guest Kernel
| Page Size | Kernel Version | Min Firecracker | End of Support |
|-----------|----------------|-----------------|----------------|
| 4K        | v5.10          | v1.0.0          | 2024-01-31     |
| 4K        | v6.1           | v1.9.0          | 2026-09-02     |

**Key facts:**
- Kernel versions supported for minimum 2 years after addition
- Only 4K page size supported (no huge pages for snapshots)

## KVM Requirements

**Absolute requirements:**
- `/dev/kvm` must exist and be accessible
- KVM kernel module loaded (`kvm_intel` or `kvm_amd`)
- Read/write access to `/dev/kvm`

**Check KVM availability:**
```bash
lsmod | grep kvm
# Expected output (Intel):
# kvm_intel  348160  0
# kvm        970752  1 kvm_intel

# Verify access
[ -r /dev/kvm ] && [ -w /dev/kvm ] && echo "OK" || echo "FAIL"
```

**Access methods:**
```bash
# Option 1: Add user to kvm group
sudo usermod -aG kvm ${USER}

# Option 2: Use ACLs
sudo setfacl -m u:${USER}:rw /dev/kvm

# Option 3: Run as root (not recommended)
sudo ./firecracker
```

## Platform Compatibility Matrix

### Where Firecracker WORKS

| Environment | Status | Notes |
|-------------|--------|-------|
| Bare metal Linux (x86_64) | ✅ Works | Best option |
| Bare metal Linux (aarch64) | ✅ Works | Supported |
| AWS EC2 `.metal` instances | ✅ Works | c5.metal, m5n.metal, m6i.metal recommended |
| GCP N1/N2 (nested virt) | ✅ Works | Requires `--enable-nested-virtualization` |
| VMware Workstation/Fusion | ✅ Works | For development only |
| Hetzner bare metal | ✅ Works | Should work on dedicated servers |

### Where Firecracker DOES NOT WORK

| Environment | Status | Reason |
|-------------|--------|--------|
| AWS EC2 regular instances | ❌ No | No KVM access on non-.metal |
| GCP N2D/E2 instances | ❌ No | No nested virtualization |
| DigitalOcean droplets | ❌ No | No KVM access |
| Hetzner Cloud VMs | ❌ No | No nested virtualization |
| WSL2 | ❌ No | No KVM in WSL2 kernel |
| macOS (any) | ❌ No | No KVM (uses Hypervisor.framework) |
| Docker container | ⚠️ Maybe | Requires `--privileged` + `/dev/kvm` |

### Cloud Provider Details

**AWS EC2:**
- Only `.metal` instance types supported
- Regular instances (c5.xlarge, m5.large, etc.) do NOT expose KVM
- Recommended: c5.metal, m5n.metal, m6i.metal, m6a.metal

**GCP Compute Engine:**
```bash
gcloud compute instances create ${VM} \
  --enable-nested-virtualization \
  --min-cpu-platform="Intel Haswell"
```
- N1 (Haswell+): Works with nested virt
- N2: Works with nested virt  
- N2D (AMD): No nested virtualization
- E2: No nested virtualization

**Azure:**
- Undocumented (official docs say `[TODO]`)
- Dv3/Ev3 series may support nested virt but untested

## Required Permissions

### Development (Firecracker directly)
- KVM read/write access via group membership, ACLs, or root

### Production (with Jailer)
- Root initially (for chroot, cgroups, namespaces setup)
- Drops to specified uid/gid after jail setup
- CAP_NET_ADMIN for TAP device creation

## Known Issues & Gotchas

### Performance
- Jailer creation scales poorly with mount points and parallel jails
- Use cgroups v2 for snapshots (v1 has high latency)

### Snapshots
- Network connectivity may break after restore
- CPU template required on x86 for MSR preservation
- No cross-GIC restore on ARM (GICv2 ↔ GICv3 incompatible)
- Take snapshots after kernel fully boots (early boot snapshots can crash)

### Device Conflicts
- Other hypervisors (VMware, VirtualBox) lock KVM with "Resource busy"
- Terminate other hypervisors before running Firecracker

### Memory
- OOM with exit code 12 requires ≥128 MB free
- Increase `vm.min_free_kbytes` for page allocation failures

## Implications for hyperbox

1. **Local development on Linux laptops**: Only works if developer has bare metal with KVM
2. **Local development on Mac**: Not possible with Firecracker
3. **Cloud deployment**: Requires `.metal` instances (AWS) or nested virt (GCP)
4. **Cost**: `.metal` instances are expensive ($1-4/hour)

**Recommendation:** For cross-platform support, need alternative isolation on macOS (Virtualization.framework) and potentially gVisor for Linux without KVM.
