use tonic::{Request, Response, Status};

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
        let config = request
            .into_inner()
            .config
            .map(from_proto_config)
            .unwrap_or_default();

        let info = self
            .runtime
            .create_sandbox(config)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::CreateSandboxResponse {
            info: Some(into_proto_info(info)),
        }))
    }

    async fn destroy_sandbox(
        &self,
        request: Request<pb::DestroySandboxRequest>,
    ) -> Result<Response<pb::DestroySandboxResponse>, Status> {
        let sandbox_id = parse_sandbox_id(&request.into_inner().sandbox_id)?;
        self.runtime
            .destroy_sandbox(&sandbox_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(pb::DestroySandboxResponse {}))
    }

    async fn inspect_sandbox(
        &self,
        request: Request<pb::InspectSandboxRequest>,
    ) -> Result<Response<pb::InspectSandboxResponse>, Status> {
        let sandbox_id = parse_sandbox_id(&request.into_inner().sandbox_id)?;
        let info = self
            .runtime
            .inspect(&sandbox_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::InspectSandboxResponse {
            info: Some(into_proto_info(info)),
        }))
    }

    async fn exec(
        &self,
        request: Request<pb::ExecRequest>,
    ) -> Result<Response<pb::ExecResponse>, Status> {
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
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
            .map_err(|e| Status::internal(e.to_string()))?;

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
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        let file = self
            .runtime
            .read_file(&sandbox_id, &request.path)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::ReadFileResponse { bytes: file.bytes }))
    }

    async fn write_file(
        &self,
        request: Request<pb::WriteFileRequest>,
    ) -> Result<Response<pb::WriteFileResponse>, Status> {
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        self.runtime
            .write_file(
                &sandbox_id,
                FilePayload {
                    path: request.path.into(),
                    bytes: request.bytes,
                },
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::WriteFileResponse {}))
    }

    async fn list_templates(
        &self,
        _request: Request<pb::ListTemplatesRequest>,
    ) -> Result<Response<pb::ListTemplatesResponse>, Status> {
        Ok(Response::new(pb::ListTemplatesResponse {
            templates: self.runtime.templates(),
        }))
    }

    async fn get_metrics(
        &self,
        _request: Request<pb::MetricsRequest>,
    ) -> Result<Response<pb::MetricsResponse>, Status> {
        let metrics = self.runtime.metrics().await;
        Ok(Response::new(metrics.into()))
    }

    async fn create_snapshot(
        &self,
        request: Request<pb::CreateSnapshotRequest>,
    ) -> Result<Response<pb::CreateSnapshotResponse>, Status> {
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
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
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::CreateSnapshotResponse {
            snapshot_id: snapshot.id.0.to_string(),
            created_at: snapshot.created_at.to_rfc3339(),
        }))
    }

    async fn restore_snapshot(
        &self,
        request: Request<pb::RestoreSnapshotRequest>,
    ) -> Result<Response<pb::RestoreSnapshotResponse>, Status> {
        let snapshot_id = SnapshotId(
            uuid::Uuid::parse_str(&request.into_inner().snapshot_id)
                .map_err(|e| Status::invalid_argument(format!("invalid snapshot_id: {e}")))?,
        );

        let info = self
            .runtime
            .restore_snapshot(&snapshot_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::RestoreSnapshotResponse {
            info: Some(into_proto_info(info)),
        }))
    }
}

pub async fn serve_grpc(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let backend = crate::select_backend(crate::BackendKind::from_env());
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
