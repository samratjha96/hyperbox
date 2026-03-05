use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::Utc;
use tokio::{process::Child, sync::Mutex};

use hyperbox_core::{
    FilePayload, HyperboxError, NetworkMode, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SandboxLease, SandboxState,
};
use hyperbox_proto::hyperbox::v1::hyperbox_agent_client::HyperboxAgentClient;

#[derive(Debug, Clone)]
pub struct AppleBackendConfig {
    pub work_dir: PathBuf,
    pub agent_endpoint: String,
    pub launch_command: Option<Vec<String>>,
}

impl Default for AppleBackendConfig {
    fn default() -> Self {
        Self {
            work_dir: std::env::temp_dir().join("hyperbox-apple"),
            agent_endpoint: "http://127.0.0.1:60061".to_string(),
            launch_command: None,
        }
    }
}

#[derive(Debug)]
struct AppleSandbox {
    info: SandboxInfo,
    _config: SandboxConfig,
    vm_process: Option<Child>,
}

#[derive(Clone)]
pub struct AppleVzBackend {
    config: AppleBackendConfig,
    sandboxes: Arc<Mutex<HashMap<SandboxId, AppleSandbox>>>,
}

impl AppleVzBackend {
    pub fn new(config: AppleBackendConfig) -> Self {
        Self {
            config,
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn start_vm_if_needed(&self, sandbox_id: &SandboxId) -> Result<Option<Child>> {
        let Some(command) = &self.config.launch_command else {
            return Ok(None);
        };

        if command.is_empty() {
            return Err(HyperboxError::InvalidConfig(
                "apple launch command is empty".to_string(),
            ));
        }

        let mut cmd = tokio::process::Command::new(&command[0]);
        cmd.args(&command[1..]);
        cmd.env("HYPERBOX_SANDBOX_ID", sandbox_id.0.to_string());
        cmd.current_dir(&self.config.work_dir);

        let child = cmd
            .spawn()
            .map_err(|e| HyperboxError::ExecutionFailed(format!("spawn apple vm command: {e}")))?;
        Ok(Some(child))
    }

    async fn ensure_exists(&self, sandbox_id: &SandboxId) -> Result<()> {
        if !self.sandboxes.lock().await.contains_key(sandbox_id) {
            return Err(HyperboxError::SandboxNotFound(sandbox_id.0.to_string()));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl SandboxBackend for AppleVzBackend {
    async fn create(&self, config: SandboxConfig) -> Result<SandboxLease> {
        if !matches!(config.network, NetworkMode::None) {
            return Err(HyperboxError::InvalidConfig(
                "apple backend network policy enforcement is not implemented yet; use network=none"
                    .to_string(),
            ));
        }

        tokio::fs::create_dir_all(&self.config.work_dir).await?;

        let id = SandboxId::new();
        let info = SandboxInfo {
            id: id.clone(),
            template: config.template.clone(),
            state: SandboxState::Ready,
            created_at: Utc::now(),
        };

        let vm_process = self.start_vm_if_needed(&id).await?;

        self.sandboxes.lock().await.insert(
            id.clone(),
            AppleSandbox {
                info: info.clone(),
                _config: config,
                vm_process,
            },
        );

        Ok(SandboxLease { id, info })
    }

    async fn exec(
        &self,
        sandbox_id: &SandboxId,
        req: hyperbox_core::ExecRequest,
    ) -> Result<hyperbox_core::ExecOutcome> {
        self.ensure_exists(sandbox_id).await?;

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
        self.ensure_exists(sandbox_id).await?;

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
        self.ensure_exists(sandbox_id).await?;

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
        let mut sandbox = self
            .sandboxes
            .lock()
            .await
            .remove(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(child) = sandbox.vm_process.as_mut() {
            child
                .kill()
                .await
                .map_err(|e| HyperboxError::ExecutionFailed(format!("kill vm process: {e}")))?;
        }

        Ok(())
    }

    async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .map(|sandbox| sandbox.info.clone())
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))
    }
}
