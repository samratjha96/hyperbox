use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

use hyperbox_core::{ExecRequest, FilePayload, NetworkMode, SandboxConfig, SandboxId, SnapshotId};
use hyperbox_proto::hyperbox::v1::{self as pb, hyperbox_control_server::HyperboxControl};

use crate::HyperboxServer;

#[derive(Clone)]
pub struct GrpcControlService {
    runtime: HyperboxServer,
}

impl GrpcControlService {
    pub fn new(runtime: HyperboxServer) -> Self {
        Self { runtime }
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
    let backend_kind = crate::BackendKind::from_env();
    info!(%addr, backend_kind = ?backend_kind, "starting grpc control service");
    let backend = crate::select_backend(backend_kind);
    let runtime = crate::HyperboxServer::new(backend);
    let service = GrpcControlService::new(runtime);

    tonic::transport::Server::builder()
        .add_service(pb::hyperbox_control_server::HyperboxControlServer::new(
            service,
        ))
        .serve(addr)
        .await?;

    Ok(())
}
