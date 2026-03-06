use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::{Duration, timeout},
};
use tracing::{debug, info, warn};

use hyperbox_core::{
    FilePayload, HyperboxError, NetworkMode, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SandboxLease, SandboxState, SnapshotId,
};
use hyperbox_proto::hyperbox::v1::hyperbox_agent_client::HyperboxAgentClient;

use crate::detect_macos_capabilities;

const WORKDIR_IN_CONTAINER: &str = "/workspace";
const DEFAULT_IO_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DESTROY_TIMEOUT_SECS: u64 = 30;
const SNAPSHOT_ARCHIVE_DIR_REL: &str = ".hyperbox/snapshots";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleRuntimeKind {
    Containerization,
    Virtualization,
}

#[derive(Debug, Clone)]
pub struct AppleBackendConfig {
    pub work_dir: PathBuf,
    pub agent_endpoint: String,
    pub launch_command: Option<Vec<String>>,
    pub runtime_kind: AppleRuntimeKind,
}

impl Default for AppleBackendConfig {
    fn default() -> Self {
        Self {
            work_dir: std::env::temp_dir().join("hyperbox-apple"),
            agent_endpoint: "http://127.0.0.1:60061".to_string(),
            launch_command: None,
            runtime_kind: AppleRuntimeKind::Virtualization,
        }
    }
}

struct AppleHelperSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _stderr: Option<ChildStderr>,
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HelperRequest {
    Create {
        sandbox_id: String,
        template: String,
        workspace_dir: Option<String>,
        runtime: String,
        network_mode: String,
        network_allowlist: Vec<String>,
        memory_mb: Option<u32>,
        vcpu_count: Option<u32>,
    },
    Exec {
        sandbox_id: String,
        command: Vec<String>,
        timeout_secs: u64,
    },
    Read {
        sandbox_id: String,
        path: String,
    },
    Write {
        sandbox_id: String,
        path: String,
        bytes_b64: String,
    },
    Destroy {
        sandbox_id: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HelperResponse {
    Ack,
    Exec {
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    Read {
        bytes_b64: String,
    },
    Error {
        message: String,
    },
}

impl AppleHelperSession {
    async fn request(&mut self, req: &HelperRequest) -> Result<HelperResponse> {
        let encoded = serde_json::to_string(req)?;
        self.stdin.write_all(encoded.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).await?;
        if read == 0 {
            return Err(HyperboxError::ExecutionFailed(
                "apple helper closed stdout unexpectedly".to_string(),
            ));
        }
        serde_json::from_str(line.trim_end()).map_err(HyperboxError::Serde)
    }

    async fn terminate(&mut self) -> Result<()> {
        self.child
            .kill()
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("kill apple helper: {e}")))?;
        Ok(())
    }
}

struct AppleSandbox {
    info: SandboxInfo,
    config: SandboxConfig,
    direct_container: Option<DirectContainerSandbox>,
}

#[derive(Debug, Clone)]
struct DirectContainerSandbox {
    container_name: String,
    workspace_host: PathBuf,
    ephemeral_workspace: bool,
}

#[derive(Debug)]
struct CommandResult {
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    duration_ms: u64,
}

#[derive(Clone)]
pub struct AppleVzBackend {
    config: AppleBackendConfig,
    sandboxes: Arc<Mutex<HashMap<SandboxId, AppleSandbox>>>,
    helper_session: Arc<Mutex<Option<AppleHelperSession>>>,
    direct_container_bin: Option<String>,
}

impl AppleVzBackend {
    pub fn new(config: AppleBackendConfig) -> Self {
        let direct_container_bin = resolve_direct_container_bin(config.launch_command.as_ref());
        Self {
            config,
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
            helper_session: Arc::new(Mutex::new(None)),
            direct_container_bin,
        }
    }

    fn runtime_name(&self) -> &'static str {
        match self.config.runtime_kind {
            AppleRuntimeKind::Containerization => "containerization",
            AppleRuntimeKind::Virtualization => "virtualization",
        }
    }

    fn use_direct_container_mode(&self) -> bool {
        self.direct_container_bin.is_some()
    }

    async fn ensure_helper_session(&self) -> Result<()> {
        let Some(command) = &self.config.launch_command else {
            return Ok(());
        };
        let mut session = self.helper_session.lock().await;
        if session.is_some() {
            return Ok(());
        }
        if command.is_empty() {
            return Err(HyperboxError::InvalidConfig(
                "apple launch command is empty".to_string(),
            ));
        }

        let spawn_started = std::time::Instant::now();
        let mut cmd = tokio::process::Command::new(&command[0]);
        cmd.args(&command[1..]);
        cmd.current_dir(&self.config.work_dir);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| HyperboxError::ExecutionFailed(format!("spawn apple helper: {e}")))?;

        let stdin = child.stdin.take().ok_or_else(|| {
            HyperboxError::ExecutionFailed("apple helper missing stdin pipe".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HyperboxError::ExecutionFailed("apple helper missing stdout pipe".to_string())
        })?;
        let stderr = child.stderr.take();

        *session = Some(AppleHelperSession {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            _stderr: stderr,
        });
        info!(
            stage = "helper_spawn",
            elapsed_ms = spawn_started.elapsed().as_millis() as u64,
            "apple helper process spawned"
        );
        Ok(())
    }

    async fn helper_request(&self, request: &HelperRequest) -> Result<HelperResponse> {
        self.ensure_helper_session().await?;
        let mut session = self.helper_session.lock().await;
        let helper = session.as_mut().ok_or_else(|| {
            HyperboxError::InvalidConfig("apple helper session is not available".to_string())
        })?;
        match helper.request(request).await {
            Ok(response) => Ok(response),
            Err(err) => {
                warn!(error = %err, "apple helper request failed, resetting helper session");
                if let Some(mut helper) = session.take() {
                    let _ = helper.terminate().await;
                }
                Err(err)
            }
        }
    }

    async fn create_direct_container(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
    ) -> Result<DirectContainerSandbox> {
        let container_bin = self.direct_container_bin.as_deref().ok_or_else(|| {
            HyperboxError::InvalidConfig("direct container mode not enabled".to_string())
        })?;
        let ephemeral_workspace = config.workspace_dir.is_none();
        let workspace_host = if let Some(dir) = &config.workspace_dir {
            let path = PathBuf::from(dir);
            let canonical = tokio::fs::canonicalize(&path).await.map_err(|e| {
                HyperboxError::ExecutionFailed(format!(
                    "workspace_dir does not exist or is not accessible `{}`: {e}",
                    path.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(HyperboxError::ExecutionFailed(format!(
                    "workspace_dir must be a directory: {}",
                    canonical.display()
                )));
            }
            canonical
        } else {
            let workspace = self
                .config
                .work_dir
                .join(sandbox_id.0.to_string())
                .join("workspace");
            tokio::fs::create_dir_all(&workspace).await.map_err(|e| {
                HyperboxError::ExecutionFailed(format!(
                    "create workspace directory `{}`: {e}",
                    workspace.display()
                ))
            })?;
            workspace
        };

        let container_name = format!("hyperbox-{}", sandbox_id.0);
        let cpus = config.vcpu_count.max(1);
        let memory_mb = config.memory_mb.max(1);
        let mount = format!("{}:{WORKDIR_IN_CONTAINER}", workspace_host.display());
        let network_args = container_network_args(&config.network)?;
        let args = vec![
            "run".to_string(),
            "--detach".to_string(),
            "--progress".to_string(),
            "none".to_string(),
            "--name".to_string(),
            container_name.clone(),
            "--cpus".to_string(),
            cpus.to_string(),
            "--memory".to_string(),
            format!("{memory_mb}M"),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            "--volume".to_string(),
            mount,
        ];
        let mut args = args;
        args.extend(network_args);
        args.extend([
            config.template.clone(),
            "sleep".to_string(),
            "infinity".to_string(),
        ]);
        let result =
            run_container_command(container_bin, args, None, DEFAULT_IO_TIMEOUT_SECS).await?;
        if result.exit_code != 0 {
            return Err(HyperboxError::ExecutionFailed(format!(
                "container run failed (exit={}): {}",
                result.exit_code,
                stderr_summary(&result.stderr)
            )));
        }

        Ok(DirectContainerSandbox {
            container_name,
            workspace_host,
            ephemeral_workspace,
        })
    }

    async fn exec_direct(
        &self,
        direct: &DirectContainerSandbox,
        req: hyperbox_core::ExecRequest,
    ) -> Result<hyperbox_core::ExecOutcome> {
        let container_bin = self.direct_container_bin.as_deref().ok_or_else(|| {
            HyperboxError::InvalidConfig("direct container mode not enabled".to_string())
        })?;
        let mut args = vec![
            "exec".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            direct.container_name.clone(),
        ];
        args.extend(req.command);
        let result =
            run_container_command(container_bin, args, None, req.timeout_secs.max(1)).await?;
        Ok(hyperbox_core::ExecOutcome {
            exit_code: result.exit_code,
            stdout: String::from_utf8_lossy(&result.stdout).to_string(),
            stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            duration_ms: result.duration_ms as u128,
        })
    }

    async fn read_file_direct(
        &self,
        direct: &DirectContainerSandbox,
        path: &str,
        timeout_secs: u64,
    ) -> Result<FilePayload> {
        let container_bin = self.direct_container_bin.as_deref().ok_or_else(|| {
            HyperboxError::InvalidConfig("direct container mode not enabled".to_string())
        })?;
        let relative = normalize_relative_path(path)?;
        let container_path = path_in_container(&relative);
        let args = vec![
            "exec".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            direct.container_name.clone(),
            "/usr/bin/env".to_string(),
            "cat".to_string(),
            container_path,
        ];
        let result = run_container_command(container_bin, args, None, timeout_secs.max(1)).await?;
        if result.exit_code != 0 {
            return Err(HyperboxError::ExecutionFailed(format!(
                "read failed (exit={}): {}",
                result.exit_code,
                stderr_summary(&result.stderr)
            )));
        }
        Ok(FilePayload {
            path: path.to_string().into(),
            bytes: result.stdout,
        })
    }

    async fn write_file_direct(
        &self,
        direct: &DirectContainerSandbox,
        payload: FilePayload,
        timeout_secs: u64,
    ) -> Result<()> {
        let container_bin = self.direct_container_bin.as_deref().ok_or_else(|| {
            HyperboxError::InvalidConfig("direct container mode not enabled".to_string())
        })?;
        let relative = normalize_relative_path(payload.path.as_str())?;
        let container_path = path_in_container(&relative);
        let payload_len = payload.bytes.len();
        debug!(
            container = %direct.container_name,
            path = %payload.path,
            bytes = payload_len,
            timeout_secs,
            "apple backend direct file write"
        );

        if let Some(parent) = relative.parent() {
            if !parent.as_os_str().is_empty() {
                let parent_in_container = path_in_container(parent);
                let mkdir_args = vec![
                    "exec".to_string(),
                    "--workdir".to_string(),
                    WORKDIR_IN_CONTAINER.to_string(),
                    direct.container_name.clone(),
                    "/usr/bin/env".to_string(),
                    "mkdir".to_string(),
                    "-p".to_string(),
                    parent_in_container,
                ];
                let mkdir_result =
                    run_container_command(container_bin, mkdir_args, None, timeout_secs.max(1))
                        .await?;
                if mkdir_result.exit_code != 0 {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "create parent directory failed (exit={}): {}",
                        mkdir_result.exit_code,
                        stderr_summary(&mkdir_result.stderr)
                    )));
                }
            }
        }

        let write_args = vec![
            "exec".to_string(),
            "--interactive".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            direct.container_name.clone(),
            "/usr/bin/env".to_string(),
            "tee".to_string(),
            container_path,
        ];
        let write_result = run_container_command(
            container_bin,
            write_args,
            Some(payload.bytes),
            timeout_secs.max(1),
        )
        .await?;
        if write_result.exit_code != 0 {
            return Err(HyperboxError::ExecutionFailed(format!(
                "write failed (exit={}): {}",
                write_result.exit_code,
                stderr_summary(&write_result.stderr)
            )));
        }
        Ok(())
    }

    async fn destroy_direct_container(&self, direct: DirectContainerSandbox) -> Result<()> {
        let container_bin = self.direct_container_bin.as_deref().ok_or_else(|| {
            HyperboxError::InvalidConfig("direct container mode not enabled".to_string())
        })?;
        let args = vec![
            "delete".to_string(),
            "--force".to_string(),
            direct.container_name.clone(),
        ];
        let result =
            run_container_command(container_bin, args, None, DEFAULT_DESTROY_TIMEOUT_SECS).await?;
        if result.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&result.stderr).to_lowercase();
            if !stderr.contains("not found") {
                return Err(HyperboxError::ExecutionFailed(format!(
                    "container delete failed (exit={}): {}",
                    result.exit_code,
                    stderr_summary(&result.stderr)
                )));
            }
        }

        if direct.ephemeral_workspace {
            tokio::fs::remove_dir_all(&direct.workspace_host)
                .await
                .map_err(|e| {
                    HyperboxError::ExecutionFailed(format!(
                        "remove ephemeral workspace `{}`: {e}",
                        direct.workspace_host.display()
                    ))
                })?;
            if let Some(sandbox_root) = direct.workspace_host.parent() {
                let _ = tokio::fs::remove_dir(sandbox_root).await;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl SandboxBackend for AppleVzBackend {
    async fn create(&self, config: SandboxConfig) -> Result<SandboxLease> {
        let create_started = std::time::Instant::now();
        ensure_supported_apple_network_mode(&config.network)?;
        if self.config.launch_command.is_none() {
            return Err(HyperboxError::InvalidConfig(
                "apple backend requires HYPERBOX_APPLE_HELPER to be configured".to_string(),
            ));
        }
        if matches!(self.config.runtime_kind, AppleRuntimeKind::Containerization)
            && !detect_macos_capabilities().supports_containerization_framework
        {
            let helper_is_builtin = self
                .config
                .launch_command
                .as_ref()
                .is_some_and(|cmd| cmd.len() >= 2 && cmd[1] == "apple-helper");
            let message = if helper_is_builtin {
                "apple built-in helper currently supports only containerization runtime, but this host does not support Apple Containerization framework; set HYPERBOX_BACKEND=local or configure an external virtualization-capable helper via HYPERBOX_APPLE_HELPER".to_string()
            } else {
                "apple containerization runtime requested but not available on this host"
                    .to_string()
            };
            return Err(HyperboxError::InvalidConfig(message));
        }

        tokio::fs::create_dir_all(&self.config.work_dir).await?;

        let id = SandboxId::new();
        let info = SandboxInfo {
            id: id.clone(),
            template: config.template.clone(),
            state: SandboxState::Ready,
            created_at: Utc::now(),
        };

        let direct_container = if self.use_direct_container_mode() {
            let direct_create_started = Instant::now();
            let direct = self.create_direct_container(&id, &config).await?;
            debug!(
                sandbox_id = %id.0,
                stage = "direct_create",
                elapsed_ms = direct_create_started.elapsed().as_millis() as u64,
                "apple direct container create finished"
            );
            Some(direct)
        } else if self.config.launch_command.is_some() {
            let helper_create_started = std::time::Instant::now();
            let (network_mode, network_allowlist) = helper_network_fields(&config.network);
            let response = self
                .helper_request(&HelperRequest::Create {
                    sandbox_id: id.0.to_string(),
                    template: config.template.clone(),
                    workspace_dir: config.workspace_dir.clone(),
                    runtime: self.runtime_name().to_string(),
                    network_mode,
                    network_allowlist,
                    memory_mb: Some(config.memory_mb),
                    vcpu_count: Some(config.vcpu_count as u32),
                })
                .await?;
            debug!(
                sandbox_id = %id.0,
                stage = "helper_create",
                elapsed_ms = helper_create_started.elapsed().as_millis() as u64,
                "apple helper create request finished"
            );
            match response {
                HelperResponse::Ack => {}
                HelperResponse::Error { message } => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "apple helper create failed: {message}"
                    )));
                }
                other => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "unexpected apple helper response on create: {other:?}"
                    )));
                }
            }
            None
        } else {
            None
        };

        self.sandboxes.lock().await.insert(
            id.clone(),
            AppleSandbox {
                info: info.clone(),
                config,
                direct_container,
            },
        );
        info!(
            sandbox_id = %id.0,
            stage = "create",
            elapsed_ms = create_started.elapsed().as_millis() as u64,
            "apple backend sandbox created"
        );

        Ok(SandboxLease { id, info })
    }

    async fn exec(
        &self,
        sandbox_id: &SandboxId,
        req: hyperbox_core::ExecRequest,
    ) -> Result<hyperbox_core::ExecOutcome> {
        let exec_started = std::time::Instant::now();

        let direct_container = {
            let sandboxes = self.sandboxes.lock().await;
            let sandbox = sandboxes
                .get(sandbox_id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;
            sandbox.direct_container.clone()
        };

        if let Some(direct) = direct_container {
            let outcome = self.exec_direct(&direct, req).await?;
            info!(
                sandbox_id = %sandbox_id.0,
                stage = "exec_direct",
                helper_exec_ms = outcome.duration_ms as u64,
                backend_elapsed_ms = exec_started.elapsed().as_millis() as u64,
                exit_code = outcome.exit_code,
                "apple backend exec completed via direct container path"
            );
            return Ok(outcome);
        }

        if self.config.launch_command.is_some() {
            let helper_exec_started = std::time::Instant::now();
            match self
                .helper_request(&HelperRequest::Exec {
                    sandbox_id: sandbox_id.0.to_string(),
                    command: req.command,
                    timeout_secs: req.timeout_secs,
                })
                .await?
            {
                HelperResponse::Exec {
                    exit_code,
                    stdout,
                    stderr,
                    duration_ms,
                } => {
                    info!(
                        sandbox_id = %sandbox_id.0,
                        stage = "exec",
                        helper_exec_ms = duration_ms,
                        backend_elapsed_ms = exec_started.elapsed().as_millis() as u64,
                        round_trip_ms = helper_exec_started.elapsed().as_millis() as u64,
                        exit_code,
                        "apple backend exec completed via helper"
                    );
                    return Ok(hyperbox_core::ExecOutcome {
                        exit_code,
                        stdout,
                        stderr,
                        duration_ms: duration_ms as u128,
                    });
                }
                HelperResponse::Error { message } => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "apple helper exec failed: {message}"
                    )));
                }
                other => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "unexpected apple helper response on exec: {other:?}"
                    )));
                }
            }
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
        info!(
            sandbox_id = %sandbox_id.0,
            stage = "exec_via_agent",
            elapsed_ms = exec_started.elapsed().as_millis() as u64,
            exit_code = response.exit_code,
            "apple backend exec completed via agent path"
        );
        Ok(hyperbox_core::ExecOutcome {
            exit_code: response.exit_code,
            stdout: response.stdout,
            stderr: response.stderr,
            duration_ms: response.duration_ms as u128,
        })
    }

    async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        let direct_container = {
            let sandboxes = self.sandboxes.lock().await;
            let sandbox = sandboxes
                .get(sandbox_id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;
            sandbox.direct_container.clone()
        };

        if let Some(direct) = direct_container {
            return self
                .read_file_direct(&direct, path, DEFAULT_IO_TIMEOUT_SECS)
                .await;
        }

        if self.config.launch_command.is_some() {
            match self
                .helper_request(&HelperRequest::Read {
                    sandbox_id: sandbox_id.0.to_string(),
                    path: path.to_string(),
                })
                .await?
            {
                HelperResponse::Read { bytes_b64 } => {
                    let bytes = BASE64.decode(bytes_b64.as_bytes()).map_err(|e| {
                        HyperboxError::ExecutionFailed(format!("decode helper read payload: {e}"))
                    })?;
                    return Ok(FilePayload {
                        path: path.to_string().into(),
                        bytes,
                    });
                }
                HelperResponse::Error { message } => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "apple helper read failed: {message}"
                    )));
                }
                other => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "unexpected apple helper response on read: {other:?}"
                    )));
                }
            }
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
        let direct_container = {
            let sandboxes = self.sandboxes.lock().await;
            let sandbox = sandboxes
                .get(sandbox_id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;
            sandbox.direct_container.clone()
        };

        if let Some(direct) = direct_container {
            return self
                .write_file_direct(&direct, payload, DEFAULT_IO_TIMEOUT_SECS)
                .await;
        }

        if self.config.launch_command.is_some() {
            match self
                .helper_request(&HelperRequest::Write {
                    sandbox_id: sandbox_id.0.to_string(),
                    path: payload.path.to_string(),
                    bytes_b64: BASE64.encode(payload.bytes),
                })
                .await?
            {
                HelperResponse::Ack => return Ok(()),
                HelperResponse::Error { message } => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "apple helper write failed: {message}"
                    )));
                }
                other => {
                    return Err(HyperboxError::ExecutionFailed(format!(
                        "unexpected apple helper response on write: {other:?}"
                    )));
                }
            }
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

    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        snapshot_id: &SnapshotId,
        artifact_path: &Path,
    ) -> Result<()> {
        let rel_path = snapshot_archive_relpath(snapshot_id);
        let abs_path = path_in_container(Path::new(&rel_path));
        let direct_container = {
            let sandboxes = self.sandboxes.lock().await;
            sandboxes
                .get(sandbox_id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?
                .direct_container
                .clone()
        };

        let create_outcome = self
            .exec(
                sandbox_id,
                hyperbox_core::ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        snapshot_create_command(&abs_path),
                    ],
                    timeout_secs: 1800,
                },
            )
            .await?;
        if create_outcome.exit_code != 0 {
            return Err(HyperboxError::ExecutionFailed(format!(
                "snapshot archive create failed (exit={}): {}",
                create_outcome.exit_code,
                truncate_error_output(&create_outcome.stderr)
            )));
        }
        if let Some(parent) = artifact_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if let Some(direct) = direct_container {
            let host_archive_path = direct.workspace_host.join(&rel_path);
            tokio::fs::copy(&host_archive_path, artifact_path).await?;
            let _ = tokio::fs::remove_file(host_archive_path).await;
        } else {
            let bytes = self.read_file(sandbox_id, &rel_path).await?.bytes;
            tokio::fs::write(artifact_path, &bytes).await?;

            let _ = self
                .exec(
                    sandbox_id,
                    hyperbox_core::ExecRequest {
                        command: vec![
                            "/bin/sh".to_string(),
                            "-lc".to_string(),
                            format!("rm -f {abs_path}"),
                        ],
                        timeout_secs: 30,
                    },
                )
                .await;
        }
        Ok(())
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

        let rel_path = snapshot_archive_relpath(snapshot_id);
        let abs_path = path_in_container(Path::new(&rel_path));
        let direct_container = {
            let sandboxes = self.sandboxes.lock().await;
            sandboxes
                .get(&lease.id)
                .ok_or_else(|| HyperboxError::SandboxNotFound(lease.id.0.to_string()))?
                .direct_container
                .clone()
        };
        let write_result: Result<()> = if let Some(direct) = direct_container {
            let host_archive_path = direct.workspace_host.join(&rel_path);
            if let Some(parent) = host_archive_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            info!(
                snapshot_id = %snapshot_id.0,
                sandbox_id = %lease.id.0,
                direct_mode = true,
                artifact_path = %artifact_path.display(),
                host_archive_path = %host_archive_path.display(),
                "apple backend restoring snapshot archive into sandbox"
            );
            tokio::fs::copy(artifact_path, &host_archive_path).await?;
            Ok(())
        } else {
            let bytes = tokio::fs::read(artifact_path).await?;
            info!(
                snapshot_id = %snapshot_id.0,
                sandbox_id = %lease.id.0,
                direct_mode = false,
                payload_bytes = bytes.len(),
                "apple backend restoring snapshot archive into sandbox"
            );
            self.write_file(
                &lease.id,
                FilePayload {
                    path: rel_path.clone().into(),
                    bytes,
                },
            )
            .await
        };
        if let Err(err) = write_result {
            let _ = self.destroy(&lease.id).await;
            return Err(err);
        }

        let restore_outcome = self
            .exec(
                &lease.id,
                hyperbox_core::ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        snapshot_restore_command(&abs_path),
                    ],
                    timeout_secs: 1800,
                },
            )
            .await;
        match restore_outcome {
            Ok(outcome) if outcome.exit_code == 0 => {
                if let Some(direct) = {
                    let sandboxes = self.sandboxes.lock().await;
                    sandboxes
                        .get(&lease.id)
                        .and_then(|sandbox| sandbox.direct_container.clone())
                } {
                    let host_archive_path = direct.workspace_host.join(&rel_path);
                    let _ = tokio::fs::remove_file(host_archive_path).await;
                } else {
                    let _ = self
                        .exec(
                            &lease.id,
                            hyperbox_core::ExecRequest {
                                command: vec![
                                    "/bin/sh".to_string(),
                                    "-lc".to_string(),
                                    format!("rm -f {abs_path}"),
                                ],
                                timeout_secs: 30,
                            },
                        )
                        .await;
                }
                Ok(lease)
            }
            Ok(outcome) => {
                let _ = self.destroy(&lease.id).await;
                Err(HyperboxError::ExecutionFailed(format!(
                    "snapshot restore failed: {}",
                    truncate_error_output(&outcome.stderr)
                )))
            }
            Err(err) => {
                let _ = self.destroy(&lease.id).await;
                Err(err)
            }
        }
    }

    async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()> {
        let destroy_started = std::time::Instant::now();
        let sandbox = self
            .sandboxes
            .lock()
            .await
            .remove(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(direct) = sandbox.direct_container {
            self.destroy_direct_container(direct).await?;
        } else if self.config.launch_command.is_some() {
            if let Err(err) = self
                .helper_request(&HelperRequest::Destroy {
                    sandbox_id: sandbox_id.0.to_string(),
                })
                .await
            {
                warn!(
                    sandbox_id = %sandbox_id.0,
                    error = %err,
                    "apple backend failed to destroy sandbox via helper"
                );
                return Err(err);
            }
        }
        info!(
            sandbox_id = %sandbox_id.0,
            stage = "destroy",
            elapsed_ms = destroy_started.elapsed().as_millis() as u64,
            "apple backend sandbox destroyed"
        );

        Ok(())
    }

    async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .map(|sandbox| {
                let _ = &sandbox.config;
                sandbox.info.clone()
            })
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))
    }
}

fn resolve_direct_container_bin(launch_command: Option<&Vec<String>>) -> Option<String> {
    let command = launch_command?;
    if command.len() < 2 || command[1] != "apple-helper" {
        return None;
    }
    extract_container_bin_from_helper_args(command).or_else(|| Some("container".to_string()))
}

fn extract_container_bin_from_helper_args(args: &[String]) -> Option<String> {
    let mut idx = 0usize;
    while idx < args.len() {
        if args[idx] == "--container-bin" {
            return args.get(idx + 1).cloned();
        }
        idx += 1;
    }
    None
}

fn snapshot_archive_relpath(snapshot_id: &SnapshotId) -> String {
    format!("{SNAPSHOT_ARCHIVE_DIR_REL}/{}.tar.gz", snapshot_id.0)
}

fn snapshot_create_command(snapshot_archive_abs: &str) -> String {
    format!(
        "set -eu; mkdir -p /workspace/{dir}; set --; for d in bin boot etc home lib lib64 opt root sbin srv usr var; do [ -e \"/$d\" ] && set -- \"$@\" \"$d\"; done; [ \"$#\" -gt 0 ]; tar --ignore-failed-read -czf {archive} -C / \"$@\"",
        dir = SNAPSHOT_ARCHIVE_DIR_REL,
        archive = snapshot_archive_abs
    )
}

fn snapshot_restore_command(snapshot_archive_abs: &str) -> String {
    format!(
        "tar --no-same-owner --no-same-permissions --no-overwrite-dir --touch -xzf {snapshot_archive_abs} -C /"
    )
}

fn truncate_error_output(raw: &str) -> String {
    const MAX_CHARS: usize = 1024;
    if raw.chars().count() <= MAX_CHARS {
        return raw.trim().to_string();
    }
    let mut out = String::with_capacity(MAX_CHARS + 32);
    for ch in raw.chars().take(MAX_CHARS) {
        out.push(ch);
    }
    out.push_str("... (truncated)");
    out
}

fn stderr_summary(stderr: &[u8]) -> String {
    truncate_error_output(&String::from_utf8_lossy(stderr))
}

async fn run_container_command(
    container_bin: &str,
    args: Vec<String>,
    stdin_payload: Option<Vec<u8>>,
    timeout_secs: u64,
) -> Result<CommandResult> {
    let mut cmd = Command::new(container_bin);
    cmd.args(&args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if stdin_payload.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let mut child = cmd.spawn().map_err(|e| {
        HyperboxError::ExecutionFailed(format!("spawn `{container_bin}` with args {:?}: {e}", args))
    })?;

    if let Some(payload) = stdin_payload {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            HyperboxError::ExecutionFailed("spawned process missing stdin".to_string())
        })?;
        stdin
            .write_all(&payload)
            .await
            .map_err(|e| HyperboxError::ExecutionFailed(format!("write stdin payload: {e}")))?;
        stdin.shutdown().await.map_err(|e| {
            HyperboxError::ExecutionFailed(format!("close stdin payload pipe: {e}"))
        })?;
    }

    let mut child_stdout = child.stdout.take().ok_or_else(|| {
        HyperboxError::ExecutionFailed("spawned process missing stdout".to_string())
    })?;
    let mut child_stderr = child.stderr.take().ok_or_else(|| {
        HyperboxError::ExecutionFailed("spawned process missing stderr".to_string())
    })?;

    let stdout_task = tokio::spawn(async move {
        let mut data = Vec::new();
        child_stdout
            .read_to_end(&mut data)
            .await
            .map(|_| data)
            .map_err(|e| HyperboxError::ExecutionFailed(format!("read stdout: {e}")))
    });
    let stderr_task = tokio::spawn(async move {
        let mut data = Vec::new();
        child_stderr
            .read_to_end(&mut data)
            .await
            .map(|_| data)
            .map_err(|e| HyperboxError::ExecutionFailed(format!("read stderr: {e}")))
    });

    let started_at = Instant::now();
    let status = match timeout(Duration::from_secs(timeout_secs.max(1)), child.wait()).await {
        Ok(result) => {
            result.map_err(|e| HyperboxError::ExecutionFailed(format!("wait for command: {e}")))?
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(HyperboxError::ExecutionFailed(format!(
                "command timed out after {timeout_secs}s"
            )));
        }
    };

    let stdout = stdout_task
        .await
        .map_err(|e| HyperboxError::ExecutionFailed(format!("join stdout task: {e}")))??;
    let stderr = stderr_task
        .await
        .map_err(|e| HyperboxError::ExecutionFailed(format!("join stderr task: {e}")))??;

    Ok(CommandResult {
        exit_code: status.code().unwrap_or(1),
        stdout,
        stderr,
        duration_ms: started_at.elapsed().as_millis() as u64,
    })
}

fn normalize_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(HyperboxError::ExecutionFailed(
            "absolute paths are not allowed".to_string(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                return Err(HyperboxError::ExecutionFailed(
                    "path traversal is not allowed".to_string(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(HyperboxError::ExecutionFailed("invalid path".to_string()));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(HyperboxError::ExecutionFailed(
            "path cannot be empty".to_string(),
        ));
    }

    Ok(normalized)
}

fn path_in_container(relative: &Path) -> String {
    Path::new(WORKDIR_IN_CONTAINER)
        .join(relative)
        .to_string_lossy()
        .to_string()
}

fn ensure_supported_apple_network_mode(network: &NetworkMode) -> Result<()> {
    if matches!(network, NetworkMode::Allowlist(_)) {
        return Err(HyperboxError::InvalidConfig(
            "apple backend does not enforce allowlist yet; use network=none or network=full"
                .to_string(),
        ));
    }
    Ok(())
}

fn helper_network_fields(network: &NetworkMode) -> (String, Vec<String>) {
    match network {
        NetworkMode::None => ("none".to_string(), vec![]),
        NetworkMode::Full => ("full".to_string(), vec![]),
        NetworkMode::Allowlist(domains) => ("allowlist".to_string(), domains.clone()),
    }
}

fn container_network_args(network: &NetworkMode) -> Result<Vec<String>> {
    match network {
        NetworkMode::None => Ok(vec!["--network".to_string(), "none".to_string()]),
        NetworkMode::Full => Ok(vec![]),
        NetworkMode::Allowlist(_) => Err(HyperboxError::InvalidConfig(
            "apple backend does not enforce allowlist yet; use network=none or network=full"
                .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppleBackendConfig, AppleRuntimeKind, AppleVzBackend, container_network_args,
        ensure_supported_apple_network_mode,
    };
    use hyperbox_core::{HyperboxError, NetworkMode, SandboxBackend, SandboxConfig};

    #[tokio::test]
    async fn create_requires_helper_command() {
        let backend = AppleVzBackend::new(AppleBackendConfig {
            launch_command: None,
            runtime_kind: AppleRuntimeKind::Virtualization,
            ..AppleBackendConfig::default()
        });

        let err = backend
            .create(SandboxConfig::default())
            .await
            .expect_err("create should fail when helper command is missing");
        assert!(matches!(err, HyperboxError::InvalidConfig(_)));
        assert!(
            err.to_string().contains("HYPERBOX_APPLE_HELPER"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apple_network_modes_allow_none_and_full() {
        assert!(ensure_supported_apple_network_mode(&NetworkMode::None).is_ok());
        assert!(ensure_supported_apple_network_mode(&NetworkMode::Full).is_ok());
    }

    #[test]
    fn apple_network_mode_rejects_allowlist_without_enforcement() {
        let err = ensure_supported_apple_network_mode(&NetworkMode::Allowlist(vec![
            "pypi.org".to_string(),
        ]))
        .expect_err("allowlist should fail without enforcement");
        assert!(
            err.to_string().contains("does not enforce allowlist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn container_network_args_follow_mode() {
        let none_args = container_network_args(&NetworkMode::None).expect("none args");
        assert_eq!(none_args, vec!["--network".to_string(), "none".to_string()]);
        let full_args = container_network_args(&NetworkMode::Full).expect("full args");
        assert!(full_args.is_empty());
    }
}
