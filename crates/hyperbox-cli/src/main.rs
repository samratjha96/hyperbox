use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use hyperbox_core::{ExecRequest, FilePayload, NetworkMode, SandboxConfig};
use hyperbox_server::{GrpcControlClient, HyperboxServer, LocalBackend};

#[derive(Debug, Parser)]
#[command(name = "hyperbox", version, about = "Secure sandbox runtime for agent code execution")]
struct Cli {
    #[arg(long)]
    server_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run {
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
        #[arg(long = "write")]
        writes: Vec<String>,
        #[arg(long = "read")]
        reads: Vec<String>,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    Templates,
    Probe,
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
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            template,
            cmd,
            network,
            allow,
            timeout,
            writes,
            reads,
            json,
        } => {
            let config = SandboxConfig {
                template,
                network: network.to_mode(allow),
                timeout_secs: timeout,
                ..SandboxConfig::default()
            };

            if let Some(server_url) = cli.server_url {
                run_remote(server_url, config, cmd, timeout, writes, reads, json).await?;
            } else {
                run_local(config, cmd, timeout, writes, reads, json).await?;
            }
        }
        Command::Templates => {
            if let Some(server_url) = cli.server_url {
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
    }

    Ok(())
}

async fn run_local(
    config: SandboxConfig,
    cmd: String,
    timeout: u64,
    writes: Vec<String>,
    reads: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let backend = Arc::new(LocalBackend::new(None));
    let server = HyperboxServer::new(backend);
    let sandbox = server.create_sandbox(config).await?;

    for entry in writes {
        let (path, content) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --write value, expected PATH=CONTENT"))?;
        server
            .write_file(
                &sandbox.id,
                FilePayload {
                    path: path.to_string().into(),
                    bytes: content.as_bytes().to_vec(),
                },
            )
            .await?;
    }

    let outcome = server
        .exec(
            &sandbox.id,
            ExecRequest {
                command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd],
                timeout_secs: timeout,
            },
        )
        .await?;

    let mut artifacts = Vec::new();
    for path in reads {
        let payload = server.read_file(&sandbox.id, &path).await?;
        artifacts.push((path, String::from_utf8_lossy(&payload.bytes).to_string()));
    }

    emit_result(outcome, artifacts, json)?;
    server.destroy_sandbox(&sandbox.id).await?;
    Ok(())
}

async fn run_remote(
    server_url: String,
    config: SandboxConfig,
    cmd: String,
    timeout: u64,
    writes: Vec<String>,
    reads: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let mut client = GrpcControlClient::connect(server_url).await?;
    let sandbox = client.create_sandbox(config).await?;

    for entry in writes {
        let (path, content) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --write value, expected PATH=CONTENT"))?;
        client
            .write_file(&sandbox.id, path.to_string(), content.as_bytes().to_vec())
            .await?;
    }

    let outcome = client
        .exec(
            &sandbox.id,
            ExecRequest {
                command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd],
                timeout_secs: timeout,
            },
        )
        .await?;

    let mut artifacts = Vec::new();
    for path in reads {
        let bytes = client.read_file(&sandbox.id, path.clone()).await?;
        artifacts.push((path, String::from_utf8_lossy(&bytes).to_string()));
    }

    emit_result(outcome, artifacts, json)?;
    client.destroy_sandbox(&sandbox.id).await?;
    Ok(())
}

fn emit_result(
    outcome: hyperbox_core::ExecOutcome,
    artifacts: Vec<(String, String)>,
    json: bool,
) -> anyhow::Result<()> {
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

    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code);
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct RunResponse {
    exit_code: i32,
    duration_ms: u128,
    stdout: String,
    stderr: String,
    artifacts: Vec<(String, String)>,
}
