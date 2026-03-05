use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MacOsCapabilities {
    pub os: String,
    pub version: String,
    pub major_version: u32,
    pub is_apple_silicon: bool,
    pub supports_virtualization_framework: bool,
    pub supports_containerization_framework: bool,
    pub has_container_cli: bool,
    pub has_containerization_framework: bool,
}

impl MacOsCapabilities {
    pub fn preferred_backend(&self) -> &'static str {
        if self.supports_containerization_framework {
            "apple_containerization"
        } else if self.supports_virtualization_framework {
            "virtualization_framework"
        } else {
            "unsupported"
        }
    }
}

pub fn detect_macos_capabilities() -> MacOsCapabilities {
    let version = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "0.0.0".to_string());

    let major_version = version
        .split('.')
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or_default();

    let is_apple_silicon = std::env::consts::ARCH == "aarch64";
    let supports_virtualization_framework = major_version >= 11 && is_apple_silicon;
    let has_container_cli = Command::new("sh")
        .arg("-lc")
        .arg("command -v container >/dev/null 2>&1")
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let has_containerization_framework =
        Path::new("/System/Library/Frameworks/Containerization.framework").exists();
    let supports_containerization_framework = major_version >= 26
        && is_apple_silicon
        && (has_container_cli || has_containerization_framework);

    MacOsCapabilities {
        os: std::env::consts::OS.to_string(),
        version,
        major_version,
        is_apple_silicon,
        supports_virtualization_framework,
        supports_containerization_framework,
        has_container_cli,
        has_containerization_framework,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_produces_consistent_backend_choice() {
        let caps = detect_macos_capabilities();
        if caps.supports_containerization_framework {
            assert_eq!(caps.preferred_backend(), "apple_containerization");
        }
    }
}
