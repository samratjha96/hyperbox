use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use tokio::sync::Mutex;

use hyperbox_core::{
    FilePayload, HyperboxError, Result, SandboxBackend, SandboxConfig, SandboxId, SandboxInfo,
    SandboxLease, SandboxState,
};
use hyperbox_network::{
    FirewallManager, NetworkPolicyEvaluator, RecordingExecutor, ShellExecutor, VmNetworkSpec,
};
use hyperbox_proto::hyperbox::v1::hyperbox_agent_client::HyperboxAgentClient;

use crate::{FirecrackerBinary, FirecrackerVmConfig, RunningVm, start_vm};

#[derive(Debug, Clone)]
pub struct FirecrackerBackendConfig {
    pub work_dir: PathBuf,
    pub firecracker_binary: FirecrackerBinary,
    pub kernel_image: PathBuf,
    pub agent_endpoint: String,
    pub rootfs_by_template: HashMap<String, PathBuf>,
    pub host_iface: String,
    pub network_dry_run: bool,
}

impl Default for FirecrackerBackendConfig {
    fn default() -> Self {
        Self {
            work_dir: std::env::temp_dir().join("hyperbox-firecracker"),
            firecracker_binary: FirecrackerBinary::default(),
            kernel_image: PathBuf::from("/var/lib/hyperbox/vmlinux"),
            agent_endpoint: "http://127.0.0.1:60061".to_string(),
            rootfs_by_template: HashMap::new(),
            host_iface: "eth0".to_string(),
            network_dry_run: true,
        }
    }
}

#[derive(Debug)]
struct FirecrackerSandbox {
    config: SandboxConfig,
    info: SandboxInfo,
    vm: RunningVm,
    network_spec: Option<VmNetworkSpec>,
}

#[derive(Clone)]
pub struct FirecrackerBackend {
    config: FirecrackerBackendConfig,
    sandboxes: Arc<Mutex<HashMap<SandboxId, FirecrackerSandbox>>>,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerBackendConfig) -> Self {
        Self {
            config,
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn resolve_rootfs(&self, template: &str) -> Result<&Path> {
        self.config
            .rootfs_by_template
            .get(template)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                HyperboxError::TemplateNotFound(format!(
                    "{template} (missing firecracker rootfs mapping)"
                ))
            })
    }

    fn vm_paths(&self, sandbox_id: &SandboxId) -> (PathBuf, PathBuf, PathBuf) {
        let base = self.config.work_dir.join(sandbox_id.0.to_string());
        let socket = base.join("firecracker.sock");
        let vsock = base.join("vsock.sock");
        let log = base.join("firecracker.log");
        (socket, vsock, log)
    }
}

#[async_trait::async_trait]
impl SandboxBackend for FirecrackerBackend {
    async fn create(&self, config: SandboxConfig) -> Result<SandboxLease> {
        tokio::fs::create_dir_all(&self.config.work_dir).await?;

        let id = SandboxId::new();
        let (socket_path, vsock_path, log_path) = self.vm_paths(&id);
        if let Some(parent) = socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let rootfs_path = self.resolve_rootfs(&config.template)?.to_path_buf();
        let vm_config = FirecrackerVmConfig {
            vm_id: id.0.to_string(),
            socket_path,
            kernel_image_path: self.config.kernel_image.clone(),
            rootfs_path,
            log_path,
            memory_mb: config.memory_mb,
            vcpu_count: config.vcpu_count,
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
            tap_name: (!matches!(config.network, hyperbox_core::NetworkMode::None))
                .then(|| format!("hbx{}", &id.0.to_string()[..8])),
            vsock_guest_cid: 3,
            vsock_uds_path: vsock_path,
        };

        let vm = start_vm(&self.config.firecracker_binary, vm_config)
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("start firecracker vm: {e}")))?;

        let network_spec = if matches!(config.network, hyperbox_core::NetworkMode::None) {
            None
        } else {
            let spec = VmNetworkSpec {
                vm_id: id.0.to_string(),
                tap_name: vm.config.tap_name.clone().unwrap_or_else(|| "hbxunknown".to_string()),
                host_iface: self.config.host_iface.clone(),
                guest_cidr: "172.16.0.0/30".to_string(),
                guest_ip: "172.16.0.2".to_string(),
            };

            if self.config.network_dry_run {
                let fw = FirewallManager::new(RecordingExecutor::default());
                fw.apply(&spec, &config.network)
                    .await
                    .map_err(|e| HyperboxError::ExecutionFailed(format!("dry-run firewall apply: {e}")))?;
            } else {
                let fw = FirewallManager::new(ShellExecutor);
                fw.apply(&spec, &config.network)
                    .await
                    .map_err(|e| HyperboxError::ExecutionFailed(format!("firewall apply: {e}")))?;
            }

            // Trigger evaluator construction to validate allowlist syntax early.
            let _ = NetworkPolicyEvaluator::new(&config.network);
            Some(spec)
        };

        let info = SandboxInfo {
            id: id.clone(),
            template: config.template.clone(),
            state: SandboxState::Ready,
            created_at: Utc::now(),
        };

        self.sandboxes.lock().await.insert(
            id.clone(),
            FirecrackerSandbox {
                config,
                info: info.clone(),
                vm,
                network_spec,
            },
        );

        Ok(SandboxLease { id, info })
    }

    async fn exec(
        &self,
        sandbox_id: &SandboxId,
        req: hyperbox_core::ExecRequest,
    ) -> Result<hyperbox_core::ExecOutcome> {
        if req.command.is_empty() {
            return Err(HyperboxError::InvalidConfig(
                "command cannot be empty".to_string(),
            ));
        }

        let exists = self.sandboxes.lock().await.contains_key(sandbox_id);
        if !exists {
            return Err(HyperboxError::SandboxNotFound(sandbox_id.0.to_string()));
        }

        let mut agent = HyperboxAgentClient::connect(self.config.agent_endpoint.clone())
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("connect agent: {e}")))?;

        let response = agent
            .exec(hyperbox_proto::hyperbox::v1::ExecRequest {
                sandbox_id: sandbox_id.0.to_string(),
                command: req.command,
                timeout_secs: req.timeout_secs,
            })
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("exec via agent: {e}")))?
            .into_inner();

        Ok(hyperbox_core::ExecOutcome {
            exit_code: response.exit_code,
            stdout: response.stdout,
            stderr: response.stderr,
            duration_ms: response.duration_ms as u128,
        })
    }

    async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        let exists = self.sandboxes.lock().await.contains_key(sandbox_id);
        if !exists {
            return Err(HyperboxError::SandboxNotFound(sandbox_id.0.to_string()));
        }

        let mut agent = HyperboxAgentClient::connect(self.config.agent_endpoint.clone())
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("connect agent: {e}")))?;

        let response = agent
            .read_file(hyperbox_proto::hyperbox::v1::ReadFileRequest {
                sandbox_id: sandbox_id.0.to_string(),
                path: path.to_string(),
            })
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("read file via agent: {e}")))?
            .into_inner();

        Ok(FilePayload {
            path: path.to_string().into(),
            bytes: response.bytes,
        })
    }

    async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()> {
        let exists = self.sandboxes.lock().await.contains_key(sandbox_id);
        if !exists {
            return Err(HyperboxError::SandboxNotFound(sandbox_id.0.to_string()));
        }

        let mut agent = HyperboxAgentClient::connect(self.config.agent_endpoint.clone())
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("connect agent: {e}")))?;

        agent
            .write_file(hyperbox_proto::hyperbox::v1::WriteFileRequest {
                sandbox_id: sandbox_id.0.to_string(),
                path: payload.path.to_string(),
                bytes: payload.bytes,
            })
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("write file via agent: {e}")))?;

        Ok(())
    }

    async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()> {
        let mut sandboxes = self.sandboxes.lock().await;
        let mut sandbox = sandboxes
            .remove(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(spec) = &sandbox.network_spec {
            if self.config.network_dry_run {
                let fw = FirewallManager::new(RecordingExecutor::default());
                fw.teardown(spec)
                    .await
                    .map_err(|e| HyperboxError::ExecutionFailed(format!("dry-run firewall teardown: {e}")))?;
            } else {
                let fw = FirewallManager::new(ShellExecutor);
                fw.teardown(spec)
                    .await
                    .map_err(|e| HyperboxError::ExecutionFailed(format!("firewall teardown: {e}")))?;
            }
        }

        sandbox
            .vm
            .terminate()
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("terminate vm: {e}")))?;
        Ok(())
    }

    async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .map(|s| {
                let _ = &s.config;
                s.info.clone()
            })
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))
    }
}
