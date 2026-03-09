use std::{
    collections::BTreeSet,
    collections::HashMap,
    fs::File,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use tar::{Archive, Builder};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{Duration, sleep},
};
use tracing::{info, warn};

use hyperbox_core::config::normalize_allowlist_domains;
use hyperbox_core::{
    FilePayload, HyperboxError, NetworkMode, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SandboxLease, SandboxState, SnapshotId,
};
use hyperbox_network::{
    CommandExecutor, FirewallManager, NetworkPolicyEvaluator, RecordingExecutor, ShellExecutor,
    VmNetworkSpec, build_allowlist_population_plan,
};
use hyperbox_proto::hyperbox::v1::hyperbox_agent_client::HyperboxAgentClient;

use crate::{FirecrackerBinary, FirecrackerVmConfig, RunningVm, start_vm};

#[derive(Debug, Clone)]
pub struct FirecrackerBackendConfig {
    pub work_dir: PathBuf,
    pub firecracker_binary: FirecrackerBinary,
    pub kernel_image: PathBuf,
    pub agent_endpoint: String,
    pub agent_root: PathBuf,
    pub auto_start_agent: bool,
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
            agent_root: std::env::temp_dir().join("hyperbox-agentd"),
            auto_start_agent: true,
            rootfs_by_template: HashMap::new(),
            host_iface: "eth0".to_string(),
            network_dry_run: true,
        }
    }
}

#[derive(Debug, Clone)]
struct AgentWorkspaceBinding {
    path: PathBuf,
    managed: bool,
    linked_workspace: bool,
}

#[derive(Debug)]
struct FirecrackerSandbox {
    config: SandboxConfig,
    info: SandboxInfo,
    vm: RunningVm,
    network_spec: Option<VmNetworkSpec>,
    allowlist_sync_task: Option<JoinHandle<()>>,
    agent_workspace: AgentWorkspaceBinding,
}

#[derive(Debug, Default)]
struct AgentRuntimeState {
    started: bool,
}

#[derive(Clone)]
pub struct FirecrackerBackend {
    config: FirecrackerBackendConfig,
    sandboxes: Arc<Mutex<HashMap<SandboxId, FirecrackerSandbox>>>,
    agent_runtime: Arc<Mutex<AgentRuntimeState>>,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerBackendConfig) -> Self {
        Self {
            config,
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
            agent_runtime: Arc::new(Mutex::new(AgentRuntimeState::default())),
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

    async fn resolve_allowlist_ips(&self, domains: &[String]) -> Result<Vec<IpAddr>> {
        resolve_allowlist_ips(domains).await
    }

    async fn ensure_agent_running(&self) -> Result<()> {
        if !self.config.auto_start_agent {
            return Err(HyperboxError::ExecutionFailed(format!(
                "connect agent: endpoint {} is unavailable and auto-start is disabled",
                self.config.agent_endpoint
            )));
        }

        let addr = parse_agent_socket_addr(&self.config.agent_endpoint)?;
        let root = self.config.agent_root.clone();
        let mut state = self.agent_runtime.lock().await;
        if state.started {
            return Ok(());
        }
        state.started = true;
        info!(
            endpoint = %self.config.agent_endpoint,
            root = %root.display(),
            "starting embedded firecracker agent sidecar"
        );
        tokio::spawn(async move {
            if let Err(err) = hyperbox_agent::serve_agent(addr, root).await {
                warn!(error = %err, "embedded firecracker agent sidecar exited");
            }
        });
        Ok(())
    }

    async fn connect_agent(&self) -> Result<HyperboxAgentClient<tonic::transport::Channel>> {
        match HyperboxAgentClient::connect(self.config.agent_endpoint.clone()).await {
            Ok(client) => Ok(client),
            Err(first_err) => {
                self.ensure_agent_running().await?;
                let mut last_error = first_err.to_string();
                for _ in 0..20 {
                    sleep(Duration::from_millis(100)).await;
                    match HyperboxAgentClient::connect(self.config.agent_endpoint.clone()).await {
                        Ok(client) => return Ok(client),
                        Err(err) => {
                            last_error = err.to_string();
                        }
                    }
                }
                let mut state = self.agent_runtime.lock().await;
                state.started = false;
                Err(HyperboxError::ExecutionFailed(format!(
                    "connect agent at {} after auto-start: {last_error}",
                    self.config.agent_endpoint
                )))
            }
        }
    }

    async fn prepare_agent_workspace(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
    ) -> Result<AgentWorkspaceBinding> {
        tokio::fs::create_dir_all(&self.config.agent_root).await?;
        let sandbox_path = self.config.agent_root.join(sandbox_id.0.to_string());
        remove_existing_path(&sandbox_path).await?;

        if let Some(workspace_dir) = &config.workspace_dir {
            let workspace = resolve_workspace_dir(workspace_dir).await?;
            create_symlink(&workspace, &sandbox_path).map_err(|e| {
                HyperboxError::ExecutionFailed(format!(
                    "link agent sandbox path `{}` -> `{}`: {e}",
                    sandbox_path.display(),
                    workspace.display()
                ))
            })?;
            return Ok(AgentWorkspaceBinding {
                path: sandbox_path,
                managed: false,
                linked_workspace: true,
            });
        }

        tokio::fs::create_dir_all(&sandbox_path).await?;
        Ok(AgentWorkspaceBinding {
            path: sandbox_path,
            managed: true,
            linked_workspace: false,
        })
    }

    async fn cleanup_agent_workspace(&self, binding: &AgentWorkspaceBinding) -> Result<()> {
        if binding.linked_workspace {
            match tokio::fs::remove_file(&binding.path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "remove agent workspace link `{}`: {err}",
                        binding.path.display()
                    )));
                }
            }
            return Ok(());
        }

        if binding.managed {
            match tokio::fs::remove_dir_all(&binding.path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "remove managed agent workspace `{}`: {err}",
                        binding.path.display()
                    )));
                }
            }
        }
        Ok(())
    }

    async fn archive_workspace(source: PathBuf, artifact_path: PathBuf) -> Result<()> {
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = File::create(&artifact_path)?;
            let encoder = GzEncoder::new(file, Compression::default());
            let mut builder = Builder::new(encoder);
            builder.append_dir_all(".", &source)?;
            let encoder = builder.into_inner()?;
            encoder.finish()?;
            Ok(())
        })
        .await
        .map_err(|e| {
            HyperboxError::ExecutionFailed(format!("snapshot archive task join failed: {e}"))
        })?
    }

    async fn unpack_workspace(artifact_path: PathBuf, destination: PathBuf) -> Result<()> {
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = File::open(&artifact_path)?;
            let decoder = GzDecoder::new(file);
            let mut archive = Archive::new(decoder);
            archive.unpack(&destination)?;
            Ok(())
        })
        .await
        .map_err(|e| {
            HyperboxError::ExecutionFailed(format!("snapshot restore task join failed: {e}"))
        })?
    }
}

fn parse_agent_socket_addr(endpoint: &str) -> Result<SocketAddr> {
    let without_scheme = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);
    without_scheme.parse::<SocketAddr>().map_err(|e| {
        HyperboxError::InvalidConfig(format!(
            "invalid HYPERBOX_AGENT_ENDPOINT `{endpoint}`: expected host:port ({e})"
        ))
    })
}

async fn remove_existing_path(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || metadata.is_file() {
                tokio::fs::remove_file(path).await?;
            } else if metadata.is_dir() {
                tokio::fs::remove_dir_all(path).await?;
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(HyperboxError::ExecutionFailed(format!(
            "inspect path `{}`: {err}",
            path.display()
        ))),
    }
}

async fn resolve_workspace_dir(raw: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .map_err(|e| HyperboxError::ExecutionFailed(format!("resolve current dir: {e}")))?
            .join(candidate)
    };
    tokio::fs::create_dir_all(&resolved).await?;
    let canonical = tokio::fs::canonicalize(&resolved).await.map_err(|e| {
        HyperboxError::ExecutionFailed(format!(
            "canonicalize workspace `{}`: {e}",
            resolved.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(HyperboxError::ExecutionFailed(format!(
            "workspace_dir must be a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

#[cfg(unix)]
fn create_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(not(unix))]
fn create_symlink(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlink is unsupported on this platform",
    ))
}

async fn resolve_allowlist_ips(domains: &[String]) -> Result<Vec<IpAddr>> {
    let domains = normalize_allowlist_domains(domains).map_err(HyperboxError::InvalidConfig)?;
    let mut resolved = BTreeSet::new();
    for domain in domains {
        let entries = tokio::net::lookup_host((domain.as_str(), 443))
            .await
            .map_err(|e| {
                HyperboxError::ExecutionFailed(format!("resolve allowlist domain `{domain}`: {e}"))
            })?;
        for entry in entries {
            resolved.insert(entry.ip());
        }
    }

    if resolved.is_empty() {
        return Err(HyperboxError::ExecutionFailed(
            "allowlist resolution returned zero IP addresses".to_string(),
        ));
    }

    Ok(resolved.into_iter().collect())
}

async fn populate_allowlist_set(vm_id: &str, ips: &[IpAddr]) -> Result<()> {
    let commands = build_allowlist_population_plan(vm_id, ips);
    let executor = ShellExecutor;
    for command in commands {
        executor.run(command).await.map_err(|e| {
            HyperboxError::ExecutionFailed(format!("populate allowlist ipset: {e}"))
        })?;
    }
    Ok(())
}

fn allowlist_refresh_interval_secs() -> u64 {
    std::env::var("HYPERBOX_ALLOWLIST_REFRESH_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(60)
}

#[async_trait::async_trait]
impl SandboxBackend for FirecrackerBackend {
    async fn create(&self, config: SandboxConfig) -> Result<SandboxLease> {
        if !matches!(config.network, NetworkMode::None) && self.config.network_dry_run {
            return Err(HyperboxError::InvalidConfig(
                "network policy requires real firewall enforcement; set HYPERBOX_NETWORK_DRY_RUN=0"
                    .to_string(),
            ));
        }
        if let NetworkMode::Allowlist(domains) = &config.network {
            if domains.is_empty() {
                return Err(HyperboxError::InvalidConfig(
                    "allowlist mode requires at least one --allow domain".to_string(),
                ));
            }
        }

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

        let mut allowlist_sync_task = None;
        let network_spec = if matches!(config.network, hyperbox_core::NetworkMode::None) {
            None
        } else {
            let spec = VmNetworkSpec {
                vm_id: id.0.to_string(),
                tap_name: vm
                    .config
                    .tap_name
                    .clone()
                    .unwrap_or_else(|| "hbxunknown".to_string()),
                host_iface: self.config.host_iface.clone(),
                guest_cidr: "172.16.0.0/30".to_string(),
                guest_ip: "172.16.0.2".to_string(),
            };

            if self.config.network_dry_run {
                let fw = FirewallManager::new(RecordingExecutor::default());
                fw.apply(&spec, &config.network).await.map_err(|e| {
                    HyperboxError::ExecutionFailed(format!("dry-run firewall apply: {e}"))
                })?;
            } else {
                let fw = FirewallManager::new(ShellExecutor);
                fw.apply(&spec, &config.network)
                    .await
                    .map_err(|e| HyperboxError::ExecutionFailed(format!("firewall apply: {e}")))?;
            }

            // Trigger evaluator construction to validate allowlist syntax early.
            let _ = NetworkPolicyEvaluator::new(&config.network);

            if let NetworkMode::Allowlist(domains) = &config.network {
                let ips = self.resolve_allowlist_ips(domains).await?;
                populate_allowlist_set(&spec.vm_id, &ips).await?;

                let vm_id = spec.vm_id.clone();
                let domains = domains.clone();
                let refresh_every = allowlist_refresh_interval_secs();
                allowlist_sync_task = Some(tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(Duration::from_secs(refresh_every));
                    loop {
                        ticker.tick().await;
                        match resolve_allowlist_ips(&domains).await {
                            Ok(ips) => {
                                if let Err(err) = populate_allowlist_set(&vm_id, &ips).await {
                                    warn!(
                                        vm_id = %vm_id,
                                        error = %err,
                                        "allowlist refresh apply failed"
                                    );
                                }
                            }
                            Err(err) => {
                                warn!(
                                    vm_id = %vm_id,
                                    error = %err,
                                    "allowlist refresh resolve failed"
                                );
                            }
                        }
                    }
                }));
            }
            Some(spec)
        };

        let info = SandboxInfo {
            id: id.clone(),
            template: config.template.clone(),
            state: SandboxState::Ready,
            created_at: Utc::now(),
        };
        let agent_workspace = self.prepare_agent_workspace(&id, &config).await?;

        self.sandboxes.lock().await.insert(
            id.clone(),
            FirecrackerSandbox {
                config,
                info: info.clone(),
                vm,
                network_spec,
                allowlist_sync_task,
                agent_workspace,
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

        let mut agent = self.connect_agent().await?;

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

        let mut agent = self.connect_agent().await?;

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

        let mut agent = self.connect_agent().await?;

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

    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        _snapshot_id: &SnapshotId,
        artifact_path: &Path,
    ) -> Result<()> {
        let workspace_path = {
            let sandboxes = self.sandboxes.lock().await;
            let sandbox = sandboxes
                .get(sandbox_id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;
            sandbox.agent_workspace.path.clone()
        };

        let source = tokio::fs::canonicalize(&workspace_path)
            .await
            .map_err(|e| {
                HyperboxError::ExecutionFailed(format!(
                    "snapshot source does not exist `{}`: {e}",
                    workspace_path.display()
                ))
            })?;
        let metadata = tokio::fs::metadata(&source).await?;
        if !metadata.is_dir() {
            return Err(HyperboxError::ExecutionFailed(format!(
                "snapshot source is not a directory: {}",
                source.display()
            )));
        }

        if let Some(parent) = artifact_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if artifact_path.exists() {
            tokio::fs::remove_file(artifact_path).await?;
        }
        Self::archive_workspace(source, artifact_path.to_path_buf()).await
    }

    async fn restore_snapshot(
        &self,
        snapshot_id: &SnapshotId,
        artifact_path: &Path,
        config: SandboxConfig,
    ) -> Result<SandboxLease> {
        if !artifact_path.exists() {
            return Err(HyperboxError::ExecutionFailed(format!(
                "snapshot artifact missing for {} at {}",
                snapshot_id.0,
                artifact_path.display()
            )));
        }

        let lease = self.create(config).await?;
        let binding = {
            let sandboxes = self.sandboxes.lock().await;
            let sandbox = sandboxes
                .get(&lease.id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(lease.id.0.to_string()))?;
            sandbox.agent_workspace.clone()
        };

        if binding.linked_workspace {
            let _ = self.destroy(&lease.id).await;
            return Err(HyperboxError::ExecutionFailed(
                "firecracker snapshot restore into shared workspace_dir is not supported"
                    .to_string(),
            ));
        }

        if binding.path.exists() {
            tokio::fs::remove_dir_all(&binding.path).await?;
        }
        tokio::fs::create_dir_all(&binding.path).await?;

        if let Err(err) =
            Self::unpack_workspace(artifact_path.to_path_buf(), binding.path.clone()).await
        {
            let _ = self.destroy(&lease.id).await;
            return Err(HyperboxError::ExecutionFailed(format!(
                "firecracker snapshot restore failed for {}: {err}",
                snapshot_id.0
            )));
        }
        Ok(lease)
    }

    async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()> {
        let mut sandboxes = self.sandboxes.lock().await;
        let mut sandbox = sandboxes
            .remove(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(task) = sandbox.allowlist_sync_task.take() {
            task.abort();
        }

        if let Some(spec) = &sandbox.network_spec {
            if self.config.network_dry_run {
                let fw = FirewallManager::new(RecordingExecutor::default());
                fw.teardown(spec).await.map_err(|e| {
                    HyperboxError::ExecutionFailed(format!("dry-run firewall teardown: {e}"))
                })?;
            } else {
                let fw = FirewallManager::new(ShellExecutor);
                fw.teardown(spec).await.map_err(|e| {
                    HyperboxError::ExecutionFailed(format!("firewall teardown: {e}"))
                })?;
            }
        }

        sandbox
            .vm
            .terminate()
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("terminate vm: {e}")))?;
        self.cleanup_agent_workspace(&sandbox.agent_workspace)
            .await?;
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

#[cfg(test)]
mod tests {
    use super::{FirecrackerBackend, FirecrackerBackendConfig, parse_agent_socket_addr};
    use hyperbox_core::{NetworkMode, Result, SandboxConfig, SandboxId};

    #[test]
    fn parses_agent_socket_addr_from_url() {
        let parsed = parse_agent_socket_addr("http://127.0.0.1:60061").expect("parse endpoint");
        assert_eq!(parsed.to_string(), "127.0.0.1:60061");
    }

    #[test]
    fn rejects_invalid_agent_endpoint() {
        let err =
            parse_agent_socket_addr("http://localhost").expect_err("invalid endpoint must fail");
        assert!(
            err.to_string()
                .contains("invalid HYPERBOX_AGENT_ENDPOINT `http://localhost`")
        );
    }

    #[tokio::test]
    async fn prepare_agent_workspace_links_shared_workspace() {
        let base = std::env::temp_dir().join(format!(
            "hyperbox-firecracker-test-{}",
            uuid::Uuid::new_v4()
        ));
        let agent_root = base.join("agent-root");
        let workspace = base.join("workspace");
        tokio::fs::create_dir_all(&workspace)
            .await
            .expect("create workspace");

        let backend = FirecrackerBackend::new(FirecrackerBackendConfig {
            agent_root: agent_root.clone(),
            ..FirecrackerBackendConfig::default()
        });
        let sandbox_id = SandboxId::new();
        let binding = backend
            .prepare_agent_workspace(
                &sandbox_id,
                &SandboxConfig {
                    workspace_dir: Some(workspace.to_string_lossy().to_string()),
                    network: NetworkMode::None,
                    ..SandboxConfig::default()
                },
            )
            .await
            .expect("prepare linked workspace");

        let metadata = tokio::fs::symlink_metadata(&binding.path)
            .await
            .expect("workspace binding metadata");
        assert!(metadata.file_type().is_symlink());
        assert!(binding.linked_workspace);
        assert!(!binding.managed);

        backend
            .cleanup_agent_workspace(&binding)
            .await
            .expect("cleanup linked workspace");
        assert!(!binding.path.exists());

        tokio::fs::remove_dir_all(&base)
            .await
            .expect("cleanup test base");
    }

    #[tokio::test]
    async fn snapshot_archive_roundtrip_preserves_workspace_files() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "hyperbox-firecracker-snap-{}",
            uuid::Uuid::new_v4()
        ));
        let source = base.join("source");
        let restored = base.join("restored");
        tokio::fs::create_dir_all(source.join("nested")).await?;
        tokio::fs::write(source.join("root.txt"), b"alpha").await?;
        tokio::fs::write(source.join("nested/data.txt"), b"beta").await?;

        let artifact = base.join("snapshot.tar.gz");
        FirecrackerBackend::archive_workspace(source.clone(), artifact.clone()).await?;
        tokio::fs::create_dir_all(&restored).await?;
        FirecrackerBackend::unpack_workspace(artifact, restored.clone()).await?;

        let root = tokio::fs::read_to_string(restored.join("root.txt")).await?;
        let nested = tokio::fs::read_to_string(restored.join("nested/data.txt")).await?;
        assert_eq!(root, "alpha");
        assert_eq!(nested, "beta");

        tokio::fs::remove_dir_all(base).await?;
        Ok(())
    }
}
