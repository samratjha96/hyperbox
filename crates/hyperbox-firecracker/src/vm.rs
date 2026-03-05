use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    fs,
    process::{Child, Command},
    time::sleep,
};

use crate::FirecrackerApiClient;

#[derive(Debug, Clone)]
pub struct FirecrackerBinary {
    pub firecracker_path: PathBuf,
}

impl Default for FirecrackerBinary {
    fn default() -> Self {
        Self {
            firecracker_path: PathBuf::from("firecracker"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FirecrackerVmConfig {
    pub vm_id: String,
    pub socket_path: PathBuf,
    pub kernel_image_path: PathBuf,
    pub rootfs_path: PathBuf,
    pub log_path: PathBuf,
    pub memory_mb: u32,
    pub vcpu_count: u8,
    pub boot_args: String,
    pub tap_name: Option<String>,
    pub vsock_guest_cid: u32,
    pub vsock_uds_path: PathBuf,
}

impl FirecrackerVmConfig {
    pub fn minimal(vm_id: String, socket_path: PathBuf, kernel: PathBuf, rootfs: PathBuf) -> Self {
        Self {
            vm_id,
            socket_path,
            kernel_image_path: kernel,
            rootfs_path: rootfs,
            log_path: std::env::temp_dir().join("hyperbox-firecracker.log"),
            memory_mb: 512,
            vcpu_count: 1,
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
            tap_name: None,
            vsock_guest_cid: 3,
            vsock_uds_path: std::env::temp_dir().join("hyperbox-vsock.sock"),
        }
    }
}

#[derive(Debug)]
pub struct RunningVm {
    pub config: FirecrackerVmConfig,
    child: Child,
}

impl RunningVm {
    pub fn api_client(&self) -> FirecrackerApiClient {
        FirecrackerApiClient::new(&self.config.socket_path)
    }

    pub async fn create_snapshot(&self, snapshot_path: &Path, mem_path: &Path) -> anyhow::Result<()> {
        self.api_client()
            .create_snapshot(
                &mem_path.to_string_lossy(),
                &snapshot_path.to_string_lossy(),
            )
            .await
    }

    pub async fn wait(mut self) -> anyhow::Result<std::process::ExitStatus> {
        Ok(self.child.wait().await?)
    }

    pub async fn terminate(&mut self) -> anyhow::Result<()> {
        self.child.kill().await?;
        Ok(())
    }
}

pub async fn start_vm(binary: &FirecrackerBinary, config: FirecrackerVmConfig) -> anyhow::Result<RunningVm> {
    if let Some(parent) = config.socket_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let _ = fs::remove_file(&config.socket_path).await;

    let mut child = Command::new(&binary.firecracker_path)
        .arg("--api-sock")
        .arg(&config.socket_path)
        .arg("--log-path")
        .arg(&config.log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    wait_for_socket(&config.socket_path, Duration::from_secs(3)).await?;

    let api = FirecrackerApiClient::new(&config.socket_path);
    api.set_machine_config(config.vcpu_count, config.memory_mb).await?;
    api.set_boot_source(&config.kernel_image_path.to_string_lossy(), &config.boot_args)
        .await?;
    api.set_rootfs(&config.rootfs_path.to_string_lossy(), false).await?;
    api.set_vsock(config.vsock_guest_cid, &config.vsock_uds_path.to_string_lossy())
        .await?;
    if let Some(tap) = &config.tap_name {
        api.attach_network(tap).await?;
    }
    api.start_instance().await?;

    Ok(RunningVm { config, child })
}

pub async fn restore_vm_from_snapshot(
    binary: &FirecrackerBinary,
    mut config: FirecrackerVmConfig,
    snapshot_path: &Path,
    mem_path: &Path,
) -> anyhow::Result<RunningVm> {
    config.boot_args = "".to_string();
    let vm = start_vm(binary, config).await?;
    vm.api_client()
        .load_snapshot(
            &mem_path.to_string_lossy(),
            &snapshot_path.to_string_lossy(),
        )
        .await?;
    Ok(vm)
}

async fn wait_for_socket(path: &Path, timeout: Duration) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(30)).await;
    }

    anyhow::bail!("timed out waiting for firecracker socket: {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_socket_times_out() {
        let path = std::env::temp_dir().join("definitely-missing-firecracker.sock");
        let err = wait_for_socket(&path, Duration::from_millis(50))
            .await
            .expect_err("should timeout");
        assert!(err.to_string().contains("timed out waiting for firecracker socket"));
    }
}
