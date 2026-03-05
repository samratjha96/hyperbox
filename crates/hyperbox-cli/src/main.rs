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
    Templates {
        #[arg(long)]
        disk_root: Option<String>,
    },
    Probe,
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
