use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Instant,
};

use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, mpsc},
    time::{Duration, timeout},
};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

use hyperbox_proto::hyperbox::v1::{self as pb, hyperbox_agent_server::HyperboxAgent};

#[derive(Debug, Clone)]
struct AgentSandbox {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AgentService {
    root: PathBuf,
    sandboxes: Arc<Mutex<HashMap<String, AgentSandbox>>>,
}

impl AgentService {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn sandbox_root(&self, sandbox_id: &str) -> Result<PathBuf, Status> {
        let mut sandboxes = self.sandboxes.lock().await;
        if let Some(existing) = sandboxes.get(sandbox_id) {
            return Ok(existing.root.clone());
        }

        let dir = self.root.join(sandbox_id);
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        sandboxes.insert(sandbox_id.to_string(), AgentSandbox { root: dir.clone() });
        info!(sandbox_id = %sandbox_id, root = %dir.display(), "agent sandbox root initialized");
        Ok(dir)
    }

    fn normalize_relative_path(path: &str) -> Result<PathBuf, Status> {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            return Err(Status::invalid_argument(
                "path must be relative to sandbox root",
            ));
        }

        let mut normalized = PathBuf::new();
        for component in candidate.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => normalized.push(part),
                Component::ParentDir => {
                    return Err(Status::invalid_argument(
                        "path traversal outside sandbox root is not allowed",
                    ));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(Status::invalid_argument("invalid path"));
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            return Err(Status::invalid_argument("path cannot be empty"));
        }
        Ok(normalized)
    }

    async fn resolve_safe_path(
        root: &Path,
        raw_path: &str,
        create_parents: bool,
    ) -> Result<PathBuf, Status> {
        let normalized = Self::normalize_relative_path(raw_path)?;
        let mut cursor = root.to_path_buf();
        let components: Vec<_> = normalized.components().collect();

        for (idx, component) in components.iter().enumerate() {
            let part = match component {
                Component::Normal(part) => part,
                _ => return Err(Status::invalid_argument("invalid path component")),
            };
            let next = cursor.join(part);
            let is_last = idx + 1 == components.len();

            match fs::symlink_metadata(&next).await {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(Status::permission_denied(
                            "symlink paths are not allowed in sandbox file operations",
                        ));
                    }
                    if !is_last && !meta.is_dir() {
                        return Err(Status::failed_precondition(
                            "path component is not a directory",
                        ));
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    if !is_last {
                        if create_parents {
                            fs::create_dir(&next).await.map_err(|e| {
                                Status::internal(format!("create directory failed: {e}"))
                            })?;
                        } else {
                            return Err(Status::not_found("path not found"));
                        }
                    }
                }
                Err(err) => {
                    return Err(Status::internal(format!("inspect path failed: {err}")));
                }
            }

            cursor = next;
        }

        Ok(cursor)
    }
}

#[tonic::async_trait]
impl HyperboxAgent for AgentService {
    type ShellStream = Pin<Box<dyn Stream<Item = Result<pb::ShellEvent, Status>> + Send>>;

    async fn exec(
        &self,
        request: Request<pb::ExecRequest>,
    ) -> Result<Response<pb::ExecResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();

        if request.command.is_empty() {
            return Err(Status::invalid_argument("command cannot be empty"));
        }

        let sandbox_id = if request.sandbox_id.is_empty() {
            "default"
        } else {
            request.sandbox_id.as_str()
        };
        info!(
            peer = ?peer,
            sandbox_id = %sandbox_id,
            timeout_secs = request.timeout_secs,
            command = %request.command.join(" "),
            "agent exec request"
        );
        let root = self.sandbox_root(sandbox_id).await?;

        let mut command = Command::new(&request.command[0]);
        command.args(&request.command[1..]).current_dir(root);

        let start = Instant::now();
        let output = timeout(
            Duration::from_secs(request.timeout_secs.max(1)),
            command.output(),
        )
        .await
        .map_err(|_| {
            warn!(peer = ?peer, sandbox_id = %sandbox_id, "agent exec timeout");
            Status::deadline_exceeded("command timed out")
        })?
        .map_err(|e| {
            error!(peer = ?peer, sandbox_id = %sandbox_id, error = %e, "agent exec process failure");
            Status::internal(e.to_string())
        })?;
        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            peer = ?peer,
            sandbox_id = %sandbox_id,
            exit_code = output.status.code().unwrap_or(1),
            duration_ms,
            "agent exec completed"
        );

        Ok(Response::new(pb::ExecResponse {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms,
        }))
    }

    async fn read_file(
        &self,
        request: Request<pb::ReadFileRequest>,
    ) -> Result<Response<pb::ReadFileResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = if request.sandbox_id.is_empty() {
            "default"
        } else {
            request.sandbox_id.as_str()
        };
        debug!(peer = ?peer, sandbox_id = %sandbox_id, path = %request.path, "agent read_file request");
        let root = self.sandbox_root(sandbox_id).await?;
        let full = Self::resolve_safe_path(&root, &request.path, false).await?;

        let bytes = fs::read(full).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Status::not_found(e.to_string()),
            _ => Status::internal(e.to_string()),
        })?;
        Ok(Response::new(pb::ReadFileResponse { bytes }))
    }

    async fn write_file(
        &self,
        request: Request<pb::WriteFileRequest>,
    ) -> Result<Response<pb::WriteFileResponse>, Status> {
        let peer = request.remote_addr();
        let request = request.into_inner();
        let sandbox_id = if request.sandbox_id.is_empty() {
            "default"
        } else {
            request.sandbox_id.as_str()
        };
        debug!(
            peer = ?peer,
            sandbox_id = %sandbox_id,
            path = %request.path,
            bytes = request.bytes.len(),
            "agent write_file request"
        );
        let root = self.sandbox_root(sandbox_id).await?;
        let full = Self::resolve_safe_path(&root, &request.path, true).await?;

        fs::write(full, request.bytes)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(pb::WriteFileResponse {}))
    }

    async fn shell(
        &self,
        request: Request<tonic::Streaming<pb::ShellRequest>>,
    ) -> Result<Response<Self::ShellStream>, Status> {
        let peer = request.remote_addr();
        let mut inbound = request.into_inner();
        let service = self.clone();
        let (tx, rx) = mpsc::channel::<Result<pb::ShellEvent, Status>>(64);

        tokio::spawn(async move {
            let first = match inbound.message().await {
                Ok(Some(msg)) => msg,
                Ok(None) => {
                    let _ = tx
                        .send(Err(Status::invalid_argument(
                            "shell stream requires an initial open message",
                        )))
                        .await;
                    return;
                }
                Err(err) => {
                    let _ = tx.send(Err(Status::internal(err.to_string()))).await;
                    return;
                }
            };

            let open = match first.payload {
                Some(pb::shell_request::Payload::Open(open)) => open,
                _ => pb::ShellOpenRequest {
                    sandbox_id: String::new(),
                    command: vec![],
                },
            };
            let sandbox_id = if open.sandbox_id.is_empty() {
                "default".to_string()
            } else {
                open.sandbox_id
            };
            let command = if open.command.is_empty() {
                vec!["/bin/sh".to_string()]
            } else {
                open.command
            };

            if command.is_empty() {
                let _ = tx
                    .send(Err(Status::invalid_argument(
                        "shell command cannot be empty",
                    )))
                    .await;
                return;
            }

            let root = match service.sandbox_root(&sandbox_id).await {
                Ok(root) => root,
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    return;
                }
            };

            info!(
                peer = ?peer,
                sandbox_id = %sandbox_id,
                command = %command.join(" "),
                "agent shell open"
            );

            let mut child = match Command::new(&command[0])
                .args(&command[1..])
                .current_dir(root)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(err) => {
                    let _ = tx
                        .send(Ok(pb::ShellEvent {
                            payload: Some(pb::shell_event::Payload::Error(err.to_string())),
                        }))
                        .await;
                    let _ = tx
                        .send(Ok(pb::ShellEvent {
                            payload: Some(pb::shell_event::Payload::ExitCode(1)),
                        }))
                        .await;
                    return;
                }
            };

            let mut stdin = match child.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    let _ = tx
                        .send(Err(Status::internal("shell child missing stdin")))
                        .await;
                    let _ = child.kill().await;
                    return;
                }
            };
            let mut stdout = match child.stdout.take() {
                Some(stdout) => stdout,
                None => {
                    let _ = tx
                        .send(Err(Status::internal("shell child missing stdout")))
                        .await;
                    let _ = child.kill().await;
                    return;
                }
            };
            let mut stderr = match child.stderr.take() {
                Some(stderr) => stderr,
                None => {
                    let _ = tx
                        .send(Err(Status::internal("shell child missing stderr")))
                        .await;
                    let _ = child.kill().await;
                    return;
                }
            };

            let tx_out = tx.clone();
            let stdout_task = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                loop {
                    let read = stdout
                        .read(&mut buf)
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    if read == 0 {
                        break;
                    }
                    tx_out
                        .send(Ok(pb::ShellEvent {
                            payload: Some(pb::shell_event::Payload::Stdout(buf[..read].to_vec())),
                        }))
                        .await
                        .map_err(|_| Status::cancelled("shell stream closed"))?;
                }
                Ok::<(), Status>(())
            });

            let tx_err = tx.clone();
            let stderr_task = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                loop {
                    let read = stderr
                        .read(&mut buf)
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    if read == 0 {
                        break;
                    }
                    tx_err
                        .send(Ok(pb::ShellEvent {
                            payload: Some(pb::shell_event::Payload::Stderr(buf[..read].to_vec())),
                        }))
                        .await
                        .map_err(|_| Status::cancelled("shell stream closed"))?;
                }
                Ok::<(), Status>(())
            });

            loop {
                match inbound.message().await {
                    Ok(Some(req)) => match req.payload {
                        Some(pb::shell_request::Payload::Stdin(chunk)) => {
                            if let Err(err) = stdin.write_all(&chunk).await {
                                let _ = tx
                                    .send(Ok(pb::ShellEvent {
                                        payload: Some(pb::shell_event::Payload::Error(
                                            err.to_string(),
                                        )),
                                    }))
                                    .await;
                                break;
                            }
                            if let Err(err) = stdin.flush().await {
                                let _ = tx
                                    .send(Ok(pb::ShellEvent {
                                        payload: Some(pb::shell_event::Payload::Error(
                                            err.to_string(),
                                        )),
                                    }))
                                    .await;
                                break;
                            }
                        }
                        Some(pb::shell_request::Payload::Close(_)) => break,
                        Some(pb::shell_request::Payload::Open(_)) | None => {}
                    },
                    Ok(None) => break,
                    Err(err) => {
                        let _ = tx
                            .send(Ok(pb::ShellEvent {
                                payload: Some(pb::shell_event::Payload::Error(err.to_string())),
                            }))
                            .await;
                        break;
                    }
                }
            }

            drop(stdin);
            let status = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(1);
            let _ = tx
                .send(Ok(pb::ShellEvent {
                    payload: Some(pb::shell_event::Payload::ExitCode(exit_code)),
                }))
                .await;
            info!(
                peer = ?peer,
                sandbox_id = %sandbox_id,
                exit_code,
                "agent shell closed"
            );
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

pub async fn serve_agent(addr: std::net::SocketAddr, root: PathBuf) -> anyhow::Result<()> {
    let service = AgentService::new(root);

    tonic::transport::Server::builder()
        .add_service(pb::hyperbox_agent_server::HyperboxAgentServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::AgentService;

    #[test]
    fn normalize_relative_path_rejects_unsafe_inputs() {
        assert!(AgentService::normalize_relative_path("/etc/passwd").is_err());
        assert!(AgentService::normalize_relative_path("../secret").is_err());
        assert!(AgentService::normalize_relative_path("").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_safe_path_blocks_symlink_traversal() {
        let root = tempfile::tempdir().expect("tempdir");
        let escape_target = tempfile::tempdir().expect("escape target");
        let link_path = root.path().join("link");
        std::os::unix::fs::symlink(escape_target.path(), &link_path).expect("create symlink");

        let err = AgentService::resolve_safe_path(root.path(), "link/file.txt", true)
            .await
            .expect_err("symlink traversal should fail");
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
