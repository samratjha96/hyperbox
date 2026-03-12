use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

use hyperbox_core::{
    ExecRequest, FilePayload, NetworkMode, ProcessDisposition, ProcessId, ProcessInfo,
    ProcessLogRead, SandboxConfig, SandboxId, SnapshotId, StreamName,
};
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

fn parse_process_id(raw: &str) -> Result<ProcessId, Status> {
    let id = uuid::Uuid::parse_str(raw)
        .map_err(|e| Status::invalid_argument(format!("invalid process_id: {e}")))?;
    Ok(ProcessId(id))
}

fn into_proto_info(info: hyperbox_core::SandboxInfo) -> pb::SandboxInfo {
    pb::SandboxInfo {
        id: info.id.0.to_string(),
        template: info.template,
        state: format!("{:?}", info.state),
        created_at: info.created_at.to_rfc3339(),
    }
}

fn into_proto_process(process: ProcessInfo) -> pb::ProcessInfo {
    pb::ProcessInfo {
        process_id: process.id.0.to_string(),
        sandbox_id: process.sandbox_id.0.to_string(),
        requested_sandbox_id: process
            .requested_sandbox_id
            .map(|id| id.0.to_string())
            .unwrap_or_default(),
        disposition: format!("{:?}", process.disposition),
        command: process.command,
        status: format!("{:?}", process.status),
        stdout_path: process.stdout_path,
        stderr_path: process.stderr_path,
        backend_pid: process.backend_pid.unwrap_or_default(),
        has_backend_pid: process.backend_pid.is_some(),
        exit_code: process.exit_code.unwrap_or_default(),
        has_exit_code: process.exit_code.is_some(),
        started_at: process.started_at.to_rfc3339(),
        finished_at: process
            .finished_at
            .map(|time| time.to_rfc3339())
            .unwrap_or_default(),
        expires_at: process
            .expires_at
            .map(|time| time.to_rfc3339())
            .unwrap_or_default(),
    }
}

fn parse_process_disposition(raw: &str) -> Result<ProcessDisposition, Status> {
    match raw {
        "" | "ReusedExisting" => Ok(ProcessDisposition::ReusedExisting),
        "CreatedNew" => Ok(ProcessDisposition::CreatedNew),
        "CreatedDueToBusy" => Ok(ProcessDisposition::CreatedDueToBusy),
        other => Err(Status::invalid_argument(format!(
            "invalid process disposition: {other}"
        ))),
    }
}

fn into_proto_prepared_run_sandbox(
    prepared: crate::runtime::PreparedRunSandbox,
) -> pb::PrepareRunSandboxResponse {
    pb::PrepareRunSandboxResponse {
        info: Some(into_proto_info(prepared.info)),
        requested_sandbox_id: prepared
            .requested_sandbox_id
            .map(|id| id.0.to_string())
            .unwrap_or_default(),
        disposition: format!("{:?}", prepared.disposition),
    }
}

fn parse_stream_name(raw: &str) -> Result<StreamName, Status> {
    match raw {
        "stdout" | "Stdout" => Ok(StreamName::Stdout),
        "stderr" | "Stderr" => Ok(StreamName::Stderr),
        other => Err(Status::invalid_argument(format!("invalid stream: {other}"))),
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
        affinity_name: if config.affinity_name.is_empty() {
            None
        } else {
            Some(config.affinity_name)
        },
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

fn into_proto_config(config: SandboxConfig) -> pb::SandboxConfig {
    let (network_mode, network_allowlist) = match config.network {
        NetworkMode::None => (pb::NetworkMode::None as i32, Vec::new()),
        NetworkMode::Allowlist(domains) => (pb::NetworkMode::Allowlist as i32, domains),
        NetworkMode::Full => (pb::NetworkMode::Full as i32, Vec::new()),
    };

    pb::SandboxConfig {
        template: config.template,
        memory_mb: config.memory_mb,
        vcpu_count: config.vcpu_count as u32,
        timeout_secs: config.timeout_secs,
        env: config.env.into_iter().collect(),
        network_mode,
        network_allowlist,
        workspace_dir: config.workspace_dir.unwrap_or_default(),
        affinity_name: config.affinity_name.unwrap_or_default(),
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
    async fn start_process(
        &self,
        request: Request<pb::StartProcessRequest>,
    ) -> Result<Response<pb::StartProcessResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        let requested_sandbox_id = if request.requested_sandbox_id.is_empty() {
            None
        } else {
            Some(parse_sandbox_id(&request.requested_sandbox_id)?)
        };
        let disposition = parse_process_disposition(&request.disposition)?;
        let process = self
            .runtime
            .start_process(
                &sandbox_id,
                request.command,
                requested_sandbox_id,
                disposition,
            )
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc start_process failed");
                Status::internal(e.to_string())
            })?;
        Ok(Response::new(pb::StartProcessResponse {
            process: Some(into_proto_process(process)),
        }))
    }

    async fn prepare_run_sandbox(
        &self,
        request: Request<pb::PrepareRunSandboxRequest>,
    ) -> Result<Response<pb::PrepareRunSandboxResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = parse_sandbox_id(&request.sandbox_id)?;
        let prepared = self
            .runtime
            .prepare_run_sandbox(&sandbox_id, request.overflow_config.map(from_proto_config))
            .await
            .map_err(|e| {
                error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc prepare_run_sandbox failed");
                Status::internal(e.to_string())
            })?;
        Ok(Response::new(into_proto_prepared_run_sandbox(prepared)))
    }

    async fn get_process(
        &self,
        request: Request<pb::GetProcessRequest>,
    ) -> Result<Response<pb::GetProcessResponse>, Status> {
        let peer = request.remote_addr();
        let process_id = parse_process_id(&request.into_inner().process_id)?;
        let process = self.runtime.get_process(&process_id).await.map_err(|e| {
            error!(peer = ?peer, process_id = %process_id.0, error = %e, "grpc get_process failed");
            Status::internal(e.to_string())
        })?;
        Ok(Response::new(pb::GetProcessResponse {
            process: Some(into_proto_process(process)),
        }))
    }

    async fn list_processes(
        &self,
        _request: Request<pb::ListProcessesRequest>,
    ) -> Result<Response<pb::ListProcessesResponse>, Status> {
        let processes = self.runtime.list_processes().await.map_err(|e| {
            error!(error = %e, "grpc list_processes failed");
            Status::internal(e.to_string())
        })?;
        Ok(Response::new(pb::ListProcessesResponse {
            processes: processes.into_iter().map(into_proto_process).collect(),
        }))
    }

    async fn read_process_log(
        &self,
        request: Request<pb::ReadProcessLogRequest>,
    ) -> Result<Response<pb::ReadProcessLogResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let process_id = parse_process_id(&request.process_id)?;
        let stream = parse_stream_name(&request.stream)?;
        let log = self
            .runtime
            .read_process_log(&process_id, stream, request.offset, request.limit)
            .await
            .map_err(|e| {
                error!(peer = ?peer, process_id = %process_id.0, error = %e, "grpc read_process_log failed");
                Status::internal(e.to_string())
            })?;
        Ok(Response::new(into_proto_process_log(log)))
    }

    async fn wait_process(
        &self,
        request: Request<pb::WaitProcessRequest>,
    ) -> Result<Response<pb::WaitProcessResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let process_id = parse_process_id(&request.process_id)?;
        let process = self
            .runtime
            .wait_process(&process_id, request.timeout_secs.max(1))
            .await
            .map_err(|e| {
                error!(peer = ?peer, process_id = %process_id.0, error = %e, "grpc wait_process failed");
                Status::internal(e.to_string())
            })?;
        Ok(Response::new(pb::WaitProcessResponse {
            process: Some(into_proto_process(process)),
        }))
    }

    async fn cancel_process(
        &self,
        request: Request<pb::CancelProcessRequest>,
    ) -> Result<Response<pb::CancelProcessResponse>, Status> {
        let peer = request.remote_addr();
        let process_id = parse_process_id(&request.into_inner().process_id)?;
        let process = self
            .runtime
            .cancel_process(&process_id)
            .await
            .map_err(|e| {
                error!(peer = ?peer, process_id = %process_id.0, error = %e, "grpc cancel_process failed");
                Status::internal(e.to_string())
            })?;
        Ok(Response::new(pb::CancelProcessResponse {
            process: Some(into_proto_process(process)),
        }))
    }

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
            config: Some(into_proto_config(
                self.runtime.sandbox_config(&sandbox_id).await.map_err(|e| {
                    error!(peer = ?peer, sandbox_id = %sandbox_id.0, error = %e, "grpc inspect_sandbox config lookup failed");
                    Status::internal(e.to_string())
                })?,
            )),
        }))
    }

    async fn list_sandboxes(
        &self,
        _request: Request<pb::ListSandboxesRequest>,
    ) -> Result<Response<pb::ListSandboxesResponse>, Status> {
        debug!("grpc list_sandboxes request");
        let sandboxes = self
            .runtime
            .list_sandboxes()
            .await
            .into_iter()
            .map(|row| pb::ActiveSandboxInfo {
                info: Some(into_proto_info(row.info)),
                affinity_name: row.affinity_name.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(pb::ListSandboxesResponse { sandboxes }))
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

    async fn list_snapshots(
        &self,
        request: Request<pb::ListSnapshotsRequest>,
    ) -> Result<Response<pb::ListSnapshotsResponse>, Status> {
        let peer = request.remote_addr();
        let template = request.into_inner().template;
        let template = if template.is_empty() {
            SandboxConfig::default().template
        } else {
            template
        };
        debug!(peer = ?peer, template = %template, "grpc list_snapshots request");
        let snapshots = self.runtime.list_snapshots(&template).await.map_err(|e| {
            error!(peer = ?peer, template = %template, error = %e, "grpc list_snapshots failed");
            Status::internal(e.to_string())
        })?;

        let snapshots = snapshots
            .into_iter()
            .map(|s| pb::SnapshotInfo {
                snapshot_id: s.id.0.to_string(),
                sandbox_id: s.sandbox_id.0.to_string(),
                template: s.template,
                created_at: s.created_at.to_rfc3339(),
                note: s.note.unwrap_or_default(),
                affinity_name: s.affinity_name.unwrap_or_default(),
            })
            .collect();

        Ok(Response::new(pb::ListSnapshotsResponse { snapshots }))
    }

    async fn resolve_affinity(
        &self,
        request: Request<pb::ResolveAffinityRequest>,
    ) -> Result<Response<pb::ResolveAffinityResponse>, Status> {
        let peer = request.remote_addr();
        let req = request.into_inner();
        if req.name.trim().is_empty() {
            return Err(Status::invalid_argument("name must not be empty"));
        }
        info!(
            peer = ?peer,
            name = %req.name,
            restore_if_needed = req.restore_if_needed,
            "grpc resolve_affinity request"
        );
        let (info, restored) = self
            .runtime
            .resolve_affinity(&req.name, req.restore_if_needed)
            .await
            .map_err(|e| {
                error!(peer = ?peer, name = %req.name, error = %e, "grpc resolve_affinity failed");
                Status::internal(e.to_string())
            })?;
        Ok(Response::new(pb::ResolveAffinityResponse {
            info: Some(into_proto_info(info)),
            restored,
        }))
    }
}

fn into_proto_process_log(log: ProcessLogRead) -> pb::ReadProcessLogResponse {
    pb::ReadProcessLogResponse {
        stream: match log.stream {
            StreamName::Stdout => "stdout".to_string(),
            StreamName::Stderr => "stderr".to_string(),
        },
        offset: log.offset,
        next_offset: log.next_offset,
        eof: log.eof,
        contents: log.contents,
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
