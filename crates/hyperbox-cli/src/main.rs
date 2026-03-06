use std::{
    io::IsTerminal,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time::sleep,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use hyperbox_core::{ExecRequest, NetworkMode, SandboxConfig, SandboxId};
use hyperbox_proto::hyperbox::v1::{
    self as pb, hyperbox_agent_client::HyperboxAgentClient, shell_event, shell_request,
};
use hyperbox_server::{GrpcControlClient, HyperboxServer, LocalBackend, serve_grpc};

mod apple_helper;
mod setup;

const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:50051";
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:50051";
const SERVER_STARTUP_RETRIES: usize = 20;
const SERVER_STARTUP_DELAY_MS: u64 = 150;

#[derive(Debug, Parser)]
#[command(
    name = "hyperbox",
    version,
    about = "Secure sandbox runtime for agent code execution"
)]
struct Cli {
    #[arg(long)]
    server_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        sandbox_id: Option<String>,
        #[arg(long, default_value = "python:3.12")]
        template: String,
        #[arg(long)]
        cmd: String,
        #[arg(long, value_enum, default_value_t = NetworkArg::None)]
        network: NetworkArg,
        #[arg(long = "allow")]
        allow: Vec<String>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long, conflicts_with = "sandbox_id")]
        workspace: Option<String>,
        #[arg(long = "write")]
        writes: Vec<String>,
        #[arg(long = "read")]
        reads: Vec<String>,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    Create {
        #[arg(long, default_value = "python:3.12")]
        template: String,
        #[arg(long, value_enum, default_value_t = NetworkArg::None)]
        network: NetworkArg,
        #[arg(long = "allow")]
        allow: Vec<String>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long, default_value_t = 512)]
        memory_mb: u32,
        #[arg(long, default_value_t = 1)]
        vcpu_count: u8,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    Destroy {
        #[arg(long)]
        sandbox_id: String,
    },
    Inspect {
        #[arg(long)]
        sandbox_id: String,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    Shell {
        #[arg(long)]
        sandbox_id: Option<String>,
        #[arg(long, default_value = "/bin/sh")]
        shell: String,
        #[arg(long, conflicts_with = "sandbox_id")]
        template: Option<String>,
        #[arg(long, conflicts_with = "sandbox_id")]
        workspace: Option<String>,
        #[arg(long, value_enum, conflicts_with = "sandbox_id")]
        network: Option<NetworkArg>,
        #[arg(long = "allow", conflicts_with = "sandbox_id")]
        allow: Vec<String>,
    },
    Templates {
        #[arg(long)]
        disk_root: Option<String>,
    },
    Serve {
        #[arg(long, default_value = DEFAULT_SERVER_ADDR)]
        addr: String,
    },
    Probe,
    Setup,
    Proxy {
        #[arg(long, default_value = "python:3.12")]
        template: String,
        #[arg(long, value_enum, default_value_t = NetworkArg::None)]
        network: NetworkArg,
        #[arg(long = "allow")]
        allow: Vec<String>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long)]
        workspace: Option<String>,
    },
    AppleHelper {
        #[arg(long, default_value = "container")]
        container_bin: String,
        #[arg(long)]
        state_root: Option<String>,
    },
    Bench {
        #[arg(long, default_value = "python:3.12")]
        template: String,
        #[arg(long)]
        cmd: String,
        #[arg(long, default_value_t = 20)]
        runs: usize,
        #[arg(long, default_value_t = 3)]
        warmup: usize,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NetworkArg {
    None,
    Full,
    Allowlist,
}

impl NetworkArg {
    fn to_mode(self, allow: Vec<String>) -> NetworkMode {
        match self {
            Self::None => NetworkMode::None,
            Self::Full => NetworkMode::Full,
            Self::Allowlist => NetworkMode::Allowlist(allow),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            sandbox_id,
            template,
            cmd,
            network,
            allow,
            timeout,
            workspace,
            writes,
            reads,
            json,
        } => {
            if let Some(sandbox_id) = sandbox_id {
                run_existing_remote(
                    cli.server_url,
                    sandbox_id,
                    cmd,
                    timeout,
                    writes,
                    reads,
                    json,
                )
                .await?;
                return Ok(());
            }

            let config = SandboxConfig {
                template,
                network: network.to_mode(allow),
                timeout_secs: timeout,
                workspace_dir: workspace,
                ..SandboxConfig::default()
            };

            run_remote(cli.server_url, config, cmd, timeout, writes, reads, json).await?;
        }
        Command::Create {
            template,
            network,
            allow,
            timeout,
            memory_mb,
            vcpu_count,
            workspace,
            json,
        } => {
            let mut client = connect_client(cli.server_url, true).await?;
            let info = client
                .create_sandbox(SandboxConfig {
                    template,
                    memory_mb,
                    vcpu_count,
                    network: network.to_mode(allow),
                    timeout_secs: timeout,
                    workspace_dir: workspace,
                    ..SandboxConfig::default()
                })
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&SandboxInfoResponse {
                        sandbox_id: info.id.0.to_string(),
                        template: info.template,
                        state: format!("{:?}", info.state).to_lowercase(),
                        created_at: info.created_at.to_rfc3339(),
                    })?
                );
            } else {
                println!("{}", info.id.0);
            }
        }
        Command::Destroy { sandbox_id } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let sandbox_id = parse_sandbox_id(&sandbox_id)?;
            client.destroy_sandbox(&sandbox_id).await?;
        }
        Command::Inspect { sandbox_id, json } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let sandbox_id = parse_sandbox_id(&sandbox_id)?;
            let info = client.inspect_sandbox(&sandbox_id).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&SandboxInfoResponse {
                        sandbox_id: info.id.0.to_string(),
                        template: info.template,
                        state: format!("{:?}", info.state).to_lowercase(),
                        created_at: info.created_at.to_rfc3339(),
                    })?
                );
            } else {
                println!("{}", info.id.0);
            }
        }
        Command::Shell {
            sandbox_id,
            shell,
            template,
            workspace,
            network,
            allow,
        } => {
            let exit_code = shell_command(
                cli.server_url,
                sandbox_id,
                &shell,
                template.unwrap_or_else(|| "python:3.12".to_string()),
                workspace,
                network.unwrap_or(NetworkArg::None).to_mode(allow),
            )
            .await?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Command::Templates { disk_root } => {
            if let Some(root) = disk_root {
                for manifest in hyperbox_core::load_template_manifests(std::path::Path::new(&root))?
                {
                    println!(
                        "{}\t{}\t{}",
                        manifest.name, manifest.rootfs, manifest.description
                    );
                }
            } else if let Some(server_url) = cli.server_url {
                let mut client = GrpcControlClient::connect(server_url).await?;
                for template in client.list_templates().await? {
                    println!("{template}");
                }
            } else {
                let backend = Arc::new(LocalBackend::new(None));
                let server = HyperboxServer::new(backend);
                for template in server.templates() {
                    println!("{template}");
                }
            }
        }
        Command::Serve { addr } => {
            let addr: std::net::SocketAddr = addr.parse()?;
            serve_grpc(addr).await?;
        }
        Command::Probe => {
            let os = std::env::consts::OS;
            if os == "linux" {
                let caps = hyperbox_firecracker::detect_linux_capabilities();
                println!("{}", serde_json::to_string_pretty(&caps)?);
            } else if os == "macos" {
                let caps = hyperbox_apple::detect_macos_capabilities();
                println!("{}", serde_json::to_string_pretty(&caps)?);
            } else {
                println!(
                    "{}",
                    serde_json::json!({
                        "os": os,
                        "supported": false,
                        "message": "unsupported host for vm backend"
                    })
                );
            }
        }
        Command::Setup => {
            setup::run_setup()?;
        }
        Command::Proxy {
            template,
            network,
            allow,
            timeout,
            workspace,
        } => {
            run_proxy_loop(
                cli.server_url,
                SandboxConfig {
                    template,
                    network: network.to_mode(allow),
                    timeout_secs: timeout,
                    workspace_dir: workspace,
                    ..SandboxConfig::default()
                },
            )
            .await?;
        }
        Command::AppleHelper {
            container_bin,
            state_root,
        } => {
            let state_root = state_root
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("hyperbox-apple-helper"));
            apple_helper::run(apple_helper::AppleHelperConfig {
                container_bin,
                state_root,
            })
            .await?;
        }
        Command::Bench {
            template,
            cmd,
            runs,
            warmup,
            json,
        } => {
            let config = SandboxConfig {
                template,
                ..SandboxConfig::default()
            };
            let summary = if let Some(server_url) = cli.server_url {
                bench_remote(server_url, config, cmd, warmup, runs).await?
            } else {
                bench_local(config, cmd, warmup, runs).await?
            };

            if json {
                println!("{}", serde_json::to_string(&summary)?);
            } else {
                println!(
                    "runs={} warmup={} mean_ms={:.2} p50_ms={} p95_ms={} min_ms={} max_ms={}",
                    summary.runs,
                    summary.warmup,
                    summary.mean_ms,
                    summary.p50_ms,
                    summary.p95_ms,
                    summary.min_ms,
                    summary.max_ms
                );
            }
        }
    }

    Ok(())
}

fn parse_sandbox_id(raw: &str) -> anyhow::Result<SandboxId> {
    let id = uuid::Uuid::parse_str(raw)
        .with_context(|| format!("invalid sandbox id `{raw}`: expected UUID"))?;
    Ok(SandboxId(id))
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("hyperbox=warn,hyperbox_server=warn,hyperbox_apple=warn")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .try_init();
}

fn spawn_local_server() -> anyhow::Result<()> {
    let current_exe = std::env::current_exe().context("resolve current executable path")?;
    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("serve")
        .arg("--addr")
        .arg(DEFAULT_SERVER_ADDR)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn().context("auto-start local server process")?;
    Ok(())
}

async fn connect_client(
    server_url: Option<String>,
    autostart_default: bool,
) -> anyhow::Result<GrpcControlClient> {
    let url = server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    if let Ok(client) = GrpcControlClient::connect(url.clone()).await {
        return Ok(client);
    }

    if autostart_default && url == DEFAULT_SERVER_URL {
        spawn_local_server()?;
        for _ in 0..SERVER_STARTUP_RETRIES {
            sleep(Duration::from_millis(SERVER_STARTUP_DELAY_MS)).await;
            if let Ok(client) = GrpcControlClient::connect(url.clone()).await {
                return Ok(client);
            }
        }
    }

    GrpcControlClient::connect(url.clone())
        .await
        .with_context(|| format!("failed to connect to hyperbox control plane at {url}"))
}

async fn run_remote(
    server_url: Option<String>,
    config: SandboxConfig,
    cmd: String,
    timeout: u64,
    writes: Vec<String>,
    reads: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let op_started = Instant::now();
    let autostart_default = server_url.is_none();
    let mut client = connect_client(server_url, autostart_default).await?;
    let create_started = Instant::now();
    let sandbox = client.create_sandbox(config).await?;
    info!(
        sandbox_id = %sandbox.id.0,
        stage = "create",
        elapsed_ms = create_started.elapsed().as_millis() as u64,
        "run command sandbox created"
    );

    let run_result: anyhow::Result<i32> = async {
        for entry in writes {
            let (path, content) = entry
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid --write value, expected PATH=CONTENT"))?;
            client
                .write_file(&sandbox.id, path.to_string(), content.as_bytes().to_vec())
                .await?;
        }

        let exec_started = Instant::now();
        let outcome = client
            .exec(
                &sandbox.id,
                ExecRequest {
                    command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd],
                    timeout_secs: timeout,
                },
            )
            .await?;
        info!(
            sandbox_id = %sandbox.id.0,
            stage = "exec",
            elapsed_ms = exec_started.elapsed().as_millis() as u64,
            exec_duration_ms = outcome.duration_ms as u64,
            exit_code = outcome.exit_code,
            "run command execution completed"
        );

        let mut artifacts = Vec::new();
        for path in reads {
            let bytes = client.read_file(&sandbox.id, path.clone()).await?;
            artifacts.push((path, String::from_utf8_lossy(&bytes).to_string()));
        }

        emit_result(outcome, artifacts, json)
    }
    .await;

    let destroy_started = Instant::now();
    let destroy_result = client.destroy_sandbox(&sandbox.id).await;
    if let Err(err) = destroy_result {
        warn!(
            sandbox_id = %sandbox.id.0,
            stage = "destroy",
            elapsed_ms = destroy_started.elapsed().as_millis() as u64,
            error = %err,
            "run command sandbox destroy failed"
        );
        if run_result.is_ok() {
            return Err(err);
        }
    }
    info!(
        sandbox_id = %sandbox.id.0,
        stage = "destroy",
        elapsed_ms = destroy_started.elapsed().as_millis() as u64,
        total_elapsed_ms = op_started.elapsed().as_millis() as u64,
        "run command sandbox destroyed"
    );

    let exit_code = run_result?;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

async fn run_existing_remote(
    server_url: Option<String>,
    sandbox_id: String,
    cmd: String,
    timeout: u64,
    writes: Vec<String>,
    reads: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let autostart_default = server_url.is_none();
    let mut client = connect_client(server_url, autostart_default).await?;
    let sandbox_id = parse_sandbox_id(&sandbox_id)?;

    for entry in writes {
        let (path, content) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --write value, expected PATH=CONTENT"))?;
        client
            .write_file(&sandbox_id, path.to_string(), content.as_bytes().to_vec())
            .await?;
    }

    let outcome = client
        .exec(
            &sandbox_id,
            ExecRequest {
                command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd],
                timeout_secs: timeout,
            },
        )
        .await?;

    let mut artifacts = Vec::new();
    for path in reads {
        let bytes = client.read_file(&sandbox_id, path.clone()).await?;
        artifacts.push((path, String::from_utf8_lossy(&bytes).to_string()));
    }

    let exit_code = emit_result(outcome, artifacts, json)?;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

async fn shell_command(
    server_url: Option<String>,
    sandbox_id: Option<String>,
    shell: &str,
    template: String,
    workspace: Option<String>,
    network: NetworkMode,
) -> anyhow::Result<i32> {
    if let Some(sandbox_id) = sandbox_id {
        return open_shell(server_url, &sandbox_id, shell).await;
    }

    let workspace_dir = match workspace {
        Some(workspace) => Some(workspace),
        None => Some(
            std::env::current_dir()
                .context("resolve current directory for shell workspace")?
                .to_string_lossy()
                .to_string(),
        ),
    };

    let autostart_default = server_url.is_none();
    let mut client = connect_client(server_url, autostart_default).await?;
    info!(
        template = %template,
        workspace = ?workspace_dir,
        "creating ephemeral shell sandbox"
    );
    let create_started = Instant::now();
    let sandbox = client
        .create_sandbox(SandboxConfig {
            template,
            network,
            workspace_dir,
            ..SandboxConfig::default()
        })
        .await?;
    info!(
        sandbox_id = %sandbox.id.0,
        stage = "create",
        elapsed_ms = create_started.elapsed().as_millis() as u64,
        "ephemeral shell sandbox created"
    );

    let attach_started = Instant::now();
    let shell_result = open_shell_with_client(&mut client, &sandbox.id, shell).await;
    info!(
        sandbox_id = %sandbox.id.0,
        stage = "attach",
        elapsed_ms = attach_started.elapsed().as_millis() as u64,
        "ephemeral shell session ended"
    );
    let destroy_started = Instant::now();
    let destroy_result = client.destroy_sandbox(&sandbox.id).await;
    match &destroy_result {
        Ok(()) => info!(
            sandbox_id = %sandbox.id.0,
            stage = "destroy",
            elapsed_ms = destroy_started.elapsed().as_millis() as u64,
            "ephemeral shell sandbox destroyed"
        ),
        Err(err) => warn!(
            sandbox_id = %sandbox.id.0,
            stage = "destroy",
            elapsed_ms = destroy_started.elapsed().as_millis() as u64,
            error = %err,
            "failed to destroy ephemeral shell sandbox"
        ),
    }

    match (shell_result, destroy_result) {
        (Ok(code), Ok(())) => Ok(code),
        (Ok(_), Err(err)) => Err(err).with_context(|| {
            format!(
                "failed to destroy ephemeral sandbox {} after shell exit",
                sandbox.id.0
            )
        }),
        (Err(shell_err), Ok(())) => Err(shell_err),
        (Err(shell_err), Err(destroy_err)) => Err(anyhow::anyhow!(
            "shell failed: {shell_err}; additionally failed to destroy ephemeral sandbox {}: {destroy_err}",
            sandbox.id.0
        )),
    }
}

async fn open_shell(
    server_url: Option<String>,
    sandbox_id: &str,
    shell: &str,
) -> anyhow::Result<i32> {
    let autostart_default = server_url.is_none();
    let mut client = connect_client(server_url, autostart_default).await?;
    let sandbox_id = parse_sandbox_id(sandbox_id)?;
    open_shell_with_client(&mut client, &sandbox_id, shell).await
}

async fn open_shell_with_client(
    client: &mut GrpcControlClient,
    sandbox_id: &SandboxId,
    shell: &str,
) -> anyhow::Result<i32> {
    let _ = client.inspect_sandbox(&sandbox_id).await?;
    let server_info = match client.get_server_info().await {
        Ok(info) => info,
        Err(err) => {
            if err.to_string().contains("Unimplemented") {
                bail!(
                    "server does not support GetServerInfo (likely stale daemon). Restart hyperbox server and retry `hyperbox shell`."
                );
            }
            return Err(err);
        }
    };
    info!(
        sandbox_id = %sandbox_id.0,
        backend = %server_info.backend_selected,
        "opening interactive shell"
    );
    debug!(
        sandbox_id = %sandbox_id.0,
        backend_reason = %server_info.backend_reason,
        helper_argv = ?server_info.apple_helper_argv,
        "resolved shell backend details"
    );

    if server_info.backend_selected == "firecracker" {
        return open_shell_via_agent_stream(&sandbox_id, shell).await;
    }
    if server_info.backend_selected == "local" {
        return open_shell_local(client, &sandbox_id, shell).await;
    }
    if server_info.backend_selected != "apple" {
        bail!(
            "interactive shell is not supported for backend `{}`",
            server_info.backend_selected
        );
    }

    if !helper_argv_is_builtin_apple_helper(&server_info.apple_helper_argv) {
        bail!(
            "interactive shell is currently supported only for built-in apple helper sessions; active helper command is `{}`",
            server_info.apple_helper_argv.join(" ")
        );
    }

    let container_bin = extract_container_bin_from_helper_argv(&server_info.apple_helper_argv)
        .unwrap_or_else(|| "container".to_string());
    let mut args = vec![
        "exec".to_string(),
        "--interactive".to_string(),
        "--workdir".to_string(),
        "/workspace".to_string(),
    ];
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        args.push("--tty".to_string());
    }
    args.push(format!("hyperbox-{}", sandbox_id.0));
    args.push(shell.to_string());

    let status = std::process::Command::new(&container_bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| {
            format!(
                "launch interactive shell with `{container_bin}` into sandbox {}",
                sandbox_id.0
            )
        })?;

    Ok(status.code().unwrap_or(1))
}

async fn open_shell_local(
    client: &mut GrpcControlClient,
    sandbox_id: &SandboxId,
    shell: &str,
) -> anyhow::Result<i32> {
    let probe = client
        .exec(
            sandbox_id,
            ExecRequest {
                command: vec!["/bin/sh".to_string(), "-lc".to_string(), "pwd".to_string()],
                timeout_secs: 10,
            },
        )
        .await
        .context("probe sandbox working directory")?;
    if probe.exit_code != 0 {
        bail!(
            "failed to resolve local sandbox working directory: {}",
            probe.stderr
        );
    }
    let workdir = probe.stdout.lines().next().unwrap_or_default().trim();
    if workdir.is_empty() {
        bail!("failed to resolve local sandbox working directory: empty output");
    }

    let status = std::process::Command::new(shell)
        .current_dir(workdir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("launch local interactive shell in `{workdir}`"))?;
    Ok(status.code().unwrap_or(1))
}

async fn open_shell_via_agent_stream(sandbox_id: &SandboxId, shell: &str) -> anyhow::Result<i32> {
    let endpoint = std::env::var("HYPERBOX_AGENT_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:60061".to_string());
    let mut agent = HyperboxAgentClient::connect(endpoint.clone())
        .await
        .with_context(|| format!("connect hyperbox agent at {endpoint}"))?;

    let (tx, rx) = mpsc::channel::<pb::ShellRequest>(64);
    tx.send(pb::ShellRequest {
        payload: Some(shell_request::Payload::Open(pb::ShellOpenRequest {
            sandbox_id: sandbox_id.0.to_string(),
            command: vec![shell.to_string()],
        })),
    })
    .await
    .context("send shell open request")?;
    let response = agent
        .shell(ReceiverStream::new(rx))
        .await
        .context("open agent shell stream")?;
    let mut stream = response.into_inner();

    let stdin_tx = tx;
    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            let read = stdin.read(&mut buf).await?;
            if read == 0 {
                break;
            }
            if stdin_tx
                .send(pb::ShellRequest {
                    payload: Some(shell_request::Payload::Stdin(buf[..read].to_vec())),
                })
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = stdin_tx
            .send(pb::ShellRequest {
                payload: Some(shell_request::Payload::Close(pb::ShellCloseRequest {})),
            })
            .await;
        anyhow::Ok(())
    });

    info!(
        sandbox_id = %sandbox_id.0,
        shell = %shell,
        "attached shell via agent stream"
    );
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    while let Some(event) = stream.message().await.context("read shell stream event")? {
        match event.payload {
            Some(shell_event::Payload::Stdout(bytes)) => {
                stdout.write_all(&bytes).await?;
                stdout.flush().await?;
            }
            Some(shell_event::Payload::Stderr(bytes)) => {
                stderr.write_all(&bytes).await?;
                stderr.flush().await?;
            }
            Some(shell_event::Payload::Error(message)) => {
                stderr.write_all(message.as_bytes()).await?;
                stderr.write_all(b"\n").await?;
                stderr.flush().await?;
            }
            Some(shell_event::Payload::ExitCode(code)) => {
                stdin_task.abort();
                return Ok(code);
            }
            None => {}
        }
    }

    stdin_task.abort();
    bail!("shell stream ended before receiving exit code")
}

fn helper_argv_is_builtin_apple_helper(argv: &[String]) -> bool {
    argv.len() >= 2 && argv[1] == "apple-helper"
}

fn extract_container_bin_from_helper_argv(argv: &[String]) -> Option<String> {
    let mut idx = 0usize;
    while idx < argv.len() {
        if argv[idx] == "--container-bin" {
            return argv.get(idx + 1).cloned();
        }
        idx += 1;
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ProxyRequest {
    Exec { cmd: String, timeout: Option<u64> },
    Read { path: String },
    Write { path: String, content: String },
    Destroy,
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ProxyResponse {
    Exec {
        exit_code: i32,
        duration_ms: u128,
        stdout: String,
        stderr: String,
    },
    Read {
        path: String,
        content: String,
    },
    Write,
    Destroy,
    Pong,
    Error {
        message: String,
    },
}

async fn run_proxy_loop(server_url: Option<String>, config: SandboxConfig) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut client = connect_client(server_url, true).await?;
    let sandbox = client.create_sandbox(config).await?;
    let sandbox_id = sandbox.id;

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<ProxyRequest>(&line) {
            Ok(ProxyRequest::Exec { cmd, timeout }) => match client
                .exec(
                    &sandbox_id,
                    ExecRequest {
                        command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd],
                        timeout_secs: timeout.unwrap_or(60),
                    },
                )
                .await
            {
                Ok(outcome) => ProxyResponse::Exec {
                    exit_code: outcome.exit_code,
                    duration_ms: outcome.duration_ms,
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                },
                Err(err) => ProxyResponse::Error {
                    message: err.to_string(),
                },
            },
            Ok(ProxyRequest::Read { path }) => {
                match client.read_file(&sandbox_id, path.clone()).await {
                    Ok(bytes) => ProxyResponse::Read {
                        path,
                        content: String::from_utf8_lossy(&bytes).to_string(),
                    },
                    Err(err) => ProxyResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(ProxyRequest::Write { path, content }) => {
                match client
                    .write_file(&sandbox_id, path, content.as_bytes().to_vec())
                    .await
                {
                    Ok(()) => ProxyResponse::Write,
                    Err(err) => ProxyResponse::Error {
                        message: err.to_string(),
                    },
                }
            }
            Ok(ProxyRequest::Destroy) => {
                let response = match client.destroy_sandbox(&sandbox_id).await {
                    Ok(()) => ProxyResponse::Destroy,
                    Err(err) => ProxyResponse::Error {
                        message: err.to_string(),
                    },
                };
                let encoded = serde_json::to_string(&response)?;
                stdout.write_all(encoded.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                return Ok(());
            }
            Ok(ProxyRequest::Ping) => ProxyResponse::Pong,
            Err(err) => ProxyResponse::Error {
                message: format!("invalid request: {err}"),
            },
        };

        let encoded = serde_json::to_string(&response)?;
        stdout.write_all(encoded.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }

    let _ = client.destroy_sandbox(&sandbox_id).await;
    Ok(())
}

fn emit_result(
    outcome: hyperbox_core::ExecOutcome,
    artifacts: Vec<(String, String)>,
    json: bool,
) -> anyhow::Result<i32> {
    if json {
        let response = RunResponse {
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration_ms,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            artifacts,
        };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        print!("{}", outcome.stdout);
        eprint!("{}", outcome.stderr);
        for (path, data) in &artifacts {
            println!("--- {path} ---");
            print!("{data}");
            if !data.ends_with('\n') {
                println!();
            }
        }
    }

    Ok(outcome.exit_code)
}

#[derive(Debug, Serialize)]
struct RunResponse {
    exit_code: i32,
    duration_ms: u128,
    stdout: String,
    stderr: String,
    artifacts: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct SandboxInfoResponse {
    sandbox_id: String,
    template: String,
    state: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct BenchSummary {
    runs: usize,
    warmup: usize,
    mean_ms: f64,
    p50_ms: u128,
    p95_ms: u128,
    min_ms: u128,
    max_ms: u128,
}

async fn bench_local(
    config: SandboxConfig,
    cmd: String,
    warmup: usize,
    runs: usize,
) -> anyhow::Result<BenchSummary> {
    let backend = Arc::new(LocalBackend::new(None));
    let server = HyperboxServer::new(backend);
    let mut samples = Vec::with_capacity(runs);

    for i in 0..(warmup + runs) {
        let sandbox = server.create_sandbox(config.clone()).await?;
        let outcome = server
            .exec(
                &sandbox.id,
                ExecRequest {
                    command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd.clone()],
                    timeout_secs: 60,
                },
            )
            .await?;
        server.destroy_sandbox(&sandbox.id).await?;
        if i >= warmup {
            samples.push(outcome.duration_ms);
        }
    }

    Ok(summarize_samples(samples, warmup))
}

async fn bench_remote(
    server_url: String,
    config: SandboxConfig,
    cmd: String,
    warmup: usize,
    runs: usize,
) -> anyhow::Result<BenchSummary> {
    let mut client = GrpcControlClient::connect(server_url).await?;
    let mut samples = Vec::with_capacity(runs);

    for i in 0..(warmup + runs) {
        let sandbox = client.create_sandbox(config.clone()).await?;
        let outcome = client
            .exec(
                &sandbox.id,
                ExecRequest {
                    command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd.clone()],
                    timeout_secs: 60,
                },
            )
            .await?;
        client.destroy_sandbox(&sandbox.id).await?;
        if i >= warmup {
            samples.push(outcome.duration_ms);
        }
    }

    Ok(summarize_samples(samples, warmup))
}

fn summarize_samples(mut samples: Vec<u128>, warmup: usize) -> BenchSummary {
    samples.sort_unstable();
    let runs = samples.len();
    let sum: u128 = samples.iter().copied().sum();
    let mean_ms = if runs == 0 {
        0.0
    } else {
        (sum as f64) / (runs as f64)
    };

    BenchSummary {
        runs,
        warmup,
        mean_ms,
        p50_ms: percentile(&samples, 50),
        p95_ms: percentile(&samples, 95),
        min_ms: samples.first().copied().unwrap_or_default(),
        max_ms: samples.last().copied().unwrap_or_default(),
    }
}

fn percentile(values: &[u128], p: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let rank = ((values.len() * p).div_ceil(100)).saturating_sub(1);
    values[rank]
}

#[cfg(test)]
mod tests {
    use super::{extract_container_bin_from_helper_argv, helper_argv_is_builtin_apple_helper};

    #[test]
    fn parses_container_bin_from_helper_args() {
        let argv = vec![
            "hyperbox".to_string(),
            "apple-helper".to_string(),
            "--container-bin".to_string(),
            "/opt/bin/container".to_string(),
        ];
        assert_eq!(
            extract_container_bin_from_helper_argv(&argv),
            Some("/opt/bin/container".to_string())
        );
    }

    #[test]
    fn detects_builtin_helper_argv_shape() {
        assert!(helper_argv_is_builtin_apple_helper(&[
            "hyperbox".to_string(),
            "apple-helper".to_string()
        ]));
        assert!(!helper_argv_is_builtin_apple_helper(&[
            "custom-helper".to_string(),
            "--foo".to_string()
        ]));
    }
}
