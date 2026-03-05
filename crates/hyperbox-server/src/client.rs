use hyperbox_core::{ExecOutcome, ExecRequest, NetworkMode, SandboxConfig, SandboxInfo};
use hyperbox_proto::hyperbox::v1::{self as pb, hyperbox_control_client::HyperboxControlClient};

#[derive(Debug, Clone)]
pub struct GrpcControlClient {
    inner: HyperboxControlClient<tonic::transport::Channel>,
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub server_version: String,
    pub process_id: String,
    pub executable_path: String,
    pub started_at: String,
    pub backend_requested: String,
    pub backend_selected: String,
    pub backend_reason: String,
    pub apple_runtime: Option<String>,
    pub apple_helper_argv: Vec<String>,
}

impl GrpcControlClient {
    pub async fn connect(endpoint: String) -> anyhow::Result<Self> {
        let inner = HyperboxControlClient::connect(endpoint).await?;
        Ok(Self { inner })
    }

    pub async fn list_templates(&mut self) -> anyhow::Result<Vec<String>> {
        let response = self
            .inner
            .list_templates(pb::ListTemplatesRequest {})
            .await?
            .into_inner();
        Ok(response.templates)
    }

    pub async fn get_server_info(&mut self) -> anyhow::Result<ServerInfo> {
        let response = self
            .inner
            .get_server_info(pb::ServerInfoRequest {})
            .await?
            .into_inner();
        Ok(ServerInfo {
            server_version: response.server_version,
            process_id: response.process_id,
            executable_path: response.executable_path,
            started_at: response.started_at,
            backend_requested: response.backend_requested,
            backend_selected: response.backend_selected,
            backend_reason: response.backend_reason,
            apple_runtime: if response.apple_runtime.is_empty() {
                None
            } else {
                Some(response.apple_runtime)
            },
            apple_helper_argv: response.apple_helper_argv,
        })
    }

    pub async fn create_sandbox(&mut self, config: SandboxConfig) -> anyhow::Result<SandboxInfo> {
        let request = pb::CreateSandboxRequest {
            config: Some(pb::SandboxConfig {
                template: config.template,
                memory_mb: config.memory_mb,
                vcpu_count: config.vcpu_count as u32,
                timeout_secs: config.timeout_secs,
                env: config.env.into_iter().collect(),
                workspace_dir: config.workspace_dir.unwrap_or_default(),
                network_mode: match config.network {
                    NetworkMode::None => pb::NetworkMode::None as i32,
                    NetworkMode::Allowlist(_) => pb::NetworkMode::Allowlist as i32,
                    NetworkMode::Full => pb::NetworkMode::Full as i32,
                },
                network_allowlist: match config.network {
                    NetworkMode::Allowlist(v) => v,
                    _ => vec![],
                },
            }),
        };

        let info = self
            .inner
            .create_sandbox(request)
            .await?
            .into_inner()
            .info
            .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?;

        Ok(SandboxInfo {
            id: hyperbox_core::SandboxId(uuid::Uuid::parse_str(&info.id)?),
            template: info.template,
            state: match info.state.as_str() {
                "Provisioning" => hyperbox_core::SandboxState::Provisioning,
                "Busy" => hyperbox_core::SandboxState::Busy,
                "Stopped" => hyperbox_core::SandboxState::Stopped,
                "Failed" => hyperbox_core::SandboxState::Failed,
                _ => hyperbox_core::SandboxState::Ready,
            },
            created_at: chrono::DateTime::parse_from_rfc3339(&info.created_at)?
                .with_timezone(&chrono::Utc),
        })
    }

    pub async fn exec(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
        request: ExecRequest,
    ) -> anyhow::Result<ExecOutcome> {
        let response = self
            .inner
            .exec(pb::ExecRequest {
                sandbox_id: sandbox_id.0.to_string(),
                command: request.command,
                timeout_secs: request.timeout_secs,
            })
            .await?
            .into_inner();

        Ok(ExecOutcome {
            exit_code: response.exit_code,
            stdout: response.stdout,
            stderr: response.stderr,
            duration_ms: response.duration_ms as u128,
        })
    }

    pub async fn destroy_sandbox(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
    ) -> anyhow::Result<()> {
        self.inner
            .destroy_sandbox(pb::DestroySandboxRequest {
                sandbox_id: sandbox_id.0.to_string(),
            })
            .await?;
        Ok(())
    }

    pub async fn inspect_sandbox(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
    ) -> anyhow::Result<SandboxInfo> {
        let info = self
            .inner
            .inspect_sandbox(pb::InspectSandboxRequest {
                sandbox_id: sandbox_id.0.to_string(),
            })
            .await?
            .into_inner()
            .info
            .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?;

        Ok(SandboxInfo {
            id: hyperbox_core::SandboxId(uuid::Uuid::parse_str(&info.id)?),
            template: info.template,
            state: hyperbox_core::SandboxState::Ready,
            created_at: chrono::DateTime::parse_from_rfc3339(&info.created_at)?
                .with_timezone(&chrono::Utc),
        })
    }

    pub async fn write_file(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
        path: String,
        bytes: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.inner
            .write_file(pb::WriteFileRequest {
                sandbox_id: sandbox_id.0.to_string(),
                path,
                bytes,
            })
            .await?;
        Ok(())
    }

    pub async fn read_file(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
        path: String,
    ) -> anyhow::Result<Vec<u8>> {
        let response = self
            .inner
            .read_file(pb::ReadFileRequest {
                sandbox_id: sandbox_id.0.to_string(),
                path,
            })
            .await?
            .into_inner();
        Ok(response.bytes)
    }
}
