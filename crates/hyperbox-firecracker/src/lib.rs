use std::{fs, path::Path, process::Command};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinuxCapabilities {
    pub os: String,
    pub has_kvm_device: bool,
    pub kvm_read_write: bool,
    pub kvm_modules_loaded: bool,
    pub has_nft: bool,
    pub has_ipset: bool,
}

impl LinuxCapabilities {
    pub fn supports_firecracker(&self) -> bool {
        self.has_kvm_device && self.kvm_read_write && self.kvm_modules_loaded
    }
}

pub fn detect_linux_capabilities() -> LinuxCapabilities {
    let has_kvm_device = Path::new("/dev/kvm").exists();
    let kvm_read_write = has_kvm_device
        && fs::metadata("/dev/kvm")
            .map(|meta| !meta.permissions().readonly())
            .unwrap_or(false);

    let kvm_modules_loaded = Command::new("sh")
        .arg("-lc")
        .arg("lsmod | grep -E '^kvm(_intel|_amd)?\\b' >/dev/null")
        .status()
        .is_ok_and(|status| status.success());

    let has_nft = Command::new("sh")
        .arg("-lc")
        .arg("command -v nft >/dev/null")
        .status()
        .is_ok_and(|status| status.success());

    let has_ipset = Command::new("sh")
        .arg("-lc")
        .arg("command -v ipset >/dev/null")
        .status()
        .is_ok_and(|status| status.success());

    LinuxCapabilities {
        os: std::env::consts::OS.to_string(),
        has_kvm_device,
        kvm_read_write,
        kvm_modules_loaded,
        has_nft,
        has_ipset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_probe_returns_os() {
        let caps = detect_linux_capabilities();
        assert!(!caps.os.is_empty());
    }
}
