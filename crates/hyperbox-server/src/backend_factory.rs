use std::{collections::HashMap, path::PathBuf, sync::Arc};

use hyperbox_apple::{
    AppleBackendConfig, AppleRuntimeKind, AppleVzBackend, detect_macos_capabilities,
};
use hyperbox_core::SandboxBackend;
use hyperbox_firecracker::{
    FirecrackerBackend, FirecrackerBackendConfig, detect_linux_capabilities,
};

use crate::LocalBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Auto,
    Local,
    Firecracker,
    Apple,
}

impl BackendKind {
    pub fn from_env() -> Self {
        match std::env::var("HYPERBOX_BACKEND")
            .unwrap_or_else(|_| "auto".to_string())
            .as_str()
        {
            "local" => Self::Local,
            "firecracker" => Self::Firecracker,
            "apple" => Self::Apple,
            _ => Self::Auto,
        }
    }
}

pub fn select_backend(kind: BackendKind) -> Arc<dyn SandboxBackend> {
    match kind {
        BackendKind::Local => Arc::new(LocalBackend::new(None)),
        BackendKind::Firecracker => Arc::new(build_firecracker_backend()),
        BackendKind::Apple => Arc::new(build_apple_backend()),
        BackendKind::Auto => auto_backend(),
    }
}

fn auto_backend() -> Arc<dyn SandboxBackend> {
    let os = std::env::consts::OS;
    match os {
        "linux" => {
            let caps = detect_linux_capabilities();
            if caps.supports_firecracker() {
                Arc::new(build_firecracker_backend())
            } else {
                Arc::new(LocalBackend::new(None))
            }
        }
        "macos" => {
            let caps = detect_macos_capabilities();
            if caps.supports_virtualization_framework {
                Arc::new(build_apple_backend())
            } else {
                Arc::new(LocalBackend::new(None))
            }
        }
        _ => Arc::new(LocalBackend::new(None)),
    }
}

fn build_firecracker_backend() -> FirecrackerBackend {
    let work_dir = std::env::var("HYPERBOX_FIRECRACKER_WORKDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("hyperbox-firecracker"));

    let kernel_image = std::env::var("HYPERBOX_FIRECRACKER_KERNEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/hyperbox/vmlinux"));

    let root_template_dir = std::env::var("HYPERBOX_FIRECRACKER_TEMPLATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("templates"));

    let mut rootfs_by_template = HashMap::new();
    for template in [
        "python:3.11",
        "python:3.12",
        "node:18",
        "node:20",
        "golang:1.22",
        "rust:1.75",
        "ubuntu:22.04",
    ] {
        let dir_name = template.replace(':', "-");
        rootfs_by_template.insert(
            template.to_string(),
            root_template_dir.join(dir_name).join("rootfs.ext4"),
        );
    }

    FirecrackerBackend::new(FirecrackerBackendConfig {
        work_dir,
        firecracker_binary: hyperbox_firecracker::FirecrackerBinary::default(),
        kernel_image,
        agent_endpoint: std::env::var("HYPERBOX_AGENT_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:60061".to_string()),
        rootfs_by_template,
        host_iface: std::env::var("HYPERBOX_HOST_IFACE").unwrap_or_else(|_| "eth0".to_string()),
        network_dry_run: std::env::var("HYPERBOX_NETWORK_DRY_RUN")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(false),
    })
}

fn build_apple_backend() -> AppleVzBackend {
    let caps = detect_macos_capabilities();
    let runtime_kind = match std::env::var("HYPERBOX_APPLE_RUNTIME")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "containerization" => AppleRuntimeKind::Containerization,
        "virtualization" => AppleRuntimeKind::Virtualization,
        _ => {
            if caps.supports_containerization_framework {
                AppleRuntimeKind::Containerization
            } else {
                AppleRuntimeKind::Virtualization
            }
        }
    };

    let launch_command = std::env::var("HYPERBOX_APPLE_HELPER")
        .ok()
        .map(|cmd| {
            cmd.split_whitespace()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|parts| !parts.is_empty());

    AppleVzBackend::new(AppleBackendConfig {
        work_dir: std::env::var("HYPERBOX_APPLE_WORKDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("hyperbox-apple")),
        agent_endpoint: std::env::var("HYPERBOX_AGENT_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:60061".to_string()),
        launch_command,
        runtime_kind,
    })
}
