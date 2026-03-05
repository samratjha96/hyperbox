use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use hyperbox_core::{ExecRequest, NetworkMode, SandboxConfig};
use hyperbox_server::{HyperboxServer, LocalBackend};

#[derive(Debug, Parser)]
#[command(name = "hyperbox", version, about = "Secure sandbox runtime for agent code execution")]
struct Cli {
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
    let backend = Arc::new(LocalBackend::new(None));
    let server = HyperboxServer::new(backend);

    match cli.command {
        Command::Run {
            template,
            cmd,
            network,
            allow,
            timeout,
        } => {
            let config = SandboxConfig {
                template,
                network: network.to_mode(allow),
                timeout_secs: timeout,
                ..SandboxConfig::default()
            };

            let sandbox = server.create_sandbox(config).await?;
            let outcome = server
                .exec(
                    &sandbox.id,
                    ExecRequest {
                        command: vec!["/bin/sh".to_string(), "-lc".to_string(), cmd],
                        timeout_secs: timeout,
                    },
                )
                .await?;

            print!("{}", outcome.stdout);
            eprint!("{}", outcome.stderr);
            server.destroy_sandbox(&sandbox.id).await?;

            if outcome.exit_code != 0 {
                std::process::exit(outcome.exit_code);
            }
        }
        Command::Templates => {
            for template in server.templates() {
                println!("{template}");
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
