use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc};

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
            let helper_command = resolve_apple_helper_command();
            if caps.supports_virtualization_framework
                && helper_command
                    .as_ref()
                    .is_some_and(|cmd| apple_runtime_is_implemented_for_host(&caps, cmd))
            {
                Arc::new(build_apple_backend_with_helper(helper_command))
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
    build_apple_backend_with_helper(resolve_apple_helper_command())
}

fn build_apple_backend_with_helper(launch_command: Option<Vec<String>>) -> AppleVzBackend {
    let caps = detect_macos_capabilities();
    let helper_is_builtin = launch_command
        .as_ref()
        .map(|cmd| is_builtin_apple_helper(cmd))
        .unwrap_or(false);
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
            } else if helper_is_builtin {
                // Built-in helper currently only implements containerization runtime.
                AppleRuntimeKind::Containerization
            } else {
                AppleRuntimeKind::Virtualization
            }
        }
    };

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

fn resolve_apple_helper_command() -> Option<Vec<String>> {
    if let Some(raw) = std::env::var("HYPERBOX_APPLE_HELPER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        return Some(raw.split_whitespace().map(ToString::to_string).collect());
    }

    if let Ok(current_exe) = std::env::current_exe() {
        let command = vec![
            current_exe.to_string_lossy().to_string(),
            "apple-helper".to_string(),
        ];
        if helper_command_supports_help(&command) {
            return Some(command);
        }
    }

    let path_command = vec!["hyperbox".to_string(), "apple-helper".to_string()];
    if helper_command_supports_help(&path_command) {
        return Some(path_command);
    }

    None
}

fn helper_command_supports_help(command: &[String]) -> bool {
    if command.is_empty() {
        return false;
    }
    let mut process = std::process::Command::new(&command[0]);
    process.args(&command[1..]);
    process.arg("--help");
    process.stdin(Stdio::null());
    process.stdout(Stdio::null());
    process.stderr(Stdio::null());
    process
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn apple_runtime_is_implemented_for_host(
    caps: &hyperbox_apple::MacOsCapabilities,
    command: &[String],
) -> bool {
    if is_builtin_apple_helper(command) {
        return caps.supports_containerization_framework;
    }
    true
}

fn is_builtin_apple_helper(command: &[String]) -> bool {
    command.len() >= 2 && command[1] == "apple-helper"
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use hyperbox_apple::MacOsCapabilities;

    use super::{
        apple_runtime_is_implemented_for_host, helper_command_supports_help,
        resolve_apple_helper_command,
    };

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("lock env mutex")
    }

    #[test]
    fn helper_support_check_handles_missing_binary() {
        let supported = helper_command_supports_help(&["definitely-not-a-real-binary".to_string()]);
        assert!(!supported);
    }

    #[test]
    fn env_helper_override_takes_precedence() {
        let _guard = env_lock();
        // SAFETY: tests in this crate are single-process and this variable is restored.
        unsafe {
            std::env::set_var("HYPERBOX_APPLE_HELPER", "custom-helper --foo");
        }
        let command = resolve_apple_helper_command();
        // SAFETY: cleanup companion for the set_var above.
        unsafe {
            std::env::remove_var("HYPERBOX_APPLE_HELPER");
        }

        assert_eq!(
            command,
            Some(vec!["custom-helper".to_string(), "--foo".to_string()])
        );
    }

    #[test]
    fn builtin_helper_requires_containerization_support() {
        let caps = MacOsCapabilities {
            os: "macos".to_string(),
            version: "15.0".to_string(),
            major_version: 15,
            is_apple_silicon: true,
            supports_virtualization_framework: true,
            supports_containerization_framework: false,
            has_container_cli: false,
            has_containerization_framework: false,
        };

        assert!(!apple_runtime_is_implemented_for_host(
            &caps,
            &["hyperbox".to_string(), "apple-helper".to_string()]
        ));
    }
}
