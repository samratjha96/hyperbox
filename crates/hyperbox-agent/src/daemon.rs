use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use tokio::{
    fs,
    process::Command,
    sync::Mutex,
    time::{Duration, timeout},
};
use tonic::{Request, Response, Status};

use hyperbox_proto::hyperbox::v1::{
    self as pb,
    hyperbox_agent_server::HyperboxAgent,
};

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
        Ok(dir)
    }
}

#[tonic::async_trait]
impl HyperboxAgent for AgentService {
    async fn exec(
        &self,
        request: Request<pb::ExecRequest>,
    ) -> Result<Response<pb::ExecResponse>, Status> {
        let request = request.into_inner();

        if request.command.is_empty() {
            return Err(Status::invalid_argument("command cannot be empty"));
        }

        let sandbox_id = if request.sandbox_id.is_empty() {
            "default"
        } else {
            request.sandbox_id.as_str()
        };
        let root = self.sandbox_root(sandbox_id).await?;

        let mut command = Command::new(&request.command[0]);
        command.args(&request.command[1..]).current_dir(root);

        let start = Instant::now();
        let output = timeout(
            Duration::from_secs(request.timeout_secs.max(1)),
            command.output(),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("command timed out"))?
        .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::ExecResponse {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        }))
    }

    async fn read_file(
        &self,
        request: Request<pb::ReadFileRequest>,
    ) -> Result<Response<pb::ReadFileResponse>, Status> {
        let request = request.into_inner();
        let sandbox_id = if request.sandbox_id.is_empty() {
            "default"
        } else {
            request.sandbox_id.as_str()
        };
        let root = self.sandbox_root(sandbox_id).await?;
        let full = root.join(request.path);

        let bytes = fs::read(full)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(pb::ReadFileResponse { bytes }))
    }

    async fn write_file(
        &self,
        request: Request<pb::WriteFileRequest>,
    ) -> Result<Response<pb::WriteFileResponse>, Status> {
        let request = request.into_inner();
        let sandbox_id = if request.sandbox_id.is_empty() {
            "default"
        } else {
            request.sandbox_id.as_str()
        };
        let root = self.sandbox_root(sandbox_id).await?;
        let full = root.join(request.path);

        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
        }

        fs::write(full, request.bytes)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(pb::WriteFileResponse {}))
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
