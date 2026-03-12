use hyperbox_core::{
    NetworkMode, ProcessDisposition, ProcessId, ProcessInfo, ProcessLogRead, ProcessStatus,
    SandboxConfig, SandboxInfo, SnapshotId, SnapshotMetadata, StreamName,
};
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

#[derive(Debug, Clone)]
pub struct ActiveSandboxInfo {
    pub info: SandboxInfo,
    pub affinity_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SandboxDetails {
    pub info: SandboxInfo,
    pub config: SandboxConfig,
}

#[derive(Debug, Clone)]
pub struct PreparedRunSandbox {
    pub info: SandboxInfo,
    pub requested_sandbox_id: Option<hyperbox_core::SandboxId>,
    pub disposition: ProcessDisposition,
}

#[derive(Debug, Clone)]
pub struct StartedRun {
    pub process: ProcessInfo,
    pub sandbox: SandboxInfo,
    pub session_name: Option<String>,
    pub session_created: bool,
}

fn parse_sandbox_info(info: pb::SandboxInfo) -> anyhow::Result<SandboxInfo> {
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

fn parse_process_info(info: pb::ProcessInfo) -> anyhow::Result<ProcessInfo> {
    Ok(ProcessInfo {
        id: ProcessId(uuid::Uuid::parse_str(&info.process_id)?),
        sandbox_id: hyperbox_core::SandboxId(uuid::Uuid::parse_str(&info.sandbox_id)?),
        requested_sandbox_id: if info.requested_sandbox_id.is_empty() {
            None
        } else {
            Some(hyperbox_core::SandboxId(uuid::Uuid::parse_str(
                &info.requested_sandbox_id,
            )?))
        },
        disposition: match info.disposition.as_str() {
            "CreatedNew" => ProcessDisposition::CreatedNew,
            "CreatedDueToBusy" => ProcessDisposition::CreatedDueToBusy,
            _ => ProcessDisposition::ReusedExisting,
        },
        destroy_sandbox_on_expiry: info.destroy_sandbox_on_expiry,
        command: info.command,
        status: match info.status.as_str() {
            "Starting" => ProcessStatus::Starting,
            "Running" => ProcessStatus::Running,
            "Succeeded" => ProcessStatus::Succeeded,
            "Cancelled" => ProcessStatus::Cancelled,
            "Lost" => ProcessStatus::Lost,
            _ => ProcessStatus::Failed,
        },
        stdout_path: info.stdout_path,
        stderr_path: info.stderr_path,
        backend_pid: info.has_backend_pid.then_some(info.backend_pid),
        exit_code: info.has_exit_code.then_some(info.exit_code),
        started_at: chrono::DateTime::parse_from_rfc3339(&info.started_at)?
            .with_timezone(&chrono::Utc),
        finished_at: if info.finished_at.is_empty() {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(&info.finished_at)?
                    .with_timezone(&chrono::Utc),
            )
        },
        expires_at: if info.expires_at.is_empty() {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(&info.expires_at)?.with_timezone(&chrono::Utc),
            )
        },
    })
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
                affinity_name: config.affinity_name.unwrap_or_default(),
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

    pub async fn start_process(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
        command: Vec<String>,
        requested_sandbox_id: Option<hyperbox_core::SandboxId>,
        disposition: ProcessDisposition,
        destroy_sandbox_on_expiry: bool,
    ) -> anyhow::Result<ProcessInfo> {
        let response = self
            .inner
            .start_process(pb::StartProcessRequest {
                sandbox_id: sandbox_id.0.to_string(),
                command,
                requested_sandbox_id: requested_sandbox_id
                    .map(|id| id.0.to_string())
                    .unwrap_or_default(),
                disposition: format!("{:?}", disposition),
                destroy_sandbox_on_expiry,
            })
            .await?
            .into_inner();
        parse_process_info(
            response
                .process
                .ok_or_else(|| anyhow::anyhow!("missing process info"))?,
        )
    }

    pub async fn start_run(
        &mut self,
        sandbox_id: Option<hyperbox_core::SandboxId>,
        affinity_name: Option<String>,
        create_config: Option<SandboxConfig>,
        reuse_auto_session: bool,
        ensure_commands: Vec<String>,
        writes: Vec<(String, Vec<u8>)>,
        command: String,
        destroy_sandbox_on_expiry: bool,
    ) -> anyhow::Result<StartedRun> {
        let response = self
            .inner
            .start_run(pb::StartRunRequest {
                sandbox_id: sandbox_id.map(|id| id.0.to_string()).unwrap_or_default(),
                affinity_name: affinity_name.unwrap_or_default(),
                create_config: create_config.map(into_proto_config),
                reuse_auto_session,
                ensure_commands,
                writes: writes
                    .into_iter()
                    .map(|(path, bytes)| pb::RunFileWrite { path, bytes })
                    .collect(),
                command,
                destroy_sandbox_on_expiry,
            })
            .await?
            .into_inner();
        Ok(StartedRun {
            process: parse_process_info(
                response
                    .process
                    .ok_or_else(|| anyhow::anyhow!("missing process info"))?,
            )?,
            sandbox: parse_sandbox_info(
                response
                    .sandbox
                    .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?,
            )?,
            session_name: if response.session_name.is_empty() {
                None
            } else {
                Some(response.session_name)
            },
            session_created: response.session_created,
        })
    }

    pub async fn prepare_run_sandbox(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
        overflow_config: Option<SandboxConfig>,
    ) -> anyhow::Result<PreparedRunSandbox> {
        let response = self
            .inner
            .prepare_run_sandbox(pb::PrepareRunSandboxRequest {
                sandbox_id: sandbox_id.0.to_string(),
                overflow_config: overflow_config.map(into_proto_config),
            })
            .await?
            .into_inner();
        Ok(PreparedRunSandbox {
            info: parse_sandbox_info(
                response
                    .info
                    .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?,
            )?,
            requested_sandbox_id: if response.requested_sandbox_id.is_empty() {
                None
            } else {
                Some(hyperbox_core::SandboxId(uuid::Uuid::parse_str(
                    &response.requested_sandbox_id,
                )?))
            },
            disposition: match response.disposition.as_str() {
                "CreatedDueToBusy" => ProcessDisposition::CreatedDueToBusy,
                "CreatedNew" => ProcessDisposition::CreatedNew,
                _ => ProcessDisposition::ReusedExisting,
            },
        })
    }

    pub async fn get_process(&mut self, process_id: &ProcessId) -> anyhow::Result<ProcessInfo> {
        let response = self
            .inner
            .get_process(pb::GetProcessRequest {
                process_id: process_id.0.to_string(),
            })
            .await?
            .into_inner();
        parse_process_info(
            response
                .process
                .ok_or_else(|| anyhow::anyhow!("missing process info"))?,
        )
    }

    pub async fn list_processes(&mut self) -> anyhow::Result<Vec<ProcessInfo>> {
        let response = self
            .inner
            .list_processes(pb::ListProcessesRequest {})
            .await?
            .into_inner();
        response
            .processes
            .into_iter()
            .map(parse_process_info)
            .collect()
    }

    pub async fn read_process_log(
        &mut self,
        process_id: &ProcessId,
        stream: StreamName,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<ProcessLogRead> {
        let response = self
            .inner
            .read_process_log(pb::ReadProcessLogRequest {
                process_id: process_id.0.to_string(),
                stream: match stream {
                    StreamName::Stdout => "stdout".to_string(),
                    StreamName::Stderr => "stderr".to_string(),
                },
                offset,
                limit,
            })
            .await?
            .into_inner();
        Ok(ProcessLogRead {
            stream: match response.stream.as_str() {
                "stderr" => StreamName::Stderr,
                _ => StreamName::Stdout,
            },
            offset: response.offset,
            next_offset: response.next_offset,
            eof: response.eof,
            contents: response.contents,
        })
    }

    pub async fn wait_process(
        &mut self,
        process_id: &ProcessId,
        timeout_secs: u64,
    ) -> anyhow::Result<ProcessInfo> {
        let response = self
            .inner
            .wait_process(pb::WaitProcessRequest {
                process_id: process_id.0.to_string(),
                timeout_secs,
            })
            .await?
            .into_inner();
        parse_process_info(
            response
                .process
                .ok_or_else(|| anyhow::anyhow!("missing process info"))?,
        )
    }

    pub async fn cancel_process(&mut self, process_id: &ProcessId) -> anyhow::Result<ProcessInfo> {
        let response = self
            .inner
            .cancel_process(pb::CancelProcessRequest {
                process_id: process_id.0.to_string(),
            })
            .await?
            .into_inner();
        parse_process_info(
            response
                .process
                .ok_or_else(|| anyhow::anyhow!("missing process info"))?,
        )
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
        Ok(self.inspect_sandbox_details(sandbox_id).await?.info)
    }

    pub async fn inspect_sandbox_details(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
    ) -> anyhow::Result<SandboxDetails> {
        let response = self
            .inner
            .inspect_sandbox(pb::InspectSandboxRequest {
                sandbox_id: sandbox_id.0.to_string(),
            })
            .await?
            .into_inner();
        let info = response
            .info
            .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?;
        let config = response
            .config
            .ok_or_else(|| anyhow::anyhow!("missing sandbox config"))?;

        Ok(SandboxDetails {
            info: parse_sandbox_info(info)?,
            config: SandboxConfig {
                affinity_name: if config.affinity_name.is_empty() {
                    None
                } else {
                    Some(config.affinity_name)
                },
                template: config.template,
                memory_mb: config.memory_mb,
                vcpu_count: config.vcpu_count as u8,
                workspace_dir: if config.workspace_dir.is_empty() {
                    None
                } else {
                    Some(config.workspace_dir)
                },
                network: match pb::NetworkMode::try_from(config.network_mode)
                    .unwrap_or(pb::NetworkMode::None)
                {
                    pb::NetworkMode::Allowlist => NetworkMode::Allowlist(config.network_allowlist),
                    pb::NetworkMode::Full => NetworkMode::Full,
                    _ => NetworkMode::None,
                },
                env: config.env.into_iter().collect(),
                timeout_secs: config.timeout_secs,
            },
        })
    }

    pub async fn list_sandboxes(&mut self) -> anyhow::Result<Vec<ActiveSandboxInfo>> {
        let response = self
            .inner
            .list_sandboxes(pb::ListSandboxesRequest {})
            .await?
            .into_inner();
        response
            .sandboxes
            .into_iter()
            .map(|row| {
                let info = row
                    .info
                    .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?;
                Ok(ActiveSandboxInfo {
                    info: parse_sandbox_info(info)?,
                    affinity_name: if row.affinity_name.is_empty() {
                        None
                    } else {
                        Some(row.affinity_name)
                    },
                })
            })
            .collect()
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

    pub async fn create_snapshot(
        &mut self,
        sandbox_id: &hyperbox_core::SandboxId,
        note: Option<String>,
    ) -> anyhow::Result<(SnapshotId, String)> {
        let response = self
            .inner
            .create_snapshot(pb::CreateSnapshotRequest {
                sandbox_id: sandbox_id.0.to_string(),
                template: "".to_string(),
                note: note.unwrap_or_default(),
            })
            .await?
            .into_inner();
        Ok((
            SnapshotId(uuid::Uuid::parse_str(&response.snapshot_id)?),
            response.created_at,
        ))
    }

    pub async fn restore_snapshot(
        &mut self,
        snapshot_id: &SnapshotId,
    ) -> anyhow::Result<SandboxInfo> {
        let info = self
            .inner
            .restore_snapshot(pb::RestoreSnapshotRequest {
                snapshot_id: snapshot_id.0.to_string(),
            })
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

    pub async fn list_snapshots(
        &mut self,
        template: &str,
    ) -> anyhow::Result<Vec<SnapshotMetadata>> {
        let response = self
            .inner
            .list_snapshots(pb::ListSnapshotsRequest {
                template: template.to_string(),
            })
            .await?
            .into_inner();
        response
            .snapshots
            .into_iter()
            .map(|s| {
                Ok(SnapshotMetadata {
                    id: SnapshotId(uuid::Uuid::parse_str(&s.snapshot_id)?),
                    sandbox_id: hyperbox_core::SandboxId(uuid::Uuid::parse_str(&s.sandbox_id)?),
                    affinity_name: if s.affinity_name.is_empty() {
                        None
                    } else {
                        Some(s.affinity_name)
                    },
                    template: s.template.clone(),
                    config: SandboxConfig {
                        template: s.template,
                        ..SandboxConfig::default()
                    },
                    created_at: chrono::DateTime::parse_from_rfc3339(&s.created_at)?
                        .with_timezone(&chrono::Utc),
                    note: if s.note.is_empty() {
                        None
                    } else {
                        Some(s.note)
                    },
                })
            })
            .collect()
    }

    pub async fn resolve_affinity(
        &mut self,
        name: &str,
        restore_if_needed: bool,
    ) -> anyhow::Result<(SandboxInfo, bool)> {
        let response = self
            .inner
            .resolve_affinity(pb::ResolveAffinityRequest {
                name: name.to_string(),
                restore_if_needed,
            })
            .await?
            .into_inner();
        let info = response
            .info
            .ok_or_else(|| anyhow::anyhow!("missing sandbox info"))?;
        Ok((
            SandboxInfo {
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
            },
            response.restored,
        ))
    }
}
