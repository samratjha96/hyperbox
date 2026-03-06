use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use anyhow::{Context, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::{Duration, timeout},
};
use tracing::{debug, info, warn};

const WORKDIR_IN_CONTAINER: &str = "/workspace";
const DEFAULT_IO_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DESTROY_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct AppleHelperConfig {
    pub container_bin: String,
    pub state_root: PathBuf,
}

#[derive(Debug)]
struct AppleHelper {
    config: AppleHelperConfig,
    sandboxes: HashMap<String, SandboxSession>,
}

#[derive(Debug)]
struct SandboxSession {
    container_name: String,
    workspace_host: PathBuf,
    ephemeral_workspace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeKind {
    Containerization,
    Virtualization,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Serialize)]
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

#[derive(Debug)]
struct CommandResult {
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    duration_ms: u64,
}

pub async fn run(config: AppleHelperConfig) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&config.state_root)
        .await
        .with_context(|| {
            format!(
                "create apple helper state root {}",
                config.state_root.display()
            )
        })?;

    let mut helper = AppleHelper {
        config,
        sandboxes: HashMap::new(),
    };
    helper.run_loop().await
}

impl AppleHelper {
    async fn run_loop(&mut self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let mut lines = BufReader::new(stdin).lines();
        let mut stdout = tokio::io::stdout();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            let response = match serde_json::from_str::<HelperRequest>(&line) {
                Ok(request) => self.handle_request(request).await,
                Err(err) => HelperResponse::Error {
                    message: format!("invalid request: {err}"),
                },
            };

            let encoded = serde_json::to_string(&response)?;
            stdout.write_all(encoded.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }

        Ok(())
    }

    async fn handle_request(&mut self, request: HelperRequest) -> HelperResponse {
        let result = match request {
            HelperRequest::Create {
                sandbox_id,
                template,
                workspace_dir,
                runtime,
            } => {
                self.create_sandbox(sandbox_id, template, workspace_dir, runtime)
                    .await
            }
            HelperRequest::Exec {
                sandbox_id,
                command,
                timeout_secs,
            } => self.exec(sandbox_id, command, timeout_secs).await,
            HelperRequest::Read { sandbox_id, path } => self.read_file(sandbox_id, path).await,
            HelperRequest::Write {
                sandbox_id,
                path,
                bytes_b64,
            } => self.write_file(sandbox_id, path, bytes_b64).await,
            HelperRequest::Destroy { sandbox_id } => self.destroy_sandbox(sandbox_id).await,
        };

        match result {
            Ok(response) => response,
            Err(err) => HelperResponse::Error {
                message: err.to_string(),
            },
        }
    }

    async fn create_sandbox(
        &mut self,
        sandbox_id: String,
        template: String,
        workspace_dir: Option<String>,
        runtime: String,
    ) -> anyhow::Result<HelperResponse> {
        let started = Instant::now();
        let sandbox_id = sanitize_sandbox_id(&sandbox_id)?;
        let runtime = RuntimeKind::parse(&runtime)?;

        if runtime != RuntimeKind::Containerization {
            bail!(
                "runtime `{}` is not implemented by this helper; use runtime=containerization",
                runtime.as_str()
            );
        }

        if self.sandboxes.contains_key(&sandbox_id) {
            bail!("sandbox `{sandbox_id}` already exists");
        }

        let ephemeral_workspace = workspace_dir.is_none();
        let workspace_host = if let Some(dir) = workspace_dir {
            let path = PathBuf::from(dir);
            let canonical = tokio::fs::canonicalize(&path).await.with_context(|| {
                format!(
                    "workspace_dir does not exist or is not accessible: {}",
                    path.display()
                )
            })?;
            if !canonical.is_dir() {
                bail!("workspace_dir must be a directory: {}", canonical.display());
            }
            canonical
        } else {
            let workspace = self
                .config
                .state_root
                .join(&sandbox_id)
                .join("workspace")
                .to_path_buf();
            tokio::fs::create_dir_all(&workspace)
                .await
                .with_context(|| format!("create workspace directory {}", workspace.display()))?;
            workspace
        };
        let container_name = format!("hyperbox-{}", sandbox_id);
        self.start_container(&container_name, &template, &workspace_host)
            .await
            .with_context(|| {
                format!(
                    "start container runtime for sandbox `{sandbox_id}` using template `{template}`"
                )
            })?;

        self.sandboxes.insert(
            sandbox_id.clone(),
            SandboxSession {
                container_name,
                workspace_host,
                ephemeral_workspace,
            },
        );

        info!(
            sandbox_id = %sandbox_id,
            stage = "create",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "apple helper sandbox created"
        );

        Ok(HelperResponse::Ack)
    }

    async fn start_container(
        &self,
        container_name: &str,
        template: &str,
        workspace_host: &Path,
    ) -> anyhow::Result<()> {
        let mount = format!("{}:{}", workspace_host.display(), WORKDIR_IN_CONTAINER);
        let args = vec![
            "run".to_string(),
            "--detach".to_string(),
            "--name".to_string(),
            container_name.to_string(),
            "--network".to_string(),
            "none".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            "--volume".to_string(),
            mount,
            template.to_string(),
            "sleep".to_string(),
            "infinity".to_string(),
        ];

        let result = self
            .run_container_command(args, None, DEFAULT_IO_TIMEOUT_SECS)
            .await?;
        if result.exit_code != 0 {
            bail!(
                "container run failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }
        Ok(())
    }

    async fn exec(
        &mut self,
        sandbox_id: String,
        command: Vec<String>,
        timeout_secs: u64,
    ) -> anyhow::Result<HelperResponse> {
        let started = Instant::now();
        if command.is_empty() {
            bail!("command cannot be empty");
        }

        let session = self
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox `{sandbox_id}` not found"))?;

        let mut args = vec![
            "exec".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            session.container_name.clone(),
        ];
        args.extend(command);

        let result = self
            .run_container_command(args, None, timeout_secs.max(1))
            .await?;

        let response = HelperResponse::Exec {
            exit_code: result.exit_code,
            stdout: String::from_utf8_lossy(&result.stdout).to_string(),
            stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            duration_ms: result.duration_ms,
        };
        info!(
            sandbox_id = %sandbox_id,
            stage = "exec",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "apple helper command execution completed"
        );
        Ok(response)
    }

    async fn read_file(
        &mut self,
        sandbox_id: String,
        path: String,
    ) -> anyhow::Result<HelperResponse> {
        let session = self
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox `{sandbox_id}` not found"))?;

        let relative = normalize_relative_path(&path)?;
        let container_path = path_in_container(&relative);

        let args = vec![
            "exec".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            session.container_name.clone(),
            "/usr/bin/env".to_string(),
            "cat".to_string(),
            container_path,
        ];

        let result = self
            .run_container_command(args, None, DEFAULT_IO_TIMEOUT_SECS)
            .await?;
        if result.exit_code != 0 {
            bail!(
                "read failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }

        Ok(HelperResponse::Read {
            bytes_b64: BASE64.encode(result.stdout),
        })
    }

    async fn write_file(
        &mut self,
        sandbox_id: String,
        path: String,
        bytes_b64: String,
    ) -> anyhow::Result<HelperResponse> {
        let session = self
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox `{sandbox_id}` not found"))?;

        let relative = normalize_relative_path(&path)?;
        let bytes = BASE64
            .decode(bytes_b64.as_bytes())
            .context("decode write payload bytes_b64")?;
        let container_path = path_in_container(&relative);

        if let Some(parent) = relative.parent() {
            if !parent.as_os_str().is_empty() {
                let parent_in_container = path_in_container(parent);
                let mkdir_args = vec![
                    "exec".to_string(),
                    "--workdir".to_string(),
                    WORKDIR_IN_CONTAINER.to_string(),
                    session.container_name.clone(),
                    "/usr/bin/env".to_string(),
                    "mkdir".to_string(),
                    "-p".to_string(),
                    parent_in_container,
                ];
                let mkdir_result = self
                    .run_container_command(mkdir_args, None, DEFAULT_IO_TIMEOUT_SECS)
                    .await?;
                if mkdir_result.exit_code != 0 {
                    bail!(
                        "create parent directory failed (exit={}): {}",
                        mkdir_result.exit_code,
                        String::from_utf8_lossy(&mkdir_result.stderr)
                    );
                }
            }
        }

        let write_args = vec![
            "exec".to_string(),
            "--interactive".to_string(),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            session.container_name.clone(),
            "/usr/bin/env".to_string(),
            "tee".to_string(),
            container_path,
        ];
        let write_result = self
            .run_container_command(write_args, Some(bytes), DEFAULT_IO_TIMEOUT_SECS)
            .await?;
        if write_result.exit_code != 0 {
            bail!(
                "write failed (exit={}): {}",
                write_result.exit_code,
                String::from_utf8_lossy(&write_result.stderr)
            );
        }

        Ok(HelperResponse::Ack)
    }

    async fn destroy_sandbox(&mut self, sandbox_id: String) -> anyhow::Result<HelperResponse> {
        let started = Instant::now();
        let Some(session) = self.sandboxes.remove(&sandbox_id) else {
            return Ok(HelperResponse::Ack);
        };

        let args = vec![
            "delete".to_string(),
            "--force".to_string(),
            session.container_name.clone(),
        ];
        let result = self
            .run_container_command(args, None, DEFAULT_DESTROY_TIMEOUT_SECS)
            .await?;
        if result.exit_code != 0 {
            let stderr = String::from_utf8_lossy(&result.stderr).to_lowercase();
            if !stderr.contains("not found") {
                bail!(
                    "container delete failed (exit={}): {}",
                    result.exit_code,
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }

        if session.ephemeral_workspace {
            tokio::fs::remove_dir_all(&session.workspace_host)
                .await
                .with_context(|| {
                    format!(
                        "remove ephemeral workspace {}",
                        session.workspace_host.display()
                    )
                })?;
            if let Some(sandbox_root) = session.workspace_host.parent() {
                let _ = tokio::fs::remove_dir(sandbox_root).await;
            }
        }

        info!(
            sandbox_id = %sandbox_id,
            stage = "destroy",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "apple helper sandbox destroyed"
        );
        Ok(HelperResponse::Ack)
    }

    async fn run_container_command(
        &self,
        args: Vec<String>,
        stdin_payload: Option<Vec<u8>>,
        timeout_secs: u64,
    ) -> anyhow::Result<CommandResult> {
        debug!(args = ?args, timeout_secs, "apple helper spawning container command");
        let mut cmd = Command::new(&self.config.container_bin);
        cmd.args(&args);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if stdin_payload.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }

        let mut child = cmd.spawn().with_context(|| {
            format!("spawn `{}` with args {:?}", self.config.container_bin, args)
        })?;

        if let Some(payload) = stdin_payload {
            let mut stdin = child
                .stdin
                .take()
                .context("spawned process missing stdin")?;
            stdin
                .write_all(&payload)
                .await
                .context("write stdin payload")?;
            stdin.shutdown().await.context("close stdin payload pipe")?;
        }

        let mut child_stdout = child
            .stdout
            .take()
            .context("spawned process missing stdout")?;
        let mut child_stderr = child
            .stderr
            .take()
            .context("spawned process missing stderr")?;

        let stdout_task = tokio::spawn(async move {
            let mut data = Vec::new();
            child_stdout
                .read_to_end(&mut data)
                .await
                .map(|_| data)
                .context("read stdout")
        });
        let stderr_task = tokio::spawn(async move {
            let mut data = Vec::new();
            child_stderr
                .read_to_end(&mut data)
                .await
                .map(|_| data)
                .context("read stderr")
        });

        let started_at = Instant::now();
        let status = match timeout(Duration::from_secs(timeout_secs.max(1)), child.wait()).await {
            Ok(result) => result.context("wait for command")?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                warn!(timeout_secs, "apple helper command timed out");
                bail!("command timed out after {timeout_secs}s");
            }
        };

        let stdout = stdout_task.await.context("join stdout task")??;
        let stderr = stderr_task.await.context("join stderr task")??;

        Ok(CommandResult {
            exit_code: status.code().unwrap_or(1),
            stdout,
            stderr,
            duration_ms: started_at.elapsed().as_millis() as u64,
        })
    }
}

impl RuntimeKind {
    fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "containerization" => Ok(Self::Containerization),
            "virtualization" => Ok(Self::Virtualization),
            other => bail!("unknown runtime `{other}`"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Containerization => "containerization",
            Self::Virtualization => "virtualization",
        }
    }
}

fn sanitize_sandbox_id(raw: &str) -> anyhow::Result<String> {
    if raw.is_empty() {
        bail!("sandbox_id is required");
    }
    if raw.len() > 128 {
        bail!("sandbox_id too long");
    }
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Ok(raw.to_string());
    }
    bail!("sandbox_id contains unsupported characters")
}

fn normalize_relative_path(raw: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        bail!("absolute paths are not allowed");
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => bail!("path traversal is not allowed"),
            Component::RootDir | Component::Prefix(_) => bail!("invalid path"),
        }
    }

    if normalized.as_os_str().is_empty() {
        bail!("path cannot be empty");
    }

    Ok(normalized)
}

fn path_in_container(relative: &Path) -> String {
    let joined = Path::new(WORKDIR_IN_CONTAINER).join(relative);
    joined.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::{RuntimeKind, normalize_relative_path, sanitize_sandbox_id};

    #[test]
    fn runtime_parser_accepts_supported_values() {
        assert_eq!(
            RuntimeKind::parse("containerization").expect("parse runtime"),
            RuntimeKind::Containerization
        );
        assert_eq!(
            RuntimeKind::parse("virtualization").expect("parse runtime"),
            RuntimeKind::Virtualization
        );
    }

    #[test]
    fn runtime_parser_rejects_unknown_values() {
        assert!(RuntimeKind::parse("unknown").is_err());
    }

    #[test]
    fn relative_path_normalization_blocks_unsafe_paths() {
        assert!(normalize_relative_path("/etc/passwd").is_err());
        assert!(normalize_relative_path("../secret.txt").is_err());
        assert!(normalize_relative_path("a/../../secret.txt").is_err());
    }

    #[test]
    fn relative_path_normalization_accepts_normal_paths() {
        let normalized = normalize_relative_path("src/main.rs").expect("normalize path");
        assert_eq!(normalized.to_string_lossy(), "src/main.rs");
    }

    #[test]
    fn sandbox_id_validation_rejects_invalid_values() {
        assert!(sanitize_sandbox_id("").is_err());
        assert!(sanitize_sandbox_id("../../bad").is_err());
        assert!(sanitize_sandbox_id("bad name").is_err());
    }

    #[test]
    fn sandbox_id_validation_accepts_safe_values() {
        let value = sanitize_sandbox_id("abc-123_DEF").expect("sanitize sandbox id");
        assert_eq!(value, "abc-123_DEF");
    }
}
