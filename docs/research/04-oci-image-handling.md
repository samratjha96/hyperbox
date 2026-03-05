# OCI Image Handling Strategies

## Summary

Full OCI/Docker image support is complex. The simplest path for a code sandbox is **pre-baked templates** (what E2B and CodeSandbox actually do). This covers 90% of use cases with minimal complexity.

## Options Analysis

### Option A: firecracker-containerd
**Complexity: HIGH | Maturity: PRODUCTION**

- Official AWS project, actively maintained
- Full containerd integration with custom shim runtime
- Runs containerd inside the microVM with an agent
- Requires custom containerd binary (compiled as plugin)

**Verdict:** Overkill for simple code execution. Designed for multi-tenant Fargate-style workloads.

### Option B: Ignite Approach (DEPRECATED)
**Complexity: MEDIUM | Status: ARCHIVED (Dec 2023)**

Key concept:
```
OCI Image → containerd pulls → Extract layers → Create ext4 rootfs → Attach to Firecracker
```

Used containerd as image store only (not runtime), then extracted layers to device-mapper volumes.

**Verdict:** Best conceptual model. Archived but instructive.

### Option C: Flintlock
**Complexity: MEDIUM-HIGH | Maturity: ACTIVE**

- Uses containerd for image pulls
- Supports Firecracker + Cloud Hypervisor
- OCI images for volumes, kernel, initrd
- gRPC/HTTP API

**Requires containerd running as daemon.**

### Option D: Manual Layer Extraction
**Complexity: MEDIUM | Control: HIGH**

Build custom OCI handling in Rust:

```rust
// Core dependencies
oci-client = "0.16"    // Registry pull, auth, manifest handling
oci-spec = "0.9"       // Parse OCI manifests
tar = "0.4"            // Extract layer tarballs
flate2 = "1.0"         // Decompress gzip layers
```

**Process:**
1. Pull manifest from registry
2. Download layers (tar.gz blobs)
3. Create ext4 filesystem
4. Extract layers in order
5. Attach to Firecracker

## What E2B and CodeSandbox Actually Do

### E2B
- **Pre-built templates only** (NOT arbitrary Docker images)
- Users define templates via SDK/CLI
- Templates built ahead of time, snapshotted
- `Template.build()` creates reusable snapshot

From E2B docs:
> "E2B templates allow you to define custom sandboxes... you can have fully configured sandboxes with running processes ready to use with zero wait time"

### CodeSandbox
- Uses Firecracker
- Pre-defined environment templates
- NOT arbitrary Docker image support
- Focus on dev environment templates (node, python, etc.)

**Pattern:** Both use **pre-baked templates/snapshots**, not runtime OCI pulls.

## Recommended Approach: Pre-baked Templates

### Phase 1: Pre-baked Snapshots (Simplest)
**Implementation time: 3-5 days**

```
Build time:
  python:3.12 → ext4 snapshot → compress → store
  node:20     → ext4 snapshot → compress → store
  golang:1.22 → ext4 snapshot → compress → store

Runtime:
  Request python → decompress cached ext4 → boot Firecracker
```

**Pros:**
- Sub-100ms boot (no layer extraction)
- No registry interaction at runtime
- Simple caching
- Deterministic, tested images

**Cons:**
- Limited to pre-built images
- Need rebuild pipeline for updates

### Supported Templates (Initial)
| Template | Base | Included |
|----------|------|----------|
| `python:3.11` | Alpine | Python 3.11, pip, common packages |
| `python:3.12` | Alpine | Python 3.12, pip, common packages |
| `node:18` | Alpine | Node.js 18, npm |
| `node:20` | Alpine | Node.js 20, npm |
| `golang:1.22` | Alpine | Go 1.22 |
| `rust:1.75` | Alpine | Rust toolchain |
| `ubuntu:22.04` | Ubuntu | Basic Ubuntu environment |

### Phase 2 (Optional): On-demand Layer Extraction
**Implementation time: 2-3 weeks**

Add support for arbitrary images if users need them:

```rust
// Lazy pull and cache
async fn get_image(reference: &str) -> Result<PathBuf> {
    if let Some(cached) = cache.get(reference) {
        return Ok(cached);
    }
    
    // Pull from registry
    let layers = pull_image(reference).await?;
    
    // Create ext4, extract layers
    let rootfs = create_rootfs(layers).await?;
    
    cache.insert(reference, rootfs.clone());
    Ok(rootfs)
}
```

**Trade-offs:**
- First pull is slow (network + extraction)
- Requires root for mounting (or complex FUSE setup)
- Layer caching helps subsequent requests

## Rust Implementation Sketch

### Registry Client
```rust
use oci_client::{Client, Reference};
use oci_spec::image::ImageManifest;

async fn pull_manifest(image: &str) -> Result<ImageManifest> {
    let client = Client::new(Default::default());
    let reference: Reference = image.parse()?;
    
    let (manifest, _) = client.pull_manifest(&reference).await?;
    Ok(manifest)
}

async fn pull_layer(
    client: &Client,
    reference: &Reference,
    digest: &str,
    output: &Path,
) -> Result<()> {
    let mut stream = client.pull_blob_stream(reference, digest).await?;
    let mut file = File::create(output)?;
    
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?)?;
    }
    Ok(())
}
```

### Layer Extraction
```rust
use tar::Archive;
use flate2::read::GzDecoder;

fn extract_layer(layer_path: &Path, rootfs_path: &Path) -> Result<()> {
    let file = File::open(layer_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    
    archive.unpack(rootfs_path)?;
    Ok(())
}
```

### ext4 Creation
```bash
# Shell out to mkfs.ext4 (simplest)
truncate -s 2G rootfs.ext4
mkfs.ext4 -F rootfs.ext4
mount -o loop rootfs.ext4 /mnt/rootfs
# ... extract layers ...
umount /mnt/rootfs
```

## Complexity Comparison

| Approach | Time | Dependencies | Arbitrary Images |
|----------|------|--------------|------------------|
| Pre-baked snapshots only | 3-5 days | None | No |
| Pre-baked + lazy pull | 2-3 weeks | oci-client, root | Yes |
| Use containerd library | 3-4 weeks | containerd | Yes |
| firecracker-containerd | 4-6 weeks | Full containerd | Yes |

## Recommendations for hyperbox

1. **Start with pre-baked templates** - covers 90% of use cases
2. **Build 5-10 common templates** - python, node, go, rust, ubuntu
3. **Add lazy pull later** - only if users demand arbitrary images
4. **Cache aggressively** - layer-level and image-level caching
5. **Version templates** - `python:3.12-v1`, `python:3.12-v2` for updates
