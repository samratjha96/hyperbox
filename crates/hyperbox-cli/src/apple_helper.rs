use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use anyhow::{Context, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hyperbox_core::config::normalize_allowlist_domains;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::{Duration, timeout},
};
use tracing::{debug, info, warn};

const WORKDIR_IN_CONTAINER: &str = "/workspace";
const DEFAULT_IO_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DESTROY_TIMEOUT_SECS: u64 = 30;
const ALLOWLIST_NETWORK_PREFIX: &str = "hyperbox-net-";
const ALLOWLIST_DNS_PREFIX: &str = "hyperbox-dns-";
const ALLOWLIST_DNS_TEMPLATE: &str = "python:3.12";
const ALLOWLIST_DNS_BOOTSTRAP: &str =
    "import base64,os;exec(base64.b64decode(os.environ['HB_DNS_SCRIPT_B64']).decode('utf-8'))";
const ALLOWLIST_DNS_SCRIPT: &str = r#"import base64
import json
import os
import socket


def load_allowlist():
    raw = os.environ.get("HB_ALLOWLIST_B64", "")
    if not raw:
        return []
    try:
        decoded = base64.b64decode(raw).decode("utf-8")
        values = json.loads(decoded)
    except Exception:
        return []
    return [str(v).lower() for v in values if isinstance(v, str) and v]


ALLOWLIST = load_allowlist()
UPSTREAM = os.environ.get("HB_DNS_UPSTREAM", "1.1.1.1")
UPSTREAM_ADDR = (UPSTREAM, 53)


def allows(host):
    host = host.lower()
    if not host:
        return False
    for entry in ALLOWLIST:
        if host == entry:
            return True
    return False


def parse_qname(packet):
    if len(packet) < 12:
        return ""
    idx = 12
    labels = []
    while idx < len(packet):
        length = packet[idx]
        idx += 1
        if length == 0:
            break
        if idx + length > len(packet):
            return ""
        labels.append(packet[idx:idx + length].decode("ascii", "ignore"))
        idx += length
    return ".".join(labels).lower()


def nxdomain(packet):
    if len(packet) < 12:
        return packet
    qdcount = packet[4:6]
    question = packet[12:]
    return packet[0:2] + b"\x81\x83" + qdcount + b"\x00\x00\x00\x00\x00\x00" + question


server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
server.bind(("0.0.0.0", 53))
upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
upstream.settimeout(2.0)

while True:
    try:
        packet, peer = server.recvfrom(4096)
    except Exception:
        continue

    host = parse_qname(packet)
    if not allows(host):
        try:
            server.sendto(nxdomain(packet), peer)
        except Exception:
            pass
        continue

    try:
        upstream.sendto(packet, UPSTREAM_ADDR)
        response, _ = upstream.recvfrom(4096)
        server.sendto(response, peer)
    except Exception:
        try:
            server.sendto(nxdomain(packet), peer)
        except Exception:
            pass
"#;

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
    allowlist_runtime: Option<AllowlistRuntime>,
}

#[derive(Debug)]
struct AllowlistRuntime {
    network_name: String,
    dns_container_name: String,
}

#[derive(Debug)]
struct AllowlistRuntimeSetup {
    network_name: String,
    dns_container_name: String,
    dns_server_ip: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeKind {
    Containerization,
    Virtualization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelperNetworkMode {
    None,
    Full,
    Allowlist(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HelperRequest {
    Create {
        sandbox_id: String,
        template: String,
        workspace_dir: Option<String>,
        runtime: String,
        network_mode: Option<String>,
        network_allowlist: Option<Vec<String>>,
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
                network_mode,
                network_allowlist,
                memory_mb,
                vcpu_count,
            } => {
                self.create_sandbox(
                    sandbox_id,
                    template,
                    workspace_dir,
                    runtime,
                    network_mode,
                    network_allowlist,
                    memory_mb,
                    vcpu_count,
                )
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
                message: format!("{err:#}"),
            },
        }
    }

    async fn create_sandbox(
        &mut self,
        sandbox_id: String,
        template: String,
        workspace_dir: Option<String>,
        runtime: String,
        network_mode: Option<String>,
        network_allowlist: Option<Vec<String>>,
        memory_mb: Option<u32>,
        vcpu_count: Option<u32>,
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
        let network = HelperNetworkMode::parse(
            network_mode.as_deref().unwrap_or("none"),
            network_allowlist.unwrap_or_default(),
        )?;

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
        let allowlist_setup = match &network {
            HelperNetworkMode::Allowlist(domains) => Some(
                self.setup_allowlist_runtime(&sandbox_id, domains)
                    .await
                    .with_context(|| {
                        format!("prepare allowlist networking for sandbox `{sandbox_id}`")
                    })?,
            ),
            HelperNetworkMode::None | HelperNetworkMode::Full => None,
        };

        let container_name = format!("hyperbox-{}", sandbox_id);
        let start_result = self
            .start_container(
                &container_name,
                &template,
                &workspace_host,
                &network,
                allowlist_setup.as_ref(),
                memory_mb,
                vcpu_count,
            )
            .await;
        if let Err(err) = start_result {
            if let Some(runtime) = &allowlist_setup {
                self.teardown_allowlist_runtime(runtime).await;
            }
            return Err(err).with_context(|| {
                format!(
                    "start container runtime for sandbox `{sandbox_id}` using template `{template}`"
                )
            });
        }

        self.sandboxes.insert(
            sandbox_id.clone(),
            SandboxSession {
                container_name,
                workspace_host,
                ephemeral_workspace,
                allowlist_runtime: allowlist_setup.map(|setup| AllowlistRuntime {
                    network_name: setup.network_name,
                    dns_container_name: setup.dns_container_name,
                }),
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
        network: &HelperNetworkMode,
        allowlist_setup: Option<&AllowlistRuntimeSetup>,
        memory_mb: Option<u32>,
        vcpu_count: Option<u32>,
    ) -> anyhow::Result<()> {
        let cpus = vcpu_count.unwrap_or(1).max(1);
        let memory_mb = memory_mb.unwrap_or(512).max(1);
        let mount = format!("{}:{}", workspace_host.display(), WORKDIR_IN_CONTAINER);
        let mut args = vec![
            "run".to_string(),
            "--detach".to_string(),
            "--progress".to_string(),
            "none".to_string(),
            "--name".to_string(),
            container_name.to_string(),
            "--cpus".to_string(),
            cpus.to_string(),
            "--memory".to_string(),
            format!("{memory_mb}M"),
            "--workdir".to_string(),
            WORKDIR_IN_CONTAINER.to_string(),
            "--volume".to_string(),
            mount,
        ];
        args.extend(network.container_args(allowlist_setup)?);
        args.extend([
            template.to_string(),
            "sleep".to_string(),
            "infinity".to_string(),
        ]);

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

    async fn setup_allowlist_runtime(
        &self,
        sandbox_id: &str,
        domains: &[String],
    ) -> anyhow::Result<AllowlistRuntimeSetup> {
        let network_name = format!("{ALLOWLIST_NETWORK_PREFIX}{sandbox_id}");
        let dns_container_name = format!("{ALLOWLIST_DNS_PREFIX}{sandbox_id}");

        let create_network_result = self
            .run_container_command(
                vec![
                    "network".to_string(),
                    "create".to_string(),
                    network_name.clone(),
                ],
                None,
                DEFAULT_IO_TIMEOUT_SECS,
            )
            .await?;
        if create_network_result.exit_code != 0 {
            bail!(
                "create allowlist network failed (exit={}): {}",
                create_network_result.exit_code,
                String::from_utf8_lossy(&create_network_result.stderr)
            );
        }

        let setup_result = async {
            let gateway_ip = self.inspect_network_gateway(&network_name).await?;
            let allowlist_json =
                serde_json::to_string(domains).context("serialize allowlist domains")?;
            let allowlist_b64 = BASE64.encode(allowlist_json.as_bytes());
            let dns_script_b64 = BASE64.encode(ALLOWLIST_DNS_SCRIPT.as_bytes());

            let dns_run_result = self
                .run_container_command(
                    vec![
                        "run".to_string(),
                        "--detach".to_string(),
                        "--progress".to_string(),
                        "none".to_string(),
                        "--name".to_string(),
                        dns_container_name.clone(),
                        "--network".to_string(),
                        network_name.clone(),
                        "--cpus".to_string(),
                        "1".to_string(),
                        "--memory".to_string(),
                        "256M".to_string(),
                        "--env".to_string(),
                        format!("HB_ALLOWLIST_B64={allowlist_b64}"),
                        "--env".to_string(),
                        format!("HB_DNS_UPSTREAM={gateway_ip}"),
                        "--env".to_string(),
                        format!("HB_DNS_SCRIPT_B64={dns_script_b64}"),
                        ALLOWLIST_DNS_TEMPLATE.to_string(),
                        "python3".to_string(),
                        "-u".to_string(),
                        "-c".to_string(),
                        ALLOWLIST_DNS_BOOTSTRAP.to_string(),
                    ],
                    None,
                    DEFAULT_IO_TIMEOUT_SECS,
                )
                .await?;
            if dns_run_result.exit_code != 0 {
                bail!(
                    "start allowlist DNS sidecar failed (exit={}): {}",
                    dns_run_result.exit_code,
                    String::from_utf8_lossy(&dns_run_result.stderr)
                );
            }

            let dns_server_ip = self.inspect_container_ipv4(&dns_container_name).await?;
            Ok(AllowlistRuntimeSetup {
                network_name: network_name.clone(),
                dns_container_name: dns_container_name.clone(),
                dns_server_ip,
            })
        }
        .await;

        match setup_result {
            Ok(setup) => Ok(setup),
            Err(err) => {
                let _ = self.delete_container_if_present(&dns_container_name).await;
                let _ = self.delete_network_if_present(&network_name).await;
                Err(err)
            }
        }
    }

    async fn teardown_allowlist_runtime(&self, runtime: &AllowlistRuntimeSetup) {
        let _ = self
            .delete_container_if_present(&runtime.dns_container_name)
            .await;
        let _ = self.delete_network_if_present(&runtime.network_name).await;
    }

    async fn teardown_allowlist_runtime_session(&self, runtime: &AllowlistRuntime) {
        let _ = self
            .delete_container_if_present(&runtime.dns_container_name)
            .await;
        let _ = self.delete_network_if_present(&runtime.network_name).await;
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
        if result.exit_code != 0 && !error_output_is_not_found(&result.stderr) {
            bail!(
                "container delete failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }

        if let Some(runtime) = &session.allowlist_runtime {
            self.teardown_allowlist_runtime_session(runtime).await;
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

    async fn inspect_network_gateway(&self, network_name: &str) -> anyhow::Result<String> {
        let result = self
            .run_container_command(
                vec![
                    "network".to_string(),
                    "inspect".to_string(),
                    network_name.to_string(),
                ],
                None,
                DEFAULT_IO_TIMEOUT_SECS,
            )
            .await?;
        if result.exit_code != 0 {
            bail!(
                "inspect network `{network_name}` failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }

        let output: Value =
            serde_json::from_slice(&result.stdout).context("parse network inspect output")?;
        let gateway = output
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|entry| entry.get("status"))
            .and_then(|status| status.get("ipv4Gateway"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("network inspect output missing status.ipv4Gateway")?;
        Ok(gateway.to_string())
    }

    async fn inspect_container_ipv4(&self, container_name: &str) -> anyhow::Result<String> {
        let result = self
            .run_container_command(
                vec!["inspect".to_string(), container_name.to_string()],
                None,
                DEFAULT_IO_TIMEOUT_SECS,
            )
            .await?;
        if result.exit_code != 0 {
            bail!(
                "inspect container `{container_name}` failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }

        let output: Value =
            serde_json::from_slice(&result.stdout).context("parse container inspect output")?;
        let cidr = output
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|entry| entry.get("networks"))
            .and_then(Value::as_array)
            .and_then(|networks| networks.first())
            .and_then(|network| network.get("ipv4Address"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("container inspect output missing networks[0].ipv4Address")?;
        let ip = cidr
            .split('/')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("container inspect ipv4Address did not contain an IPv4 value")?;
        Ok(ip.to_string())
    }

    async fn delete_container_if_present(&self, container_name: &str) -> anyhow::Result<()> {
        let result = self
            .run_container_command(
                vec![
                    "delete".to_string(),
                    "--force".to_string(),
                    container_name.to_string(),
                ],
                None,
                DEFAULT_DESTROY_TIMEOUT_SECS,
            )
            .await?;
        if result.exit_code != 0 && !error_output_is_not_found(&result.stderr) {
            bail!(
                "container delete failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }
        Ok(())
    }

    async fn delete_network_if_present(&self, network_name: &str) -> anyhow::Result<()> {
        let result = self
            .run_container_command(
                vec![
                    "network".to_string(),
                    "delete".to_string(),
                    network_name.to_string(),
                ],
                None,
                DEFAULT_DESTROY_TIMEOUT_SECS,
            )
            .await?;
        if result.exit_code != 0 && !error_output_is_not_found(&result.stderr) {
            bail!(
                "network delete failed (exit={}): {}",
                result.exit_code,
                String::from_utf8_lossy(&result.stderr)
            );
        }
        Ok(())
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

impl HelperNetworkMode {
    fn parse(raw_mode: &str, allowlist: Vec<String>) -> anyhow::Result<Self> {
        match raw_mode.to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "full" => Ok(Self::Full),
            "allowlist" => {
                let normalized = normalize_allowlist_domains(&allowlist)
                    .map_err(|msg| anyhow!("invalid allowlist: {msg}"))?;
                Ok(Self::Allowlist(normalized))
            }
            other => bail!("unknown network mode `{other}`"),
        }
    }

    fn container_args(
        &self,
        allowlist_setup: Option<&AllowlistRuntimeSetup>,
    ) -> anyhow::Result<Vec<String>> {
        match self {
            Self::None => Ok(vec!["--network".to_string(), "none".to_string()]),
            Self::Full => Ok(vec![]),
            Self::Allowlist(_) => {
                let setup = allowlist_setup.context("allowlist networking runtime is missing")?;
                Ok(vec![
                    "--network".to_string(),
                    setup.network_name.clone(),
                    "--dns".to_string(),
                    setup.dns_server_ip.clone(),
                ])
            }
        }
    }
}

fn error_output_is_not_found(stderr: &[u8]) -> bool {
    let normalized = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    normalized.contains("not found") || normalized.contains("no such")
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
    use super::{
        AllowlistRuntimeSetup, HelperNetworkMode, RuntimeKind, normalize_relative_path,
        sanitize_sandbox_id,
    };

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

    #[test]
    fn helper_network_parser_accepts_none_and_full() {
        assert_eq!(
            HelperNetworkMode::parse("none", vec![]).expect("none mode"),
            HelperNetworkMode::None
        );
        assert_eq!(
            HelperNetworkMode::parse("full", vec![]).expect("full mode"),
            HelperNetworkMode::Full
        );
    }

    #[test]
    fn helper_network_parser_preserves_allowlist_mode() {
        let mode = HelperNetworkMode::parse("allowlist", vec!["pypi.org".to_string()])
            .expect("allowlist mode");
        assert!(matches!(mode, HelperNetworkMode::Allowlist(_)));
        assert!(mode.container_args(None).is_err());
    }

    #[test]
    fn helper_network_allowlist_uses_runtime_dns_and_network() {
        let mode = HelperNetworkMode::parse("allowlist", vec!["pypi.org".to_string()])
            .expect("allowlist mode");
        let setup = AllowlistRuntimeSetup {
            network_name: "hyperbox-net-test".to_string(),
            dns_container_name: "hyperbox-dns-test".to_string(),
            dns_server_ip: "192.168.64.10".to_string(),
        };
        let args = mode
            .container_args(Some(&setup))
            .expect("allowlist args should resolve");
        assert_eq!(
            args,
            vec![
                "--network".to_string(),
                "hyperbox-net-test".to_string(),
                "--dns".to_string(),
                "192.168.64.10".to_string()
            ]
        );
    }

    #[test]
    fn helper_network_parser_rejects_wildcard_allowlist_entries() {
        let err = HelperNetworkMode::parse("allowlist", vec!["*.example.com".to_string()])
            .expect_err("wildcards must be rejected");
        assert!(err.to_string().contains("wildcard"));
    }
}
