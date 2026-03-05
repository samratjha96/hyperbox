use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, Clone)]
pub struct FirecrackerApiClient {
    socket_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status_code: u16,
    pub body: String,
}

impl FirecrackerApiClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
        }
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<ApiResponse> {
        self.send("GET", path, None).await
    }

    pub async fn put_json<T: Serialize>(&self, path: &str, payload: &T) -> anyhow::Result<ApiResponse> {
        let body = serde_json::to_string(payload)?;
        self.send("PUT", path, Some(body)).await
    }

    pub async fn patch_json<T: Serialize>(&self, path: &str, payload: &T) -> anyhow::Result<ApiResponse> {
        let body = serde_json::to_string(payload)?;
        self.send("PATCH", path, Some(body)).await
    }

    async fn send(&self, method: &str, path: &str, body: Option<String>) -> anyhow::Result<ApiResponse> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect firecracker socket {}", self.socket_path.display()))?;

        let body = body.unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );

        stream.write_all(request.as_bytes()).await?;
        stream.shutdown().await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        parse_http_response(&response)
    }

    pub async fn set_machine_config(&self, vcpu_count: u8, mem_size_mib: u32) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/machine-config",
                &serde_json::json!({
                    "vcpu_count": vcpu_count,
                    "mem_size_mib": mem_size_mib,
                    "track_dirty_pages": true
                }),
            )
            .await?;

        ensure_success(response)
    }

    pub async fn set_boot_source(&self, kernel_image_path: &str, boot_args: &str) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/boot-source",
                &serde_json::json!({
                    "kernel_image_path": kernel_image_path,
                    "boot_args": boot_args
                }),
            )
            .await?;

        ensure_success(response)
    }

    pub async fn set_rootfs(&self, path_on_host: &str, is_read_only: bool) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/drives/rootfs",
                &serde_json::json!({
                    "drive_id": "rootfs",
                    "path_on_host": path_on_host,
                    "is_root_device": true,
                    "is_read_only": is_read_only
                }),
            )
            .await?;

        ensure_success(response)
    }

    pub async fn set_vsock(&self, guest_cid: u32, uds_path: &str) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/vsock",
                &serde_json::json!({
                    "vsock_id": "agent",
                    "guest_cid": guest_cid,
                    "uds_path": uds_path
                }),
            )
            .await?;

        ensure_success(response)
    }

    pub async fn attach_network(&self, tap_name: &str) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/network-interfaces/eth0",
                &serde_json::json!({
                    "iface_id": "eth0",
                    "host_dev_name": tap_name,
                    "guest_mac": "06:00:AC:10:00:02"
                }),
            )
            .await?;

        ensure_success(response)
    }

    pub async fn start_instance(&self) -> anyhow::Result<()> {
        let response = self
            .put_json("/actions", &serde_json::json!({"action_type": "InstanceStart"}))
            .await?;
        ensure_success(response)
    }

    pub async fn create_snapshot(&self, mem_file_path: &str, snapshot_path: &str) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/snapshot/create",
                &serde_json::json!({
                    "snapshot_type": "Full",
                    "snapshot_path": snapshot_path,
                    "mem_file_path": mem_file_path,
                }),
            )
            .await?;
        ensure_success(response)
    }

    pub async fn load_snapshot(&self, mem_file_path: &str, snapshot_path: &str) -> anyhow::Result<()> {
        let response = self
            .put_json(
                "/snapshot/load",
                &serde_json::json!({
                    "snapshot_path": snapshot_path,
                    "mem_file_path": mem_file_path,
                    "enable_diff_snapshots": false,
                    "resume_vm": true
                }),
            )
            .await?;
        ensure_success(response)
    }

    pub async fn describe_instance(&self) -> anyhow::Result<Value> {
        let response = self.get("/").await?;
        if response.status_code >= 400 {
            anyhow::bail!("firecracker describe instance failed: {} {}", response.status_code, response.body);
        }
        Ok(serde_json::from_str(&response.body).unwrap_or_else(|_| serde_json::json!({"raw": response.body})))
    }
}

fn ensure_success(response: ApiResponse) -> anyhow::Result<()> {
    if response.status_code >= 400 {
        anyhow::bail!(
            "firecracker api failed with status {}: {}",
            response.status_code,
            response.body
        )
    }

    Ok(())
}

fn parse_http_response(raw: &[u8]) -> anyhow::Result<ApiResponse> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response from firecracker"))?;

    let status_line = head.lines().next().unwrap_or_default();
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("missing HTTP status code"))?;

    Ok(ApiResponse {
        status_code,
        body: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_http_response() {
        let raw = b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
        let parsed = parse_http_response(raw).expect("parse response");
        assert_eq!(parsed.status_code, 204);
    }
}
