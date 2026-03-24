use std::{
    io::IsTerminal,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
    time::sleep,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use hyperbox_core::{
    Allowlist, ExecRequest, NetworkMode, ProcessDisposition, ProcessId, ProcessInfo, SandboxConfig,
    SandboxId, SnapshotId, StreamName,
};
use hyperbox_proto::hyperbox::v1::{
    self as pb, hyperbox_agent_client::HyperboxAgentClient, shell_event, shell_request,
};
use hyperbox_server::{GrpcControlClient, HyperboxServer, LocalBackend, ServerInfo, serve_grpc};

mod apple_helper;
mod setup;
mod template_auto;

const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:50051";
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:50051";
const SERVER_STARTUP_RETRIES: usize = 20;
const SERVER_STARTUP_DELAY_MS: u64 = 150;

#[derive(Debug, Parser)]
#[command(
    name = "hyperbox",
    version,
    about = "Secure sandbox runtime for agent code execution",
    long_about = "Secure sandbox runtime for agent code execution.\n\nMost users start with `hyperbox run --cmd \"...\"`.\nFor persistent environments use `create` + `run --sandbox-id/--name`.\nUse `setup` once on macOS to install runtime prerequisites."
)]
struct Cli {
    #[arg(
        long,
        value_name = "URL",
        help = "Control-plane server URL (default: auto-start local server at http://127.0.0.1:50051)"
    )]
    server_url: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Path to profile config TOML (default: ~/.hyperbox/profiles.toml)"
    )]
    profile_config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a command in a sandbox.
    Run {
        #[arg(long, value_name = "ID", help = "Run in an existing sandbox by id")]
        sandbox_id: Option<String>,
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with = "sandbox_id",
            help = "Run in an existing named sandbox (affinity)"
        )]
        name: Option<String>,
        #[arg(
            long,
            default_value = "auto",
            help = "Template image for new sandbox creation (`auto` detects from command/workspace)"
        )]
        template: String,
        #[arg(
            long,
            value_name = "COMMAND",
            help = "Shell command to execute inside the sandbox"
        )]
        cmd: String,
        #[arg(long, value_enum, help = "Network mode for new sandbox creation")]
        network: Option<NetworkArg>,
        #[arg(
            long = "allow",
            value_name = "DOMAIN",
            help = "Allowlisted domain or wildcard subdomain pattern (repeat for multiple entries, only with allowlist mode)"
        )]
        allow: Vec<String>,
        #[arg(
            long,
            value_name = "NAME",
            help = "Profile name (built-ins: locked, web, full; custom from --profile-config)"
        )]
        profile: Option<String>,
        #[arg(long, default_value_t = 60, help = "Command timeout in seconds")]
        timeout: u64,
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "sandbox_id",
            help = "Bind this host workspace into the sandbox (for new sandbox creation)"
        )]
        workspace: Option<String>,
        #[arg(
            long = "ensure",
            value_name = "CMD",
            help = "Run setup command once per reusable session before --cmd (for example: install dependencies)"
        )]
        ensure: Vec<String>,
        #[arg(
            long = "write",
            value_name = "PATH=CONTENT",
            help = "Write file before command (repeatable)"
        )]
        writes: Vec<String>,
        #[arg(
            long = "read",
            value_name = "PATH",
            help = "Read file after command (repeatable)"
        )]
        reads: Vec<String>,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Print effective isolation and backend selection"
        )]
        explain: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Force one-off execution (create + destroy sandbox for this run)"
        )]
        ephemeral: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Start the command and return immediately with the process id"
        )]
        detach: bool,
    },
    /// Create a persistent sandbox and print its id.
    Create {
        #[arg(
            long,
            value_name = "NAME",
            help = "Optional affinity name for reusable named sandbox"
        )]
        name: Option<String>,
        #[arg(
            long,
            default_value = "auto",
            help = "Template image for sandbox (`auto` detects from workspace)"
        )]
        template: String,
        #[arg(long, value_enum, help = "Network mode")]
        network: Option<NetworkArg>,
        #[arg(
            long = "allow",
            value_name = "DOMAIN",
            help = "Allowlisted domain or wildcard subdomain pattern (repeat for multiple entries, only with allowlist mode)"
        )]
        allow: Vec<String>,
        #[arg(
            long,
            value_name = "NAME",
            help = "Profile name (built-ins: locked, web, full; custom from --profile-config)"
        )]
        profile: Option<String>,
        #[arg(
            long,
            default_value_t = 60,
            help = "Default command timeout in seconds"
        )]
        timeout: u64,
        #[arg(long, default_value_t = 512, help = "Sandbox memory limit in MiB")]
        memory_mb: u32,
        #[arg(long, default_value_t = 1, help = "Virtual CPU count")]
        vcpu_count: u8,
        #[arg(
            long,
            value_name = "PATH",
            help = "Bind this host workspace into the sandbox"
        )]
        workspace: Option<String>,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Print effective isolation and backend selection"
        )]
        explain: bool,
    },
    /// Destroy a sandbox by id or by affinity name.
    Destroy {
        #[arg(
            long,
            value_name = "ID",
            conflicts_with = "name",
            help = "Sandbox id to destroy"
        )]
        sandbox_id: Option<String>,
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with = "sandbox_id",
            help = "Affinity name to resolve and destroy"
        )]
        name: Option<String>,
    },
    /// List active sandboxes.
    List {
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// List managed processes.
    Ps {
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// Read managed process logs.
    Logs {
        #[arg(value_name = "PROCESS_ID", help = "Managed process id")]
        process_id: String,
        #[arg(
            long,
            default_value_t = false,
            help = "Follow logs until the process exits"
        )]
        follow: bool,
    },
    /// Wait for a managed process to finish.
    Wait {
        #[arg(value_name = "PROCESS_ID", help = "Managed process id")]
        process_id: String,
        #[arg(long, default_value_t = 60, help = "Wait timeout in seconds")]
        timeout: u64,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// Cancel a managed process.
    Cancel {
        #[arg(value_name = "PROCESS_ID", help = "Managed process id")]
        process_id: String,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// Inspect sandbox metadata.
    Inspect {
        #[arg(long, value_name = "ID", help = "Sandbox id")]
        sandbox_id: String,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// Open an interactive shell in a sandbox.
    Shell {
        #[arg(long, value_name = "ID", help = "Attach shell to existing sandbox id")]
        sandbox_id: Option<String>,
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with = "sandbox_id",
            help = "Attach shell to existing named sandbox"
        )]
        name: Option<String>,
        #[arg(
            long,
            default_value = "/bin/sh",
            help = "Shell executable inside sandbox"
        )]
        shell: String,
        #[arg(
            long,
            conflicts_with = "sandbox_id",
            help = "Template image for ephemeral shell creation"
        )]
        template: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "sandbox_id",
            help = "Workspace bind mount for ephemeral shell creation"
        )]
        workspace: Option<String>,
        #[arg(
            long,
            value_enum,
            conflicts_with = "sandbox_id",
            help = "Network mode for ephemeral shell creation"
        )]
        network: Option<NetworkArg>,
        #[arg(
            long = "allow",
            value_name = "DOMAIN",
            conflicts_with = "sandbox_id",
            help = "Allowlisted domain or wildcard subdomain pattern for ephemeral shell creation"
        )]
        allow: Vec<String>,
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with = "sandbox_id",
            help = "Profile name (built-ins: locked, web, full; custom from --profile-config)"
        )]
        profile: Option<String>,
        #[arg(
            long,
            default_value_t = false,
            help = "Print effective isolation and backend selection"
        )]
        explain: bool,
    },
    /// List available templates.
    Templates {
        #[arg(
            long,
            value_name = "PATH",
            help = "Optional local template manifest root"
        )]
        disk_root: Option<String>,
    },
    /// Start the local gRPC control-plane server (advanced).
    Serve {
        #[arg(long, default_value = DEFAULT_SERVER_ADDR, help = "Bind address")]
        addr: String,
    },
    /// Print host capability probe for backend selection.
    Probe,
    /// Install/check runtime prerequisites on this host.
    Setup,
    /// Start JSON-lines proxy mode for adapter integrations.
    #[command(
        hide = true,
        after_help = "Protocol:\n  read JSON lines from stdin, write JSON lines to stdout.\n  requests: {\"op\":\"ping\"} | {\"op\":\"exec\",\"cmd\":\"...\",\"timeout\":60} | {\"op\":\"read\",\"path\":\"...\"} | {\"op\":\"write\",\"path\":\"...\",\"content\":\"...\"} | {\"op\":\"destroy\"}\n  responses include op-specific fields or {\"op\":\"error\",\"message\":\"...\"}"
    )]
    Proxy {
        #[arg(
            long,
            default_value = "auto",
            help = "Template image for proxy sandbox (`auto` detects from workspace)"
        )]
        template: String,
        #[arg(long, value_enum, default_value_t = NetworkArg::None, help = "Network mode")]
        network: NetworkArg,
        #[arg(
            long = "allow",
            value_name = "DOMAIN",
            help = "Allowlisted domain or wildcard subdomain pattern (repeat for multiple entries, only with allowlist mode)"
        )]
        allow: Vec<String>,
        #[arg(long, default_value_t = 60, help = "Default exec timeout in seconds")]
        timeout: u64,
        #[arg(long, value_name = "PATH", help = "Workspace bind mount path")]
        workspace: Option<String>,
    },
    #[command(hide = true)]
    AppleHelper {
        #[arg(long, default_value = "container")]
        container_bin: String,
        #[arg(long)]
        state_root: Option<String>,
    },
    #[command(hide = true)]
    Bench {
        #[command(subcommand)]
        command: BenchCommand,
    },
    /// Manage snapshots for sandbox reuse and restore.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
}

#[derive(Debug, Subcommand)]
enum BenchCommand {
    Exec {
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
    Snapshot {
        #[arg(long, default_value = "python:3.12")]
        template: String,
        #[arg(long, value_enum, default_value_t = NetworkArg::None)]
        network: NetworkArg,
        #[arg(long = "allow")]
        allow: Vec<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(
            long,
            default_value = "echo state-one > hb-state.txt && mkdir -p .hb-bench && echo state-two > .hb-bench/state.txt"
        )]
        mutate_cmd: String,
        #[arg(long, default_value = "cat hb-state.txt && cat .hb-bench/state.txt")]
        verify_cmd: String,
        #[arg(long, default_value_t = 5)]
        runs: usize,
        #[arg(long, default_value_t = 1)]
        warmup: usize,
        #[arg(long, default_value_t = 240)]
        timeout: u64,
        #[arg(long, default_value_t = false)]
        keep_snapshot_artifacts: bool,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SnapshotCommand {
    /// Create a snapshot from an existing sandbox.
    Create {
        #[arg(
            long,
            value_name = "ID",
            conflicts_with = "name",
            help = "Source sandbox id"
        )]
        sandbox_id: Option<String>,
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with = "sandbox_id",
            help = "Source sandbox affinity name"
        )]
        name: Option<String>,
        #[arg(long, value_name = "TEXT", help = "Optional user note")]
        note: Option<String>,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// Restore a sandbox from snapshot id.
    Restore {
        #[arg(long, value_name = "SNAPSHOT_ID", help = "Snapshot id to restore")]
        snapshot_id: String,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
        json: bool,
    },
    /// List snapshots for a template.
    List {
        #[arg(long, help = "Template name (defaults to CLI default template)")]
        template: Option<String>,
        #[arg(long, default_value_t = false, help = "Emit JSON response")]
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
    fn to_mode(self, allow: Allowlist) -> NetworkMode {
        match self {
            Self::None => NetworkMode::None,
            Self::Full => NetworkMode::Full,
            Self::Allowlist => NetworkMode::Allowlist(allow),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProfileArg {
    Locked,
    Web,
    Full,
}

impl ProfileArg {
    fn parse_builtin(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "locked" => Some(Self::Locked),
            "web" => Some(Self::Web),
            "full" => Some(Self::Full),
            _ => None,
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
            name,
            template,
            cmd,
            network,
            allow,
            profile,
            timeout,
            workspace,
            ensure,
            writes,
            reads,
            json,
            explain,
            ephemeral,
            detach,
        } => {
            let autostart_default = cli.server_url.is_none();
            let mut client = connect_client(cli.server_url, autostart_default).await?;
            let server_info = load_server_info_best_effort(&mut client).await;
            let explain_details = explain
                .then(|| server_info.as_ref().map(build_explain_details))
                .flatten();
            if (sandbox_id.is_some() || name.is_some())
                && (network.is_some()
                    || profile.is_some()
                    || workspace.is_some()
                    || !allow.is_empty()
                    || ephemeral)
            {
                bail!(
                    "--network/--allow/--profile/--workspace/--ephemeral apply only when creating a sandbox; remove --sandbox-id/--name"
                );
            }
            let resolved_policy = resolve_network_policy(
                profile.as_deref(),
                network,
                allow,
                cli.profile_config.as_deref(),
            )?;

            if let Some(sandbox_id) = sandbox_id {
                let parsed_sandbox_id = parse_sandbox_id(&sandbox_id)?;
                let summary = build_effective_isolation_summary(
                    server_info.as_ref(),
                    None,
                    None,
                    writable_scope_from_writes(&writes, true),
                    timeout,
                );
                print_effective_isolation_summary(&summary, explain_details.as_ref())?;
                run_existing_with_client(
                    &mut client,
                    Some(parsed_sandbox_id),
                    None,
                    None,
                    false,
                    ensure,
                    cmd,
                    timeout,
                    writes,
                    reads,
                    json,
                    detach,
                    false,
                    Some(&summary),
                    explain_details.as_ref(),
                )
                .await?;
                return Ok(());
            }
            if let Some(name) = name {
                let summary = build_effective_isolation_summary(
                    server_info.as_ref(),
                    None,
                    None,
                    writable_scope_from_writes(&writes, true),
                    timeout,
                );
                print_effective_isolation_summary(&summary, explain_details.as_ref())?;
                run_existing_with_client(
                    &mut client,
                    None,
                    Some(name),
                    None,
                    false,
                    ensure,
                    cmd,
                    timeout,
                    writes,
                    reads,
                    json,
                    detach,
                    false,
                    Some(&summary),
                    explain_details.as_ref(),
                )
                .await?;
                return Ok(());
            }

            let resolved_template =
                resolve_template_for_operation(&template, workspace.as_deref(), Some(&cmd));
            let config = SandboxConfig {
                template: resolved_template,
                network: resolved_policy.network_mode,
                timeout_secs: timeout,
                workspace_dir: workspace,
                ..SandboxConfig::default()
            };
            let summary = build_effective_isolation_summary(
                server_info.as_ref(),
                Some(&config.network),
                resolved_policy.profile_label.as_deref(),
                writable_scope_from_workspace_and_writes(config.workspace_dir.as_deref(), &writes),
                timeout,
            );
            print_effective_isolation_summary(&summary, explain_details.as_ref())?;
            ensure_network_mode_supported(server_info.as_ref(), &config.network)?;

            if ephemeral {
                run_existing_with_client(
                    &mut client,
                    None,
                    None,
                    Some(config),
                    false,
                    ensure,
                    cmd,
                    timeout,
                    writes,
                    reads,
                    json,
                    detach,
                    !detach,
                    Some(&summary),
                    explain_details.as_ref(),
                )
                .await?;
            } else {
                run_existing_with_client(
                    &mut client,
                    None,
                    None,
                    Some(config),
                    true,
                    ensure,
                    cmd,
                    timeout,
                    writes,
                    reads,
                    json,
                    detach,
                    false,
                    Some(&summary),
                    explain_details.as_ref(),
                )
                .await?;
            }
        }
        Command::Create {
            name,
            template,
            network,
            allow,
            profile,
            timeout,
            memory_mb,
            vcpu_count,
            workspace,
            json,
            explain,
        } => {
            let mut client = connect_client(cli.server_url, true).await?;
            let server_info = load_server_info_best_effort(&mut client).await;
            let explain_details = explain
                .then(|| server_info.as_ref().map(build_explain_details))
                .flatten();
            let resolved_policy = resolve_network_policy(
                profile.as_deref(),
                network,
                allow,
                cli.profile_config.as_deref(),
            )?;
            let resolved_template =
                resolve_template_for_operation(&template, workspace.as_deref(), None);
            let config = SandboxConfig {
                affinity_name: name,
                template: resolved_template,
                memory_mb,
                vcpu_count,
                network: resolved_policy.network_mode,
                timeout_secs: timeout,
                workspace_dir: workspace,
                ..SandboxConfig::default()
            };
            let summary = build_effective_isolation_summary(
                server_info.as_ref(),
                Some(&config.network),
                resolved_policy.profile_label.as_deref(),
                writable_scope_from_workspace_and_writes(config.workspace_dir.as_deref(), &[]),
                timeout,
            );
            print_effective_isolation_summary(&summary, explain_details.as_ref())?;
            ensure_network_mode_supported(server_info.as_ref(), &config.network)?;

            let info = client.create_sandbox(config).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&CreateSandboxResponse {
                        sandbox_id: info.id.0.to_string(),
                        template: info.template,
                        state: format!("{:?}", info.state).to_lowercase(),
                        created_at: info.created_at.to_rfc3339(),
                        effective_isolation: Some(summary),
                        explain: explain_details,
                    })?
                );
            } else {
                println!("{}", info.id.0);
                eprintln!(
                    "tip: snapshot this environment with `hyperbox snapshot create --sandbox-id {}`",
                    info.id.0
                );
            }
        }
        Command::Destroy { sandbox_id, name } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let sandbox_id = if let Some(raw) = sandbox_id {
                parse_sandbox_id(&raw)?
            } else if let Some(name) = name {
                let (info, _) = client.resolve_affinity(&name, false).await?;
                info.id
            } else {
                bail!("destroy requires either --sandbox-id or --name");
            };
            client.destroy_sandbox(&sandbox_id).await?;
        }
        Command::List { json } => {
            let mut client = connect_client(cli.server_url, true).await?;
            let sandboxes = match client.list_sandboxes().await {
                Ok(rows) => rows,
                Err(err) => {
                    if err.to_string().contains("Unimplemented") {
                        bail!(
                            "server does not support `list` yet (likely stale daemon). Restart hyperbox server and retry `hyperbox list`."
                        );
                    }
                    return Err(err);
                }
            };
            if json {
                let rows: Vec<ListSandboxItemResponse> = sandboxes
                    .into_iter()
                    .map(|row| ListSandboxItemResponse {
                        sandbox_id: row.info.id.0.to_string(),
                        affinity_name: row.affinity_name,
                        template: row.info.template,
                        state: format!("{:?}", row.info.state).to_lowercase(),
                        created_at: row.info.created_at.to_rfc3339(),
                    })
                    .collect();
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                if sandboxes.is_empty() {
                    println!("no active sandboxes");
                    return Ok(());
                }
                println!("SANDBOX_ID\tNAME\tTEMPLATE\tSTATE\tCREATED_AT");
                for row in sandboxes {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        row.info.id.0,
                        row.affinity_name.unwrap_or_else(|| "-".to_string()),
                        row.info.template,
                        format!("{:?}", row.info.state).to_lowercase(),
                        row.info.created_at.to_rfc3339()
                    );
                }
                eprintln!("tip: attach with `hyperbox shell --sandbox-id <id>`");
            }
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
                        effective_isolation: None,
                        explain: None,
                    })?
                );
            } else {
                println!("{}", info.id.0);
            }
        }
        Command::Ps { json } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let processes = client.list_processes().await?;
            if json {
                let rows: Vec<ProcessListItemResponse> = processes
                    .into_iter()
                    .map(|process| ProcessListItemResponse::from(&process))
                    .collect();
                println!("{}", serde_json::to_string(&rows)?);
            } else if processes.is_empty() {
                eprintln!("no managed processes");
            } else {
                for process in processes {
                    println!(
                        "{}  {}  {}  {}",
                        process.id.0,
                        process.sandbox_id.0,
                        format!("{:?}", process.status).to_ascii_lowercase(),
                        process.command.join(" ")
                    );
                }
            }
        }
        Command::Logs { process_id, follow } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let process_id = parse_process_id(&process_id)?;
            if follow {
                let _ = stream_process_logs(&mut client, &process_id, false).await?;
            } else {
                let logs = read_process_logs_once(&mut client, &process_id).await?;
                print!("{}", logs.stdout);
                eprint!("{}", logs.stderr);
            }
        }
        Command::Wait {
            process_id,
            timeout,
            json,
        } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let process_id = parse_process_id(&process_id)?;
            let process = client.wait_process(&process_id, timeout).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&ProcessInfoResponse::from(&process))?
                );
            } else {
                println!(
                    "{} {} {}",
                    process.id.0,
                    process.sandbox_id.0,
                    format!("{:?}", process.status).to_ascii_lowercase()
                );
            }
        }
        Command::Cancel { process_id, json } => {
            let mut client = connect_client(cli.server_url, false).await?;
            let process_id = parse_process_id(&process_id)?;
            let process = client.cancel_process(&process_id).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&ProcessInfoResponse::from(&process))?
                );
            } else {
                println!(
                    "{} {} {}",
                    process.id.0,
                    process.sandbox_id.0,
                    format!("{:?}", process.status).to_ascii_lowercase()
                );
            }
        }
        Command::Shell {
            sandbox_id,
            name,
            shell,
            template,
            workspace,
            network,
            allow,
            profile,
            explain,
        } => {
            if (sandbox_id.is_some() || name.is_some())
                && (network.is_some()
                    || profile.is_some()
                    || workspace.is_some()
                    || !allow.is_empty())
            {
                bail!(
                    "--network/--allow/--profile/--workspace apply only when creating an ephemeral shell; remove --sandbox-id/--name"
                );
            }
            let resolved_policy = resolve_network_policy(
                profile.as_deref(),
                network,
                allow,
                cli.profile_config.as_deref(),
            )?;
            let exit_code = shell_command(
                cli.server_url,
                sandbox_id,
                name,
                &shell,
                template.unwrap_or_else(|| "auto".to_string()),
                workspace,
                resolved_policy.network_mode,
                resolved_policy.profile_label,
                explain,
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
            let resolved_template =
                resolve_template_for_operation(&template, workspace.as_deref(), None);
            run_proxy_loop(
                cli.server_url,
                SandboxConfig {
                    template: resolved_template,
                    network: network.to_mode(
                        Allowlist::parse(&allow)
                            .map_err(|msg| anyhow::anyhow!("invalid allowlist: {msg}"))?,
                    ),
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
        Command::Bench { command } => {
            if std::env::var_os("HYPERBOX_INTERNAL").is_none() {
                bail!(
                    "`bench` is an internal command and is disabled by default; set HYPERBOX_INTERNAL=1 to enable"
                );
            }
            match command {
                BenchCommand::Exec {
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
                BenchCommand::Snapshot {
                    template,
                    network,
                    allow,
                    workspace,
                    mutate_cmd,
                    verify_cmd,
                    runs,
                    warmup,
                    timeout,
                    keep_snapshot_artifacts,
                    json,
                } => {
                    let config = SandboxConfig {
                        template,
                        network: network.to_mode(
                            Allowlist::parse(&allow)
                                .map_err(|msg| anyhow::anyhow!("invalid allowlist: {msg}"))?,
                        ),
                        timeout_secs: timeout,
                        workspace_dir: workspace,
                        ..SandboxConfig::default()
                    };
                    let summary = bench_snapshot_remote(
                        cli.server_url,
                        config,
                        mutate_cmd,
                        verify_cmd,
                        warmup,
                        runs,
                        timeout,
                        keep_snapshot_artifacts,
                    )
                    .await?;
                    if json {
                        println!("{}", serde_json::to_string(&summary)?);
                    } else {
                        print_snapshot_bench_summary(&summary);
                    }
                }
            }
        }
        Command::Snapshot { command } => {
            run_snapshot_command(cli.server_url, command).await?;
        }
    }

    Ok(())
}

fn parse_sandbox_id(raw: &str) -> anyhow::Result<SandboxId> {
    let id = uuid::Uuid::parse_str(raw)
        .with_context(|| format!("invalid sandbox id `{raw}`: expected UUID"))?;
    Ok(SandboxId(id))
}

fn parse_process_id(raw: &str) -> anyhow::Result<ProcessId> {
    let id = uuid::Uuid::parse_str(raw)
        .with_context(|| format!("invalid process id `{raw}`: expected UUID"))?;
    Ok(ProcessId(id))
}

fn parse_snapshot_id(raw: &str) -> anyhow::Result<SnapshotId> {
    let id = uuid::Uuid::parse_str(raw)
        .with_context(|| format!("invalid snapshot id `{raw}`: expected UUID"))?;
    Ok(SnapshotId(id))
}

fn resolve_template_for_operation(
    template_arg: &str,
    workspace: Option<&str>,
    command_hint: Option<&str>,
) -> String {
    let workspace = workspace.map(ToString::to_string).unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string()
    });
    let command_hint = command_hint.unwrap_or_default();
    let resolved = template_auto::resolve_template(template_arg, &workspace, command_hint);
    if template_arg.eq_ignore_ascii_case("auto") {
        eprintln!(
            "template: auto-selected `{}` ({})",
            resolved.template, resolved.reason
        );
    }
    resolved.template
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

async fn load_server_info_best_effort(client: &mut GrpcControlClient) -> Option<ServerInfo> {
    match client.get_server_info().await {
        Ok(info) => Some(info),
        Err(err) => {
            warn!(error = %err, "failed to fetch server info; explain output will be partial");
            None
        }
    }
}

fn build_explain_details(server_info: &ServerInfo) -> ExplainDetails {
    ExplainDetails {
        backend_requested: server_info.backend_requested.clone(),
        backend_selected: server_info.backend_selected.clone(),
        backend_reason: server_info.backend_reason.clone(),
        apple_runtime: server_info.apple_runtime.clone(),
        apple_helper_argv: server_info.apple_helper_argv.clone(),
    }
}

fn build_effective_isolation_summary(
    server_info: Option<&ServerInfo>,
    network: Option<&NetworkMode>,
    profile: Option<&str>,
    writable_paths: Vec<String>,
    timeout_secs: u64,
) -> EffectiveIsolationSummary {
    let backend = server_info
        .map(|info| info.backend_selected.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let isolation_class = match backend.as_str() {
        "firecracker" | "apple" => "vm",
        "local" => "host",
        _ => "unknown",
    }
    .to_string();
    let network_mode = match network {
        Some(NetworkMode::None) => "none".to_string(),
        Some(NetworkMode::Full) => "full".to_string(),
        Some(NetworkMode::Allowlist(_)) => "allowlist".to_string(),
        None => "unknown".to_string(),
    };
    let profile = match profile {
        Some(profile) => profile.to_string(),
        None => match network {
            Some(NetworkMode::None) => "locked".to_string(),
            Some(NetworkMode::Allowlist(_)) => "web".to_string(),
            Some(NetworkMode::Full) => "full".to_string(),
            None => "unknown".to_string(),
        },
    };
    let (network_enforcement, network_reason) = network_enforcement_status(server_info, network);

    EffectiveIsolationSummary {
        backend,
        isolation_class,
        profile,
        network_mode,
        network_enforcement,
        network_reason,
        writable_paths,
        timeout_secs,
    }
}

fn network_enforcement_status(
    server_info: Option<&ServerInfo>,
    network: Option<&NetworkMode>,
) -> (String, Option<String>) {
    let Some(network) = network else {
        return (
            "unknown".to_string(),
            Some("network mode unavailable".to_string()),
        );
    };
    let Some(server_info) = server_info else {
        return (
            "unknown".to_string(),
            Some("server info unavailable".to_string()),
        );
    };

    match server_info.backend_selected.as_str() {
        "firecracker" => ("enforced".to_string(), None),
        "local" => (
            "not_enforced".to_string(),
            Some(
                "local backend is non-isolated dev mode; use auto/apple/firecracker for policy enforcement"
                    .to_string(),
            ),
        ),
        "apple" => match network {
            NetworkMode::Allowlist(_) => {
                let runtime_containerization =
                    server_info.apple_runtime.as_deref() == Some("containerization");
                let runtime_virtualization =
                    server_info.apple_runtime.as_deref() == Some("virtualization");
                let builtin_helper = helper_argv_is_builtin_apple_helper(&server_info.apple_helper_argv);
                if builtin_helper && runtime_containerization {
                    (
                        "enforced".to_string(),
                        Some("enforced by built-in apple helper allowlist runtime".to_string()),
                    )
                } else if runtime_virtualization && !builtin_helper {
                    (
                        "enforced".to_string(),
                        Some("enforced by external apple helper virtualization runtime".to_string()),
                    )
                } else if !server_info.apple_helper_argv.is_empty() {
                    (
                        "enforced".to_string(),
                        Some("enforced by helper-managed apple runtime".to_string()),
                    )
                } else {
                    (
                        "unsupported".to_string(),
                        Some(
                            "allowlist requires helper-managed apple runtime; direct container mode supports none/full only".to_string(),
                        ),
                    )
                }
            }
            NetworkMode::None | NetworkMode::Full => ("enforced".to_string(), None),
        },
        _ => (
            "unknown".to_string(),
            Some("unknown backend/network enforcement".to_string()),
        ),
    }
}

fn ensure_network_mode_supported(
    server_info: Option<&ServerInfo>,
    network: &NetworkMode,
) -> anyhow::Result<()> {
    let (status, reason) = network_enforcement_status(server_info, Some(network));
    if status == "unsupported" {
        if let Some(reason) = reason {
            bail!("{reason}");
        }
        bail!("requested network mode is unsupported by active backend");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ProfileConfigFile {
    profiles: std::collections::HashMap<String, ProfileDefinition>,
}

#[derive(Debug, Deserialize)]
struct ProfileDefinition {
    network: String,
    #[serde(default)]
    allow: Vec<String>,
}

#[derive(Debug)]
struct ResolvedNetworkPolicy {
    network_mode: NetworkMode,
    profile_label: Option<String>,
}

fn resolve_network_policy(
    profile: Option<&str>,
    network: Option<NetworkArg>,
    allow: Vec<String>,
    profile_config_path: Option<&std::path::Path>,
) -> anyhow::Result<ResolvedNetworkPolicy> {
    let mut resolved = if let Some(profile_name) = profile {
        let profile_mode = resolve_profile_network_defaults(profile_name, profile_config_path)?;
        ResolvedNetworkPolicy {
            network_mode: profile_mode,
            profile_label: Some(profile_name.to_string()),
        }
    } else {
        ResolvedNetworkPolicy {
            network_mode: NetworkMode::None,
            profile_label: None,
        }
    };

    if let Some(network_override) = network {
        resolved.network_mode = match network_override {
            NetworkArg::None => NetworkMode::None,
            NetworkArg::Full => NetworkMode::Full,
            NetworkArg::Allowlist => {
                let defaults = match &resolved.network_mode {
                    NetworkMode::Allowlist(domains) => domains.clone(),
                    _ => Allowlist::parse(&[]).expect("empty allowlist"),
                };
                NetworkMode::Allowlist(defaults)
            }
        };
    }

    if !allow.is_empty() {
        match &mut resolved.network_mode {
            NetworkMode::Allowlist(domains) => {
                *domains = Allowlist::parse(&allow)
                    .map_err(|msg| anyhow::anyhow!("invalid allowlist: {msg}"))?
            }
            _ => bail!(
                "--allow requires allowlist network (set --network allowlist or use an allowlist profile)"
            ),
        }
    }

    if let NetworkMode::Allowlist(domains) = &resolved.network_mode
        && domains.is_empty()
    {
        bail!(
            "allowlist mode requires at least one domain (via --allow or profile default allowlist)"
        );
    }

    Ok(resolved)
}

fn resolve_profile_network_defaults(
    profile_name: &str,
    profile_config_path: Option<&std::path::Path>,
) -> anyhow::Result<NetworkMode> {
    if let Some(builtin) = ProfileArg::parse_builtin(profile_name) {
        return Ok(match builtin {
            ProfileArg::Locked => NetworkMode::None,
            ProfileArg::Web => {
                NetworkMode::Allowlist(Allowlist::parse(&[]).expect("empty allowlist"))
            }
            ProfileArg::Full => NetworkMode::Full,
        });
    }

    let config_path = resolve_profile_config_path(profile_config_path)?;
    let raw = std::fs::read_to_string(&config_path).with_context(|| {
        format!(
            "read profile config `{}` for profile `{profile_name}`",
            config_path.display()
        )
    })?;
    let parsed: ProfileConfigFile = toml::from_str(&raw)
        .with_context(|| format!("parse TOML profile config `{}`", config_path.display()))?;

    let profile = parsed.profiles.get(profile_name).ok_or_else(|| {
        anyhow::anyhow!(
            "profile `{profile_name}` not found in `{}`",
            config_path.display()
        )
    })?;

    let network = match profile.network.to_ascii_lowercase().as_str() {
        "none" => NetworkMode::None,
        "full" => NetworkMode::Full,
        "allowlist" => NetworkMode::Allowlist(
            Allowlist::parse(&profile.allow)
                .map_err(|msg| anyhow::anyhow!("invalid allowlist: {msg}"))?,
        ),
        other => {
            bail!(
                "profile `{profile_name}` has invalid network `{other}`; expected one of: none, allowlist, full"
            )
        }
    };

    if !profile.allow.is_empty() && !matches!(network, NetworkMode::Allowlist(_)) {
        bail!("profile `{profile_name}` defines `allow` entries but network is not allowlist");
    }

    Ok(network)
}

fn resolve_profile_config_path(
    profile_config_path: Option<&std::path::Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = profile_config_path {
        return Ok(path.to_path_buf());
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .context("resolve HOME for default profile config path")?;
    Ok(home.join(".hyperbox/profiles.toml"))
}

fn writable_scope_from_workspace_and_writes(
    workspace: Option<&str>,
    writes: &[String],
) -> Vec<String> {
    let mut writable = Vec::new();
    if let Some(workspace) = workspace {
        writable.push(workspace.to_string());
    } else {
        writable.push("<ephemeral sandbox workspace>".to_string());
    }

    for entry in writes {
        if let Some((path, _)) = entry.split_once('=') {
            writable.push(path.to_string());
        }
    }

    writable.sort();
    writable.dedup();
    writable
}

fn writable_scope_from_writes(writes: &[String], include_existing_workspace: bool) -> Vec<String> {
    let mut writable = Vec::new();
    if include_existing_workspace {
        writable.push("<existing sandbox workspace>".to_string());
    }
    for entry in writes {
        if let Some((path, _)) = entry.split_once('=') {
            writable.push(path.to_string());
        }
    }
    writable.sort();
    writable.dedup();
    writable
}

fn print_effective_isolation_summary(
    summary: &EffectiveIsolationSummary,
    explain: Option<&ExplainDetails>,
) -> anyhow::Result<()> {
    eprintln!("effective isolation:");
    eprintln!(
        "  backend: {} ({})",
        summary.backend, summary.isolation_class
    );
    eprintln!("  profile: {}", summary.profile);
    eprintln!(
        "  network: {} [{}]",
        summary.network_mode, summary.network_enforcement
    );
    if let Some(reason) = &summary.network_reason {
        eprintln!("  network_reason: {reason}");
    }
    eprintln!("  writable: {}", summary.writable_paths.join(", "));
    if summary.timeout_secs > 0 {
        eprintln!("  timeout_secs: {}", summary.timeout_secs);
    }

    if let Some(details) = explain {
        eprintln!("explain:");
        eprintln!("  backend_requested: {}", details.backend_requested);
        eprintln!("  backend_selected: {}", details.backend_selected);
        eprintln!("  backend_reason: {}", details.backend_reason);
        if let Some(runtime) = &details.apple_runtime {
            eprintln!("  apple_runtime: {runtime}");
        }
        if !details.apple_helper_argv.is_empty() {
            eprintln!("  apple_helper: {}", details.apple_helper_argv.join(" "));
        }
    }

    Ok(())
}

async fn run_existing_with_client(
    client: &mut GrpcControlClient,
    sandbox_id: Option<SandboxId>,
    affinity_name: Option<String>,
    create_config: Option<SandboxConfig>,
    reuse_auto_session: bool,
    ensure_commands: Vec<String>,
    cmd: String,
    timeout: u64,
    writes: Vec<String>,
    reads: Vec<String>,
    json: bool,
    detach: bool,
    destroy_after_wait: bool,
    effective_isolation: Option<&EffectiveIsolationSummary>,
    explain: Option<&ExplainDetails>,
) -> anyhow::Result<()> {
    let writes = writes
        .into_iter()
        .map(|entry| {
            let (path, content) = entry
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid --write value, expected PATH=CONTENT"))?;
            Ok((path.to_string(), content.as_bytes().to_vec()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let started = client
        .start_run(
            sandbox_id,
            affinity_name,
            create_config,
            reuse_auto_session,
            ensure_commands,
            writes,
            cmd,
            detach,
        )
        .await?;
    maybe_print_session_notice(&started);
    maybe_print_overflow_notice(&started.process);
    if detach {
        emit_process_start(&started.process, json, effective_isolation, explain)?;
        return Ok(());
    }
    let outcome = wait_for_process_outcome(client, &started.process.id, timeout, json).await?;

    let mut artifacts = Vec::new();
    for path in reads {
        let bytes = client.read_file(&started.sandbox.id, path.clone()).await?;
        artifacts.push((path, String::from_utf8_lossy(&bytes).to_string()));
    }

    let run_result = emit_result(
        outcome,
        artifacts,
        json,
        !json,
        effective_isolation,
        explain,
    );
    if destroy_after_wait {
        let destroy_result = client.destroy_sandbox(&started.sandbox.id).await;
        if let Err(err) = destroy_result {
            if run_result.is_ok() {
                return Err(err);
            }
        }
    }

    let exit_code = run_result?;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn maybe_print_overflow_notice(process: &ProcessInfo) {
    if process.disposition == ProcessDisposition::CreatedDueToBusy {
        eprintln!(
            "requested sandbox was busy; created a new sandbox {} for this run",
            process.sandbox_id.0
        );
    }
}

fn maybe_print_session_notice(started: &hyperbox_server::StartedRun) {
    if let Some(session_name) = started.session_name.as_deref() {
        if started.session_created {
            eprintln!(
                "session: started `{session_name}` (reused automatically on next `run`; use --ephemeral for one-off)"
            );
        } else {
            eprintln!("session: reusing `{session_name}` (use --ephemeral for one-off)");
        }
    }
}

#[derive(Default)]
struct ProcessLogs {
    stdout: String,
    stderr: String,
}

struct SyncRunOutcome {
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u128,
}

async fn run_sync_in_sandbox(
    client: &mut GrpcControlClient,
    sandbox_id: &SandboxId,
    command: String,
    timeout_secs: u64,
) -> anyhow::Result<SyncRunOutcome> {
    let started = Instant::now();
    let process = client
        .start_run(
            Some(sandbox_id.clone()),
            None,
            None,
            false,
            vec![],
            vec![],
            command,
            false,
        )
        .await?;
    let outcome = wait_for_process_outcome(client, &process.process.id, timeout_secs, true).await?;
    Ok(SyncRunOutcome {
        exit_code: outcome.exit_code,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        duration_ms: started.elapsed().as_millis(),
    })
}

async fn wait_for_process_outcome(
    client: &mut GrpcControlClient,
    process_id: &ProcessId,
    timeout_secs: u64,
    json: bool,
) -> anyhow::Result<hyperbox_core::ExecOutcome> {
    let started = Instant::now();
    let logs = if json {
        let process = client.wait_process(process_id, timeout_secs).await?;
        let logs = read_process_logs_once(client, process_id).await?;
        let duration_ms = process
            .finished_at
            .map(|finished_at| (finished_at - process.started_at).num_milliseconds().max(0) as u128)
            .unwrap_or_else(|| started.elapsed().as_millis());
        return Ok(hyperbox_core::ExecOutcome {
            exit_code: process.exit_code.unwrap_or(1),
            stdout: logs.stdout,
            stderr: logs.stderr,
            duration_ms,
        });
    } else {
        stream_process_logs(client, process_id, true).await?
    };

    let process = client.wait_process(process_id, timeout_secs).await?;
    let duration_ms = process
        .finished_at
        .map(|finished_at| (finished_at - process.started_at).num_milliseconds().max(0) as u128)
        .unwrap_or_else(|| started.elapsed().as_millis());
    Ok(hyperbox_core::ExecOutcome {
        exit_code: process.exit_code.unwrap_or(1),
        stdout: logs.stdout,
        stderr: logs.stderr,
        duration_ms,
    })
}

async fn read_process_logs_once(
    client: &mut GrpcControlClient,
    process_id: &ProcessId,
) -> anyhow::Result<ProcessLogs> {
    let stdout = read_process_log_all(client, process_id, StreamName::Stdout).await?;
    let stderr = read_process_log_all(client, process_id, StreamName::Stderr).await?;
    Ok(ProcessLogs { stdout, stderr })
}

async fn read_process_log_all(
    client: &mut GrpcControlClient,
    process_id: &ProcessId,
    stream: StreamName,
) -> anyhow::Result<String> {
    let mut offset = 0u64;
    let mut contents = String::new();
    loop {
        let chunk = client
            .read_process_log(process_id, stream.clone(), offset, 8192)
            .await?;
        if chunk.contents.is_empty() {
            if chunk.eof {
                return Ok(contents);
            }
        } else {
            offset = chunk.next_offset;
            contents.push_str(&chunk.contents);
        }
        if chunk.eof {
            return Ok(contents);
        }
    }
}

async fn stream_process_logs(
    client: &mut GrpcControlClient,
    process_id: &ProcessId,
    stop_on_exit: bool,
) -> anyhow::Result<ProcessLogs> {
    let mut stdout_offset = 0u64;
    let mut stderr_offset = 0u64;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut out = tokio::io::stdout();
    let mut err = tokio::io::stderr();

    loop {
        let stdout_chunk = client
            .read_process_log(process_id, StreamName::Stdout, stdout_offset, 8192)
            .await?;
        if !stdout_chunk.contents.is_empty() {
            stdout_offset = stdout_chunk.next_offset;
            stdout.push_str(&stdout_chunk.contents);
            out.write_all(stdout_chunk.contents.as_bytes()).await?;
            out.flush().await?;
        }

        let stderr_chunk = client
            .read_process_log(process_id, StreamName::Stderr, stderr_offset, 8192)
            .await?;
        if !stderr_chunk.contents.is_empty() {
            stderr_offset = stderr_chunk.next_offset;
            stderr.push_str(&stderr_chunk.contents);
            err.write_all(stderr_chunk.contents.as_bytes()).await?;
            err.flush().await?;
        }

        let process = client.get_process(process_id).await?;
        if process.status.is_terminal() {
            if stop_on_exit {
                loop {
                    let chunk = client
                        .read_process_log(process_id, StreamName::Stdout, stdout_offset, 8192)
                        .await?;
                    if chunk.contents.is_empty() {
                        break;
                    }
                    stdout_offset = chunk.next_offset;
                    stdout.push_str(&chunk.contents);
                    out.write_all(chunk.contents.as_bytes()).await?;
                    out.flush().await?;
                    if chunk.eof {
                        break;
                    }
                }
                loop {
                    let chunk = client
                        .read_process_log(process_id, StreamName::Stderr, stderr_offset, 8192)
                        .await?;
                    if chunk.contents.is_empty() {
                        break;
                    }
                    stderr_offset = chunk.next_offset;
                    stderr.push_str(&chunk.contents);
                    err.write_all(chunk.contents.as_bytes()).await?;
                    err.flush().await?;
                    if chunk.eof {
                        break;
                    }
                }
            }
            return Ok(ProcessLogs { stdout, stderr });
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn emit_process_start(
    process: &ProcessInfo,
    json: bool,
    effective_isolation: Option<&EffectiveIsolationSummary>,
    explain: Option<&ExplainDetails>,
) -> anyhow::Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&ProcessStartResponse {
                process: ProcessInfoResponse::from(process),
                effective_isolation: effective_isolation.cloned(),
                explain: explain.cloned(),
            })?
        );
    } else {
        println!("process_id: {}", process.id.0);
        println!("sandbox_id: {}", process.sandbox_id.0);
        println!(
            "disposition: {}",
            format!("{:?}", process.disposition).to_ascii_lowercase()
        );
    }
    Ok(())
}

async fn shell_command(
    server_url: Option<String>,
    sandbox_id: Option<String>,
    affinity_name: Option<String>,
    shell: &str,
    template: String,
    workspace: Option<String>,
    network: NetworkMode,
    profile: Option<String>,
    explain: bool,
) -> anyhow::Result<i32> {
    let autostart_default = server_url.is_none();
    let mut client = connect_client(server_url, autostart_default).await?;
    let server_info = load_server_info_best_effort(&mut client).await;
    let explain_details = explain
        .then(|| server_info.as_ref().map(build_explain_details))
        .flatten();

    if let Some(sandbox_id) = sandbox_id {
        let sandbox_id = parse_sandbox_id(&sandbox_id)?;
        let summary = build_effective_isolation_summary(
            server_info.as_ref(),
            None,
            None,
            vec!["<existing sandbox workspace>".to_string()],
            0,
        );
        print_effective_isolation_summary(&summary, explain_details.as_ref())?;
        return open_shell_with_client(&mut client, &sandbox_id, shell).await;
    }
    if let Some(name) = affinity_name {
        let (info, restored) = client.resolve_affinity(&name, true).await?;
        info!(
            affinity = %name,
            sandbox_id = %info.id.0,
            restored,
            "resolved affinity for shell command"
        );
        let summary = build_effective_isolation_summary(
            server_info.as_ref(),
            None,
            None,
            vec!["<affinity sandbox workspace>".to_string()],
            0,
        );
        print_effective_isolation_summary(&summary, explain_details.as_ref())?;
        return open_shell_with_client(&mut client, &info.id, shell).await;
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
    let resolved_template =
        resolve_template_for_operation(&template, workspace_dir.as_deref(), None);

    let summary = build_effective_isolation_summary(
        server_info.as_ref(),
        Some(&network),
        profile.as_deref(),
        writable_scope_from_workspace_and_writes(workspace_dir.as_deref(), &[]),
        0,
    );
    print_effective_isolation_summary(&summary, explain_details.as_ref())?;
    ensure_network_mode_supported(server_info.as_ref(), &network)?;

    info!(
        template = %resolved_template,
        workspace = ?workspace_dir,
        "creating ephemeral shell sandbox"
    );
    let create_started = Instant::now();
    let sandbox = client
        .create_sandbox(SandboxConfig {
            template: resolved_template,
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
        return open_shell_via_agent_stream(sandbox_id, shell).await.with_context(|| {
            format!(
                "interactive shell for apple backend with external helper `{}` requires agent stream connectivity (set HYPERBOX_AGENT_ENDPOINT if non-default)",
                server_info.apple_helper_argv.join(" ")
            )
        });
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
    let probe = run_sync_in_sandbox(client, sandbox_id, "pwd".to_string(), 10)
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

async fn run_snapshot_command(
    server_url: Option<String>,
    command: SnapshotCommand,
) -> anyhow::Result<()> {
    let autostart_default = server_url.is_none();
    let mut client = connect_client(server_url, autostart_default).await?;
    match command {
        SnapshotCommand::Create {
            sandbox_id,
            name,
            note,
            json,
        } => {
            let sandbox_id = if let Some(raw) = sandbox_id {
                parse_sandbox_id(&raw)?
            } else if let Some(name) = name {
                let (info, _) = client.resolve_affinity(&name, false).await?;
                info.id
            } else {
                bail!("snapshot create requires either --sandbox-id or --name");
            };
            let (snapshot_id, created_at) = client.create_snapshot(&sandbox_id, note).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&SnapshotCreateResponse {
                        snapshot_id: snapshot_id.0.to_string(),
                        sandbox_id: sandbox_id.0.to_string(),
                        created_at,
                    })?
                );
            } else {
                println!("{}", snapshot_id.0);
                eprintln!(
                    "tip: restore with `hyperbox snapshot restore --snapshot-id {}`",
                    snapshot_id.0
                );
                eprintln!(
                    "tip: list snapshots for template with `hyperbox snapshot list --template python:3.12`"
                );
            }
        }
        SnapshotCommand::Restore { snapshot_id, json } => {
            let snapshot_id = parse_snapshot_id(&snapshot_id)?;
            let info = client.restore_snapshot(&snapshot_id).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&SandboxInfoResponse {
                        sandbox_id: info.id.0.to_string(),
                        template: info.template,
                        state: format!("{:?}", info.state).to_lowercase(),
                        created_at: info.created_at.to_rfc3339(),
                        effective_isolation: None,
                        explain: None,
                    })?
                );
            } else {
                println!("{}", info.id.0);
                eprintln!(
                    "tip: open a shell in the restored sandbox with `hyperbox shell --sandbox-id {}`",
                    info.id.0
                );
            }
        }
        SnapshotCommand::List { template, json } => {
            let template = template.unwrap_or_else(|| SandboxConfig::default().template);
            let snapshots = client.list_snapshots(&template).await?;
            if json {
                let rows: Vec<SnapshotListItemResponse> = snapshots
                    .into_iter()
                    .map(|s| SnapshotListItemResponse {
                        snapshot_id: s.id.0.to_string(),
                        sandbox_id: s.sandbox_id.0.to_string(),
                        template: s.template,
                        affinity_name: s.affinity_name,
                        created_at: s.created_at.to_rfc3339(),
                        note: s.note,
                    })
                    .collect();
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                for snapshot in snapshots {
                    println!(
                        "{}\t{}\t{}\t{}",
                        snapshot.id.0,
                        snapshot.template,
                        snapshot.affinity_name.unwrap_or_else(|| "-".to_string()),
                        snapshot.created_at.to_rfc3339()
                    );
                }
            }
        }
    }
    Ok(())
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
    let template = config.template.clone();
    let workspace = config.workspace_dir.clone();
    let network_label = match &config.network {
        NetworkMode::None => "none".to_string(),
        NetworkMode::Full => "full".to_string(),
        NetworkMode::Allowlist(domains) => {
            if domains.is_empty() {
                "allowlist".to_string()
            } else {
                format!("allowlist({})", domains.to_strings().join(","))
            }
        }
    };

    let mut client = connect_client(server_url, true).await?;
    let sandbox = client.create_sandbox(config).await?;
    let sandbox_id = sandbox.id;
    eprintln!(
        "proxy: started sandbox {} (template={}, network={}, workspace={})",
        sandbox_id.0,
        template,
        network_label,
        workspace.unwrap_or_else(|| "<ephemeral sandbox workspace>".to_string())
    );
    eprintln!("proxy: reading JSON lines from stdin and writing JSON lines to stdout");
    eprintln!("proxy: request ops = ping | exec | read | write | destroy (destroy exits proxy)");

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<ProxyRequest>(&line) {
            Ok(ProxyRequest::Exec { cmd, timeout }) => {
                match run_sync_in_sandbox(&mut client, &sandbox_id, cmd, timeout.unwrap_or(60))
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
                }
            }
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
    logs_already_emitted: bool,
    effective_isolation: Option<&EffectiveIsolationSummary>,
    explain: Option<&ExplainDetails>,
) -> anyhow::Result<i32> {
    let command_not_found =
        outcome.exit_code == 127 && outcome.stderr.to_ascii_lowercase().contains("not found");
    if json {
        let response = RunResponse {
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration_ms,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            artifacts,
            effective_isolation: effective_isolation.cloned(),
            explain: explain.cloned(),
        };
        println!("{}", serde_json::to_string(&response)?);
    } else {
        if !logs_already_emitted {
            print!("{}", outcome.stdout);
            eprint!("{}", outcome.stderr);
        }
        for (path, data) in &artifacts {
            println!("--- {path} ---");
            print!("{data}");
            if !data.ends_with('\n') {
                println!();
            }
        }
        if command_not_found {
            eprintln!(
                "tip: install missing tools in this sandbox session (for example: `hyperbox run --profile full --cmd \"python3 -m pip install pytest\"`), then rerun"
            );
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
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_isolation: Option<EffectiveIsolationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<ExplainDetails>,
}

#[derive(Debug, Serialize)]
struct SandboxInfoResponse {
    sandbox_id: String,
    template: String,
    state: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_isolation: Option<EffectiveIsolationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<ExplainDetails>,
}

#[derive(Debug, Serialize)]
struct CreateSandboxResponse {
    sandbox_id: String,
    template: String,
    state: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_isolation: Option<EffectiveIsolationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<ExplainDetails>,
}

#[derive(Debug, Clone, Serialize)]
struct EffectiveIsolationSummary {
    backend: String,
    isolation_class: String,
    profile: String,
    network_mode: String,
    network_enforcement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_reason: Option<String>,
    writable_paths: Vec<String>,
    timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ExplainDetails {
    backend_requested: String,
    backend_selected: String,
    backend_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    apple_runtime: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    apple_helper_argv: Vec<String>,
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

#[derive(Debug, Serialize)]
struct SnapshotBenchStageSummary {
    mean_ms: f64,
    p50_ms: u128,
    p95_ms: u128,
    min_ms: u128,
    max_ms: u128,
}

#[derive(Debug, Serialize)]
struct SnapshotBenchRun {
    run: usize,
    sandbox_id: String,
    restored_sandbox_id: String,
    snapshot_id: String,
    create_ms: u128,
    mutate_ms: u128,
    snapshot_create_ms: u128,
    destroy_initial_ms: u128,
    restore_verify_ms: u128,
    destroy_restored_ms: u128,
    total_ms: u128,
}

#[derive(Debug, Serialize)]
struct SnapshotBenchSummary {
    runs: usize,
    warmup: usize,
    create_ms: SnapshotBenchStageSummary,
    mutate_ms: SnapshotBenchStageSummary,
    snapshot_create_ms: SnapshotBenchStageSummary,
    destroy_initial_ms: SnapshotBenchStageSummary,
    restore_verify_ms: SnapshotBenchStageSummary,
    destroy_restored_ms: SnapshotBenchStageSummary,
    total_ms: SnapshotBenchStageSummary,
    raw_runs: Vec<SnapshotBenchRun>,
}

#[derive(Debug, Serialize)]
struct SnapshotCreateResponse {
    snapshot_id: String,
    sandbox_id: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct SnapshotListItemResponse {
    snapshot_id: String,
    sandbox_id: String,
    template: String,
    affinity_name: Option<String>,
    created_at: String,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct ListSandboxItemResponse {
    sandbox_id: String,
    affinity_name: Option<String>,
    template: String,
    state: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct ProcessInfoResponse {
    process_id: String,
    sandbox_id: String,
    requested_sandbox_id: Option<String>,
    disposition: String,
    status: String,
    command: Vec<String>,
    exit_code: Option<i32>,
    started_at: String,
    finished_at: Option<String>,
    expires_at: Option<String>,
}

impl From<&ProcessInfo> for ProcessInfoResponse {
    fn from(value: &ProcessInfo) -> Self {
        Self {
            process_id: value.id.0.to_string(),
            sandbox_id: value.sandbox_id.0.to_string(),
            requested_sandbox_id: value
                .requested_sandbox_id
                .as_ref()
                .map(|id| id.0.to_string()),
            disposition: format!("{:?}", value.disposition).to_ascii_lowercase(),
            status: format!("{:?}", value.status).to_ascii_lowercase(),
            command: value.command.clone(),
            exit_code: value.exit_code,
            started_at: value.started_at.to_rfc3339(),
            finished_at: value.finished_at.map(|time| time.to_rfc3339()),
            expires_at: value.expires_at.map(|time| time.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
struct ProcessListItemResponse {
    process_id: String,
    sandbox_id: String,
    status: String,
    command: Vec<String>,
    started_at: String,
}

impl From<&ProcessInfo> for ProcessListItemResponse {
    fn from(value: &ProcessInfo) -> Self {
        Self {
            process_id: value.id.0.to_string(),
            sandbox_id: value.sandbox_id.0.to_string(),
            status: format!("{:?}", value.status).to_ascii_lowercase(),
            command: value.command.clone(),
            started_at: value.started_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ProcessStartResponse {
    process: ProcessInfoResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_isolation: Option<EffectiveIsolationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<ExplainDetails>,
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
        let outcome = run_sync_in_sandbox(&mut client, &sandbox.id, cmd.clone(), 60).await?;
        client.destroy_sandbox(&sandbox.id).await?;
        if i >= warmup {
            samples.push(outcome.duration_ms);
        }
    }

    Ok(summarize_samples(samples, warmup))
}

async fn bench_snapshot_remote(
    server_url: Option<String>,
    config: SandboxConfig,
    mutate_cmd: String,
    verify_cmd: String,
    warmup: usize,
    runs: usize,
    timeout_secs: u64,
    keep_snapshot_artifacts: bool,
) -> anyhow::Result<SnapshotBenchSummary> {
    let mut client = connect_client(server_url, true).await?;
    let total_runs = warmup + runs;

    let mut create_samples = Vec::with_capacity(runs);
    let mut mutate_samples = Vec::with_capacity(runs);
    let mut snapshot_create_samples = Vec::with_capacity(runs);
    let mut destroy_initial_samples = Vec::with_capacity(runs);
    let mut restore_verify_samples = Vec::with_capacity(runs);
    let mut destroy_restored_samples = Vec::with_capacity(runs);
    let mut total_samples = Vec::with_capacity(runs);
    let mut raw_runs = Vec::with_capacity(runs);

    for i in 0..total_runs {
        let run_number = i + 1;
        let affinity_name = format!("benchsnap-{}", uuid::Uuid::new_v4().simple());
        let mut created_sandbox_id: Option<SandboxId> = None;
        let mut restored_sandbox_id: Option<SandboxId> = None;
        let mut snapshot_id: Option<SnapshotId> = None;

        let mut run_config = config.clone();
        run_config.affinity_name = Some(affinity_name.clone());

        let run_result: anyhow::Result<SnapshotBenchRun> = async {
            let create_started = Instant::now();
            let created_sandbox = client.create_sandbox(run_config).await?;
            let create_ms = create_started.elapsed().as_millis();
            created_sandbox_id = Some(created_sandbox.id.clone());

            let mutate_started = Instant::now();
            let mutate_outcome = run_sync_in_sandbox(
                &mut client,
                &created_sandbox.id,
                mutate_cmd.clone(),
                timeout_secs,
            )
            .await?;
            if mutate_outcome.exit_code != 0 {
                bail!(
                    "snapshot benchmark mutate command failed (run={} sandbox_id={} exit={}): {}",
                    run_number,
                    created_sandbox.id.0,
                    mutate_outcome.exit_code,
                    mutate_outcome.stderr
                );
            }
            let mutate_ms = mutate_started.elapsed().as_millis();

            let snapshot_started = Instant::now();
            let (created_snapshot_id, _) =
                client.create_snapshot(&created_sandbox.id, None).await?;
            let snapshot_create_ms = snapshot_started.elapsed().as_millis();
            snapshot_id = Some(created_snapshot_id.clone());

            let destroy_initial_started = Instant::now();
            client.destroy_sandbox(&created_sandbox.id).await?;
            created_sandbox_id = None;
            let destroy_initial_ms = destroy_initial_started.elapsed().as_millis();

            let restore_verify_started = Instant::now();
            let (restored_sandbox, _) = client.resolve_affinity(&affinity_name, true).await?;
            restored_sandbox_id = Some(restored_sandbox.id.clone());
            let verify_outcome = run_sync_in_sandbox(
                &mut client,
                &restored_sandbox.id,
                verify_cmd.clone(),
                timeout_secs,
            )
            .await?;
            if verify_outcome.exit_code != 0 {
                bail!(
                    "snapshot benchmark verify command failed (run={} sandbox_id={} exit={}): {}",
                    run_number,
                    restored_sandbox.id.0,
                    verify_outcome.exit_code,
                    verify_outcome.stderr
                );
            }
            let restore_verify_ms = restore_verify_started.elapsed().as_millis();

            let destroy_restored_started = Instant::now();
            client.destroy_sandbox(&restored_sandbox.id).await?;
            restored_sandbox_id = None;
            let destroy_restored_ms = destroy_restored_started.elapsed().as_millis();

            let total_ms = create_ms
                + mutate_ms
                + snapshot_create_ms
                + destroy_initial_ms
                + restore_verify_ms
                + destroy_restored_ms;

            Ok(SnapshotBenchRun {
                run: run_number,
                sandbox_id: created_sandbox.id.0.to_string(),
                restored_sandbox_id: restored_sandbox.id.0.to_string(),
                snapshot_id: created_snapshot_id.0.to_string(),
                create_ms,
                mutate_ms,
                snapshot_create_ms,
                destroy_initial_ms,
                restore_verify_ms,
                destroy_restored_ms,
                total_ms,
            })
        }
        .await;

        if let Some(id) = restored_sandbox_id {
            let _ = client.destroy_sandbox(&id).await;
        }
        if let Some(id) = created_sandbox_id {
            let _ = client.destroy_sandbox(&id).await;
        }
        if !keep_snapshot_artifacts {
            if let Some(id) = &snapshot_id {
                let _ = cleanup_local_snapshot_artifact(id).await;
            }
        }

        let mut run = run_result?;
        if i >= warmup {
            run.run = i - warmup + 1;
            create_samples.push(run.create_ms);
            mutate_samples.push(run.mutate_ms);
            snapshot_create_samples.push(run.snapshot_create_ms);
            destroy_initial_samples.push(run.destroy_initial_ms);
            restore_verify_samples.push(run.restore_verify_ms);
            destroy_restored_samples.push(run.destroy_restored_ms);
            total_samples.push(run.total_ms);
            raw_runs.push(run);
        }
    }

    Ok(SnapshotBenchSummary {
        runs,
        warmup,
        create_ms: summarize_snapshot_samples(create_samples),
        mutate_ms: summarize_snapshot_samples(mutate_samples),
        snapshot_create_ms: summarize_snapshot_samples(snapshot_create_samples),
        destroy_initial_ms: summarize_snapshot_samples(destroy_initial_samples),
        restore_verify_ms: summarize_snapshot_samples(restore_verify_samples),
        destroy_restored_ms: summarize_snapshot_samples(destroy_restored_samples),
        total_ms: summarize_snapshot_samples(total_samples),
        raw_runs,
    })
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

fn summarize_snapshot_samples(mut samples: Vec<u128>) -> SnapshotBenchStageSummary {
    samples.sort_unstable();
    let runs = samples.len();
    let sum: u128 = samples.iter().copied().sum();
    let mean_ms = if runs == 0 {
        0.0
    } else {
        (sum as f64) / (runs as f64)
    };

    SnapshotBenchStageSummary {
        mean_ms,
        p50_ms: percentile(&samples, 50),
        p95_ms: percentile(&samples, 95),
        min_ms: samples.first().copied().unwrap_or_default(),
        max_ms: samples.last().copied().unwrap_or_default(),
    }
}

async fn cleanup_local_snapshot_artifact(snapshot_id: &SnapshotId) -> anyhow::Result<()> {
    let root = if let Ok(value) = std::env::var("HYPERBOX_SNAPSHOT_ROOT") {
        std::path::PathBuf::from(value)
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".hyperbox/snapshots")
    } else {
        std::env::temp_dir().join("hyperbox/snapshots")
    };
    let artifact = root.join(format!("{}.tar.gz", snapshot_id.0));
    if artifact.exists() {
        tokio::fs::remove_file(artifact).await?;
    }
    Ok(())
}

fn print_snapshot_bench_summary(summary: &SnapshotBenchSummary) {
    println!("runs={} warmup={}", summary.runs, summary.warmup);
    print_snapshot_stage("create_ms", &summary.create_ms);
    print_snapshot_stage("mutate_ms", &summary.mutate_ms);
    print_snapshot_stage("snapshot_create_ms", &summary.snapshot_create_ms);
    print_snapshot_stage("destroy_initial_ms", &summary.destroy_initial_ms);
    print_snapshot_stage("restore_verify_ms", &summary.restore_verify_ms);
    print_snapshot_stage("destroy_restored_ms", &summary.destroy_restored_ms);
    print_snapshot_stage("total_ms", &summary.total_ms);
}

fn print_snapshot_stage(name: &str, stats: &SnapshotBenchStageSummary) {
    println!(
        "{} mean_ms={:.2} p50_ms={} p95_ms={} min_ms={} max_ms={}",
        name, stats.mean_ms, stats.p50_ms, stats.p95_ms, stats.min_ms, stats.max_ms
    );
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
    use std::fs;

    use super::{
        NetworkArg, extract_container_bin_from_helper_argv, helper_argv_is_builtin_apple_helper,
        network_enforcement_status, resolve_network_policy,
        writable_scope_from_workspace_and_writes,
    };
    use hyperbox_core::{Allowlist, NetworkMode};
    use hyperbox_server::ServerInfo;
    use uuid::Uuid;

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

    #[test]
    fn network_enforcement_marks_local_as_not_enforced() {
        let server_info = ServerInfo {
            server_version: "x".to_string(),
            process_id: "1".to_string(),
            executable_path: "/tmp/hyperbox".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            backend_requested: "auto".to_string(),
            backend_selected: "local".to_string(),
            backend_reason: "selected via local".to_string(),
            apple_runtime: None,
            apple_helper_argv: vec![],
        };

        let (status, reason) =
            network_enforcement_status(Some(&server_info), Some(&NetworkMode::None));
        assert_eq!(status, "not_enforced");
        assert!(reason.is_some());
    }

    #[test]
    fn network_enforcement_accepts_builtin_apple_allowlist() {
        let server_info = ServerInfo {
            server_version: "x".to_string(),
            process_id: "1".to_string(),
            executable_path: "/tmp/hyperbox".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            backend_requested: "auto".to_string(),
            backend_selected: "apple".to_string(),
            backend_reason: "selected via auto".to_string(),
            apple_runtime: Some("containerization".to_string()),
            apple_helper_argv: vec!["hyperbox".to_string(), "apple-helper".to_string()],
        };

        let (status, reason) = network_enforcement_status(
            Some(&server_info),
            Some(&NetworkMode::Allowlist(
                Allowlist::parse(&["example.com".to_string()]).expect("allowlist"),
            )),
        );
        assert_eq!(status, "enforced");
        assert!(reason.is_some());
    }

    #[test]
    fn network_enforcement_accepts_external_virtualization_allowlist() {
        let server_info = ServerInfo {
            server_version: "x".to_string(),
            process_id: "1".to_string(),
            executable_path: "/tmp/hyperbox".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            backend_requested: "auto".to_string(),
            backend_selected: "apple".to_string(),
            backend_reason: "selected via auto".to_string(),
            apple_runtime: Some("virtualization".to_string()),
            apple_helper_argv: vec!["external-helper".to_string()],
        };

        let (status, reason) = network_enforcement_status(
            Some(&server_info),
            Some(&NetworkMode::Allowlist(
                Allowlist::parse(&["example.com".to_string()]).expect("allowlist"),
            )),
        );
        assert_eq!(status, "enforced");
        assert!(reason.is_some());
    }

    #[test]
    fn writable_scope_includes_workspace_and_write_paths_without_duplicates() {
        let writable = writable_scope_from_workspace_and_writes(
            Some("/tmp/workspace"),
            &[
                "output.txt=1".to_string(),
                "output.txt=2".to_string(),
                "state/cache.txt=3".to_string(),
            ],
        );
        assert!(writable.contains(&"/tmp/workspace".to_string()));
        assert!(writable.contains(&"output.txt".to_string()));
        assert!(writable.contains(&"state/cache.txt".to_string()));
        assert_eq!(
            writable
                .iter()
                .filter(|p| p.as_str() == "output.txt")
                .count(),
            1
        );
    }

    #[test]
    fn resolve_network_policy_defaults_to_none() {
        let resolved = resolve_network_policy(None, None, vec![], None).expect("default policy");
        assert!(matches!(resolved.network_mode, NetworkMode::None));
        assert!(resolved.profile_label.is_none());
    }

    #[test]
    fn resolve_network_policy_rejects_empty_allowlist() {
        let err = resolve_network_policy(None, Some(NetworkArg::Allowlist), vec![], None)
            .expect_err("empty allowlist should fail");
        assert!(err.to_string().contains("at least one domain"));
    }

    #[test]
    fn resolve_network_policy_accepts_wildcard_allowlist_entries() {
        let resolved = resolve_network_policy(
            None,
            Some(NetworkArg::Allowlist),
            vec!["*.example.com".to_string()],
            None,
        )
        .expect("wildcards should be accepted");
        match resolved.network_mode {
            NetworkMode::Allowlist(domains) => {
                assert_eq!(domains.to_strings(), vec!["*.example.com"]);
            }
            other => panic!("expected allowlist mode, got {other:?}"),
        }
    }

    #[test]
    fn resolve_network_policy_normalizes_allowlist_entries() {
        let resolved = resolve_network_policy(
            None,
            Some(NetworkArg::Allowlist),
            vec!["Example.com".to_string(), "example.com".to_string()],
            None,
        )
        .expect("allowlist should normalize");
        let domains = match resolved.network_mode {
            NetworkMode::Allowlist(domains) => domains,
            other => panic!("expected allowlist mode, got {other:?}"),
        };
        assert_eq!(domains.to_strings(), vec!["example.com".to_string()]);
    }

    #[test]
    fn resolve_network_policy_web_profile_requires_allow_entries() {
        let err = resolve_network_policy(Some("web"), None, vec![], None)
            .expect_err("web profile without allowlist should fail");
        assert!(err.to_string().contains("at least one domain"));
    }

    #[test]
    fn resolve_network_policy_full_profile_rejects_allow_entries() {
        let err = resolve_network_policy(
            Some("full"),
            Some(NetworkArg::Full),
            vec!["example.com".to_string()],
            None,
        )
        .expect_err("full profile should reject allowlist entries");
        assert!(err.to_string().contains("--allow requires allowlist"));
    }

    #[test]
    fn resolve_network_policy_profile_web_maps_to_allowlist() {
        let resolved = resolve_network_policy(
            Some("web"),
            Some(NetworkArg::Allowlist),
            vec!["example.com".to_string()],
            None,
        )
        .expect("web profile");
        assert!(matches!(resolved.network_mode, NetworkMode::Allowlist(_)));
        assert_eq!(resolved.profile_label.as_deref(), Some("web"));
    }

    #[test]
    fn resolve_network_policy_allows_profile_plus_override_mix_and_match() {
        let resolved = resolve_network_policy(
            Some("locked"),
            Some(NetworkArg::Allowlist),
            vec!["example.com".to_string()],
            None,
        )
        .expect("mixed profile + override");
        assert!(matches!(resolved.network_mode, NetworkMode::Allowlist(_)));
        assert_eq!(resolved.profile_label.as_deref(), Some("locked"));
    }

    #[test]
    fn resolve_network_policy_supports_custom_profiles_from_toml() {
        let path =
            std::env::temp_dir().join(format!("hyperbox-profile-test-{}.toml", Uuid::new_v4()));
        fs::write(
            &path,
            r#"
[profiles.team_web]
network = "allowlist"
allow = ["github.com", "pypi.org"]
"#,
        )
        .expect("write profile config");

        let resolved = resolve_network_policy(Some("team_web"), None, vec![], Some(&path))
            .expect("custom profile from toml");
        let domains = match resolved.network_mode {
            NetworkMode::Allowlist(domains) => domains,
            other => panic!("expected allowlist mode, got {other:?}"),
        };
        assert_eq!(
            domains.to_strings(),
            vec!["github.com".to_string(), "pypi.org".to_string()]
        );
        assert_eq!(resolved.profile_label.as_deref(), Some("team_web"));

        let _ = fs::remove_file(path);
    }
}
