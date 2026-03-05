use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

use hyperbox_core::{ExecRequest, FilePayload, NetworkMode, SandboxConfig, SandboxId, SnapshotId};
use hyperbox_proto::hyperbox::v1::{self as pb, hyperbox_control_server::HyperboxControl};

use crate::HyperboxServer;

#[derive(Clone)]
pub struct GrpcControlService {
    runtime: HyperboxServer,
    server_info: ServerRuntimeInfo,
}

#[derive(Clone)]
pub struct ServerRuntimeInfo {
    pub server_version: String,
    pub process_id: u32,
    pub executable_path: String,
    pub started_at: String,
    pub backend_requested: String,
    pub backend_selected: String,
    pub backend_reason: String,
    pub apple_runtime: Option<String>,
    pub apple_helper_argv: Vec<String>,
}

impl GrpcControlService {
    pub fn new(runtime: HyperboxServer, server_info: ServerRuntimeInfo) -> Self {
        Self {
            runtime,
            server_info,
        }
    }
}

fn parse_sandbox_id(raw: &str) -> Result<SandboxId, Status> {
    let id = uuid::Uuid::parse_str(raw)
        .map_err(|e| Status::invalid_argument(format!("invalid sandbox_id: {e}")))?;
    Ok(SandboxId(id))
}

fn into_proto_info(info: hyperbox_core::SandboxInfo) -> pb::SandboxInfo {
    pb::SandboxInfo {
        id: info.id.0.to_string(),
        template: info.template,
        state: format!("{:?}", info.state),
        created_at: info.created_at.to_rfc3339(),
    }
}

fn from_proto_config(config: pb::SandboxConfig) -> SandboxConfig {
    let network =
        match pb::NetworkMode::try_from(config.network_mode).unwrap_or(pb::NetworkMode::None) {
            pb::NetworkMode::Allowlist => NetworkMode::Allowlist(config.network_allowlist),
            pb::NetworkMode::Full => NetworkMode::Full,
            _ => NetworkMode::None,
        };

    SandboxConfig {
        template: if config.template.is_empty() {
            SandboxConfig::default().template
        } else {
            config.template
        },
        memory_mb: if config.memory_mb == 0 {
            SandboxConfig::default().memory_mb
        } else {
            config.memory_mb
        },
        vcpu_count: if config.vcpu_count == 0 {
            SandboxConfig::default().vcpu_count
        } else {
            config.vcpu_count as u8
        },
        workspace_dir: if config.workspace_dir.is_empty() {
            None
        } else {
            Some(config.workspace_dir)
        },
        network,
        env: config.env.into_iter().collect(),
        timeout_secs: if config.timeout_secs == 0 {
            SandboxConfig::default().timeout_secs
        } else {
            config.timeout_secs
        },
    }
}

impl From<crate::MetricsSnapshot> for pb::MetricsResponse {
    fn from(value: crate::MetricsSnapshot) -> Self {
        Self {
            creates: value.creates,
            destroys: value.destroys,
            execs: value.execs,
            exec_failures: value.exec_failures,
            p50_exec_ms: value.p50_exec_ms as u64,
            p95_exec_ms: value.p95_exec_ms as u64,
        }
    }
}

#[tonic::async_trait]
impl HyperboxControl for GrpcControlService {
    async fn create_sandbox(
        &self,
        request: Request<pb::CreateSandboxRequest>,
    ) -> Result<Response<pb::CreateSandboxResponse>, Status> {
        let peer = request.remote_addr();
        let config = request
            .into_inner()
            .config
            .map(from_proto_config)
            .unwrap_or_default();
        info!(
            peer = ?peer,
            template = %config.template,
            memory_mb = config.memory_mb,
            vcpu_count = config.vcpu_count,
            "grpc create_sandbox request"
        );

        let info = self.runtime.create_sandbox(config).await.map_err(|e| {
            error!(peer = ?peer, error = %e, "grpc create_sandbox failed");
            Status::internal(e.to_string())
        })?;

        info!(peer = ?peer, sandbox_id = %info.id.0, template = %info.template, "grpc create_sandbox success");

        Ok(Response::new(pb::CreateSandboxResponse {
            info: Some(into_proto_info(info)),
        }))
    }

    async fn destroy_sandbox(
        &self,
        request: Request<pb::DestroySandboxRequest>,
    ) -> Result<Response<pb::DestroySandboxResponse>, Status> {
        let peer = request.remote_addr();
        let sandbox_id = parse_sandbox_id(&request.into_inner().sandbox_id)?;
        info!(peer = ?peer, sandbox_id = %sandbox_id.0, "grpc destroy_sandbox request");
        self.runtime
            .destroy_sandbox(&sandbox_id)
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc destroy_sandbox failed");
                Status::internal(e.to_string())
            })?;
        info!(peer = ?peer, sandbox_id = %sandbox_id.0, "grpc destroy_sandbox success");
        Ok(Response::new(pb::DestroySandboxResponse {}))
    }

    async fn inspect_sandbox(
        &self,
        request: Request<pb::InspectSandboxRequest>,
    ) -> Result<Response<pb::InspectSandboxResponse>, Status> {
        let peer = request.remote_addr();
        let sandbox_id = parse_sandbox_id(&request.into_inner().sandbox_id)?;
        debug!(peer = ?peer, sandbox_id = %sandbox_id.0, "grpc inspect_sandbox request");
        let info = self
            .runtime
            .inspect(&sandbox_id)
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc inspect_sandbox failed");
                Status::internal(e.to_string())
            })?;

        Ok(Response::new(pb::InspectSandboxResponse {
            info: Some(into_proto_info(info)),
        }))
    }

    async fn exec(
        &self,
        request: Request<pb::ExecRequest>,
    ) -> Result<Response<pb::ExecResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        let command_preview = request.command.join(" ");
        info!(
            peer = ?peer,
            sandbox_id = %sandbox_id.0,
            timeout_secs = request.timeout_secs,
            command = %command_preview,
            "grpc exec request"
        );
        let outcome = self
            .runtime
            .exec(
                &sandbox_id,
                ExecRequest {
                    command: request.command,
                    timeout_secs: request.timeout_secs.max(1),
                },
            )
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc exec failed");
                Status::internal(e.to_string())
            })?;
        info!(
            peer = ?peer,
            sandbox_id = %sandbox_id.0,
            exit_code = outcome.exit_code,
            duration_ms = outcome.duration_ms,
            "grpc exec success"
        );

        Ok(Response::new(pb::ExecResponse {
            exit_code: outcome.exit_code,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            duration_ms: outcome.duration_ms as u64,
        }))
    }

    async fn read_file(
        &self,
        request: Request<pb::ReadFileRequest>,
    ) -> Result<Response<pb::ReadFileResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        debug!(peer = ?peer, sandbox_id = %sandbox_id.0, path = %request.path, "grpc read_file request");
        let file = self
            .runtime
            .read_file(&sandbox_id, &request.path)
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, path = %request.path, error = %e, "grpc read_file failed");
                Status::internal(e.to_string())
            })?;

        Ok(Response::new(pb::ReadFileResponse { bytes: file.bytes }))
    }

    async fn write_file(
        &self,
        request: Request<pb::WriteFileRequest>,
    ) -> Result<Response<pb::WriteFileResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        let write_path = request.path.clone();
        debug!(
            peer = ?peer,
            sandbox_id = %sandbox_id.0,
            path = %write_path,
            bytes = request.bytes.len(),
            "grpc write_file request"
        );
        self.runtime
            .write_file(
                &sandbox_id,
                FilePayload {
                    path: request.path.into(),
                    bytes: request.bytes,
                },
            )
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, path = %write_path, error = %e, "grpc write_file failed");
                Status::internal(e.to_string())
            })?;

        Ok(Response::new(pb::WriteFileResponse {}))
    }

    async fn list_templates(
        &self,
        _request: Request<pb::ListTemplatesRequest>,
    ) -> Result<Response<pb::ListTemplatesResponse>, Status> {
        debug!("grpc list_templates request");
        Ok(Response::new(pb::ListTemplatesResponse {
            templates: self.runtime.templates(),
        }))
    }

    async fn get_metrics(
        &self,
        _request: Request<pb::MetricsRequest>,
    ) -> Result<Response<pb::MetricsResponse>, Status> {
        debug!("grpc get_metrics request");
        let metrics = self.runtime.metrics().await;
        Ok(Response::new(metrics.into()))
    }

    async fn get_server_info(
        &self,
        _request: Request<pb::ServerInfoRequest>,
    ) -> Result<Response<pb::ServerInfoResponse>, Status> {
        debug!("grpc get_server_info request");
        Ok(Response::new(pb::ServerInfoResponse {
            server_version: self.server_info.server_version.clone(),
            process_id: self.server_info.process_id.to_string(),
            executable_path: self.server_info.executable_path.clone(),
            started_at: self.server_info.started_at.clone(),
            backend_requested: self.server_info.backend_requested.clone(),
            backend_selected: self.server_info.backend_selected.clone(),
            backend_reason: self.server_info.backend_reason.clone(),
            apple_runtime: self.server_info.apple_runtime.clone().unwrap_or_default(),
            apple_helper_argv: self.server_info.apple_helper_argv.clone(),
        }))
    }

    async fn create_snapshot(
        &self,
        request: Request<pb::CreateSnapshotRequest>,
    ) -> Result<Response<pb::CreateSnapshotResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        info!(peer = ?peer, sandbox_id = %sandbox_id.0, "grpc create_snapshot request");
        let snapshot = self
            .runtime
            .create_snapshot(
                &sandbox_id,
                if request.note.is_empty() {
                    None
                } else {
                    Some(request.note)
                },
            )
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc create_snapshot failed");
                Status::internal(e.to_string())
            })?;
        info!(peer = ?peer, sandbox_id = %sandbox_id.0, snapshot_id = %snapshot.id.0, "grpc create_snapshot success");

        Ok(Response::new(pb::CreateSnapshotResponse {
            snapshot_id: snapshot.id.0.to_string(),
            created_at: snapshot.created_at.to_rfc3339(),
        }))
    }

    async fn restore_snapshot(
        &self,
        request: Request<pb::RestoreSnapshotRequest>,
    ) -> Result<Response<pb::RestoreSnapshotResponse>, Status> {
        let peer = request.remote_addr();
        let snapshot_id = SnapshotId(
            uuid::Uuid::parse_str(&request.into_inner().snapshot_id)
                .map_err(|e| Status::invalid_argument(format!("invalid snapshot_id: {e}")))?,
        );
        info!(peer = ?peer, snapshot_id = %snapshot_id.0, "grpc restore_snapshot request");

        let info = self
            .runtime
            .restore_snapshot(&snapshot_id)
            .await
            .map_err(|e| {
                error!(peer = ?peer, snapshot_id = %snapshot_id.0, error = %e, "grpc restore_snapshot failed");
                Status::internal(e.to_string())
            })?;
        warn!(peer = ?peer, snapshot_id = %snapshot_id.0, sandbox_id = %info.id.0, "grpc restore_snapshot created new sandbox");

        Ok(Response::new(pb::RestoreSnapshotResponse {
            info: Some(into_proto_info(info)),
        }))
    }
}

pub async fn serve_grpc(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let requested = crate::BackendKind::from_env();
    let selection = crate::resolve_backend(requested);
    info!(
        %addr,
        requested_backend = selection.requested.as_str(),
        selected_backend = selection.selected.as_str(),
        reason = %selection.reason,
        "starting grpc control service"
    );
    let backend = selection.backend;
    let runtime = crate::HyperboxServer::new(backend);
    let executable_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let service = GrpcControlService::new(
        runtime,
        ServerRuntimeInfo {
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            process_id: std::process::id(),
            executable_path,
            started_at: chrono::Utc::now().to_rfc3339(),
            backend_requested: selection.requested.as_str().to_string(),
            backend_selected: selection.selected.as_str().to_string(),
            backend_reason: selection.reason,
            apple_runtime: selection.apple_runtime.map(|runtime| match runtime {
                hyperbox_apple::AppleRuntimeKind::Containerization => {
                    "containerization".to_string()
                }
                hyperbox_apple::AppleRuntimeKind::Virtualization => "virtualization".to_string(),
            }),
            apple_helper_argv: selection.apple_helper_command.unwrap_or_default(),
        },
    );

    tonic::transport::Server::builder()
        .add_service(pb::hyperbox_control_server::HyperboxControlServer::new(
            service,
        ))
        .serve(addr)
        .await?;

    Ok(())
}
