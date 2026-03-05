use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout},
    sync::Mutex,
};

use hyperbox_core::{
    FilePayload, HyperboxError, NetworkMode, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SandboxLease, SandboxState,
};
use hyperbox_proto::hyperbox::v1::hyperbox_agent_client::HyperboxAgentClient;

use crate::detect_macos_capabilities;

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
        duration_ms: u128,
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

    async fn shutdown(&mut self, sandbox_id: &SandboxId) -> Result<()> {
        let _ = self
            .request(&HelperRequest::Destroy {
                sandbox_id: sandbox_id.0.to_string(),
            })
            .await;
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
    helper: Option<AppleHelperSession>,
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

    fn runtime_name(&self) -> &'static str {
        match self.config.runtime_kind {
            AppleRuntimeKind::Containerization => "containerization",
            AppleRuntimeKind::Virtualization => "virtualization",
        }
    }

    async fn start_helper_if_needed(
        &self,
        sandbox_id: &SandboxId,
    ) -> Result<Option<AppleHelperSession>> {
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

        Ok(Some(AppleHelperSession {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            _stderr: stderr,
        }))
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

        let mut helper = self.start_helper_if_needed(&id).await?;
        if let Some(helper_session) = helper.as_mut() {
            let response = helper_session
                .request(&HelperRequest::Create {
                    sandbox_id: id.0.to_string(),
                    template: config.template.clone(),
                    workspace_dir: config.workspace_dir.clone(),
                    runtime: self.runtime_name().to_string(),
                })
                .await?;
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
        }

        self.sandboxes.lock().await.insert(
            id.clone(),
            AppleSandbox {
                info: info.clone(),
                config,
                helper,
            },
        );

        Ok(SandboxLease { id, info })
    }

    async fn exec(
        &self,
        sandbox_id: &SandboxId,
        req: hyperbox_core::ExecRequest,
    ) -> Result<hyperbox_core::ExecOutcome> {
        let mut sandboxes = self.sandboxes.lock().await;
        let sandbox = sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(helper) = sandbox.helper.as_mut() {
            match helper
                .request(&HelperRequest::Exec {
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
                    return Ok(hyperbox_core::ExecOutcome {
                        exit_code,
                        stdout,
                        stderr,
                        duration_ms,
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
        Ok(hyperbox_core::ExecOutcome {
            exit_code: response.exit_code,
            stdout: response.stdout,
            stderr: response.stderr,
            duration_ms: response.duration_ms as u128,
        })
    }

    async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        let mut sandboxes = self.sandboxes.lock().await;
        let sandbox = sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(helper) = sandbox.helper.as_mut() {
            match helper
                .request(&HelperRequest::Read {
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
        let mut sandboxes = self.sandboxes.lock().await;
        let sandbox = sandboxes
            .get_mut(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(helper) = sandbox.helper.as_mut() {
            match helper
                .request(&HelperRequest::Write {
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

    async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()> {
        let mut sandbox = self
            .sandboxes
            .lock()
            .await
            .remove(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if let Some(helper) = sandbox.helper.as_mut() {
            helper.shutdown(sandbox_id).await?;
        }

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

#[cfg(test)]
mod tests {
    use super::{AppleBackendConfig, AppleRuntimeKind, AppleVzBackend};
    use hyperbox_core::{HyperboxError, SandboxBackend, SandboxConfig};

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
}
