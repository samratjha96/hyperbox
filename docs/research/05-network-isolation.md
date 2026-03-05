# Network Isolation Implementation

## Summary

Network isolation requires different approaches on Linux vs macOS. Linux can use kernel-level filtering (nftables + ipset). macOS requires userspace filtering via `VZFileHandleNetworkDeviceAttachment`.

## Network Policy Modes

### Mode 1: None (Default)
Complete air-gap. No network interface attached to VM.

```rust
NetworkPolicy::None
// Don't attach any network device
```

### Mode 2: Full
Unrestricted internet access. Use only for trusted code.

```rust
NetworkPolicy::Full
// Standard NAT networking
```

### Mode 3: Allowlist (Key Feature)
Only specified domains reachable. DNS-based enforcement with IP tracking.

```rust
NetworkPolicy::Allowlist(vec![
    "api.openai.com",
    "*.github.com",
    "pypi.org",
])
```

## Linux Implementation

### Architecture
```
┌─────────────────────────────────────────────────────────────────────┐
│ HOST                                                                 │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐ │
│  │   DNS Proxy     │───>│   IP Tracker    │───>│    nftables     │ │
│  │ (per-VM, Rust)  │    │   (ipset)       │    │   (per-VM set)  │ │
│  └────────┬────────┘    └─────────────────┘    └─────────────────┘ │
│           │                                                         │
│  ┌────────▼────────┐                                                │
│  │     tap0        │  172.16.0.1/30                                │
│  └────────┬────────┘                                                │
└───────────┼─────────────────────────────────────────────────────────┘
            │
┌───────────▼─────────────────────────────────────────────────────────┐
│ GUEST VM                                                             │
│  eth0: 172.16.0.2/30, DNS: 172.16.0.1                               │
└──────────────────────────────────────────────────────────────────────┘
```

### Components

#### 1. TAP Device Setup
```bash
# Create TAP device for VM
ip tuntap add tap0 mode tap
ip addr add 172.16.0.1/30 dev tap0
ip link set tap0 up

# Enable IP forwarding
echo 1 > /proc/sys/net/ipv4/ip_forward
```

#### 2. DNS Proxy
Rust implementation using `hickory-dns`:

```rust
pub struct AllowlistDnsProxy {
    allowlist: Vec<DomainPattern>,
    upstream: SocketAddr,
    on_resolve: Box<dyn Fn(IpAddr, Duration) + Send + Sync>,
}

impl AllowlistDnsProxy {
    pub async fn handle_query(&self, query: &Message) -> Message {
        let domain = query.queries()[0].name().to_string();
        
        if self.matches_allowlist(&domain) {
            let response = self.forward_upstream(query).await?;
            for answer in response.answers() {
                if let Some(ip) = answer.ip_addr() {
                    let ttl = Duration::from_secs(answer.ttl() as u64);
                    (self.on_resolve)(ip, ttl);
                }
            }
            response
        } else {
            // Return NXDOMAIN
            Message::error_msg(query.id(), ResponseCode::NXDomain)
        }
    }
}

pub enum DomainPattern {
    Exact(String),      // "api.openai.com"
    Wildcard(String),   // "*.github.com"
    Suffix(String),     // "github.com" (matches subdomains)
}
```

#### 3. nftables + ipset Rules
```bash
# Create per-VM ipset with TTL support
ipset create vm_abc_allowed hash:ip timeout 300

# nftables rules
nft add table inet vm_abc
nft add chain inet vm_abc forward { type filter hook forward priority 0 \; }
nft add rule inet vm_abc forward iifname "tap0" ip daddr @vm_abc_allowed accept
nft add rule inet vm_abc forward iifname "tap0" log prefix "blocked: " drop

# Allow established connections back
nft add rule inet vm_abc forward iifname "eth0" oifname "tap0" \
    ct state established,related accept

# NAT for outbound
nft add table nat
nft add chain nat postrouting { type nat hook postrouting priority 100 \; }
nft add rule nat postrouting ip saddr 172.16.0.2 oifname "eth0" masquerade
```

#### 4. Dynamic IP Allowlist
```rust
pub struct VmFirewall {
    vm_id: String,
    ipset_name: String,
}

impl VmFirewall {
    pub async fn allow_ip(&self, ip: IpAddr, ttl: Duration) -> Result<()> {
        Command::new("ipset")
            .args(["add", &self.ipset_name, &ip.to_string()])
            .args(["timeout", &ttl.as_secs().to_string()])
            .status().await?;
        Ok(())
    }
    
    pub async fn teardown(&self) -> Result<()> {
        Command::new("nft")
            .args(["delete", "table", "inet", &format!("vm_{}", self.vm_id)])
            .status().await?;
        Command::new("ipset")
            .args(["destroy", &self.ipset_name])
            .status().await?;
        Ok(())
    }
}
```

## macOS Implementation

### The Challenge
macOS has no kernel-level equivalent to nftables+ipset:
- PF (packet filter) exists but VM NAT traffic bypasses it
- No dynamic IP sets
- No per-process/VM filtering

### Solution: Userspace Filtering
Use `VZFileHandleNetworkDeviceAttachment` to route all traffic through host process.

### Architecture
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

### VZFileHandleNetworkDeviceAttachment
```swift
// Routes all VM traffic through a file descriptor
let socketPair = createSocketPair()
let attachment = try VZFileHandleNetworkDeviceAttachment(
    fileHandle: FileHandle(fileDescriptor: socketPair.vmSide)
)

// Host side receives raw Ethernet frames
let hostSocket = socketPair.hostSide
// → Connect to gvisor-tap-vsock for TCP/IP handling
```

### gvisor-tap-vsock Integration
Lima uses gvisor-tap-vsock for userspace networking. For filtering:

```go
// Fork gvisor-tap-vsock with filtering
type NetworkPolicy struct {
    Mode           string   // "none", "allowlist", "full"
    AllowedDomains []string
    ResolvedIPs    map[string][]net.IP
}

func (p *NetworkPolicy) AllowConnection(destIP net.IP) bool {
    switch p.Mode {
    case "none":
        return false
    case "full":
        return true
    case "allowlist":
        return p.isIPAllowed(destIP)
    }
    return false
}
```

### DNS Filtering
```go
func (h *Handler) ServeDNS(w dns.ResponseWriter, r *dns.Msg) {
    domain := r.Question[0].Name
    
    if !h.policy.IsDomainAllowed(domain) {
        // Return NXDOMAIN
        m := new(dns.Msg)
        m.SetRcode(r, dns.RcodeNameError)
        w.WriteMsg(m)
        return
    }
    
    // Forward to upstream, cache resolved IPs
    resp := h.forwardUpstream(r)
    for _, ans := range resp.Answer {
        if a, ok := ans.(*dns.A); ok {
            h.policy.TrackResolvedIP(domain, a.A)
        }
    }
    w.WriteMsg(resp)
}
```

## Bypass Prevention

| Attack Vector | Mitigation |
|---------------|------------|
| Direct IP access | Only IPs from DNS resolution allowed |
| DNS over HTTPS (DoH) | Block known DoH IPs (8.8.8.8:443) |
| DNS over TLS (DoT) | Block port 853 |
| Hardcoded /etc/hosts | We control guest rootfs |
| Tunnel over allowed domain | Out of scope (requires L7 inspection) |

## Platform Comparison

| Feature | Linux | macOS |
|---------|-------|-------|
| Filtering location | Kernel (nftables) | Userspace |
| Dynamic IP sets | ipset (kernel) | In-memory (Go/Rust) |
| DNS interception | iptables redirect | Userspace proxy |
| Performance | Kernel-speed | Slight overhead |
| Implementation | nftables + ipset | Fork gvisor-tap-vsock |

## Rust Dependencies

```toml
# DNS proxy
hickory-dns = "0.24"      # DNS server/client
hickory-resolver = "0.24" # DNS resolution

# Linux firewall
nix = "0.28"              # System calls
# Shell out to ipset/nft for simplicity

# macOS
# Use Swift for VZ.framework, or Go for gvisor-tap-vsock
```

## Recommendations

1. **Default to `NetworkPolicy::None`** - secure by default
2. **Linux: nftables + ipset** - kernel-speed filtering
3. **macOS: userspace gateway** - fork gvisor-tap-vsock
4. **DNS proxy on both platforms** - domain allowlisting
5. **Track resolved IPs** - prevent direct IP bypass
6. **TTL-based expiry** - respect DNS TTLs for IP allowlist
