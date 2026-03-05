use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;

use hyperbox_core::NetworkMode;

#[derive(Debug, Clone)]
pub struct VmNetworkSpec {
    pub vm_id: String,
    pub tap_name: String,
    pub host_iface: String,
    pub guest_cidr: String,
    pub guest_ip: String,
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn run(&self, cmd: CommandSpec) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Default)]
pub struct RecordingExecutor {
    pub commands: Arc<Mutex<Vec<CommandSpec>>>,
}

#[async_trait::async_trait]
impl CommandExecutor for RecordingExecutor {
    async fn run(&self, cmd: CommandSpec) -> anyhow::Result<()> {
        self.commands.lock().await.push(cmd);
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ShellExecutor;

#[async_trait::async_trait]
impl CommandExecutor for ShellExecutor {
    async fn run(&self, cmd: CommandSpec) -> anyhow::Result<()> {
        let status = tokio::process::Command::new(&cmd.program)
            .args(&cmd.args)
            .status()
            .await
            .with_context(|| format!("spawn {}", cmd.program))?;

        if !status.success() {
            anyhow::bail!("command failed: {} {:?}", cmd.program, cmd.args);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct FirewallManager<E: CommandExecutor> {
    executor: E,
}

impl<E: CommandExecutor> FirewallManager<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }

    pub async fn apply(&self, spec: &VmNetworkSpec, mode: &NetworkMode) -> anyhow::Result<()> {
        let plan = build_apply_plan(spec, mode);
        for cmd in plan {
            self.executor.run(cmd).await?;
        }
        Ok(())
    }

    pub async fn teardown(&self, spec: &VmNetworkSpec) -> anyhow::Result<()> {
        for cmd in build_teardown_plan(spec) {
            self.executor.run(cmd).await?;
        }
        Ok(())
    }
}

pub fn build_apply_plan(spec: &VmNetworkSpec, mode: &NetworkMode) -> Vec<CommandSpec> {
    let mut commands = vec![
        CommandSpec {
            program: "ipset".to_string(),
            args: vec![
                "create".to_string(),
                format!("vm_{}_allowed", spec.vm_id),
                "hash:ip".to_string(),
                "timeout".to_string(),
                "300".to_string(),
                "-exist".to_string(),
            ],
        },
        CommandSpec {
            program: "nft".to_string(),
            args: vec!["add".to_string(), "table".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id)],
        },
        CommandSpec {
            program: "nft".to_string(),
            args: vec![
                "add".to_string(), "chain".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id),
                "forward".to_string(), "{".to_string(), "type".to_string(), "filter".to_string(),
                "hook".to_string(), "forward".to_string(), "priority".to_string(), "0".to_string(), ";".to_string(), "}".to_string(),
            ],
        },
    ];

    match mode {
        NetworkMode::None => commands.push(CommandSpec {
            program: "nft".to_string(),
            args: vec![
                "add".to_string(), "rule".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id),
                "forward".to_string(), "iifname".to_string(), spec.tap_name.clone(), "drop".to_string(),
            ],
        }),
        NetworkMode::Full => commands.push(CommandSpec {
            program: "nft".to_string(),
            args: vec![
                "add".to_string(), "rule".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id),
                "forward".to_string(), "iifname".to_string(), spec.tap_name.clone(), "accept".to_string(),
            ],
        }),
        NetworkMode::Allowlist(_) => {
            commands.push(CommandSpec {
                program: "nft".to_string(),
                args: vec![
                    "add".to_string(), "rule".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id),
                    "forward".to_string(), "iifname".to_string(), spec.tap_name.clone(),
                    "ip".to_string(), "daddr".to_string(), format!("@vm_{}_allowed", spec.vm_id), "accept".to_string(),
                ],
            });
            commands.push(CommandSpec {
                program: "nft".to_string(),
                args: vec![
                    "add".to_string(), "rule".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id),
                    "forward".to_string(), "iifname".to_string(), spec.tap_name.clone(), "drop".to_string(),
                ],
            });
        }
    }

    commands.push(CommandSpec {
        program: "nft".to_string(),
        args: vec![
            "add".to_string(), "rule".to_string(), "nat".to_string(), "postrouting".to_string(),
            "ip".to_string(), "saddr".to_string(), spec.guest_ip.clone(),
            "oifname".to_string(), spec.host_iface.clone(), "masquerade".to_string(),
        ],
    });

    commands
}

pub fn build_teardown_plan(spec: &VmNetworkSpec) -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            program: "nft".to_string(),
            args: vec!["delete".to_string(), "table".to_string(), "inet".to_string(), format!("vm_{}", spec.vm_id)],
        },
        CommandSpec {
            program: "ipset".to_string(),
            args: vec!["destroy".to_string(), format!("vm_{}_allowed", spec.vm_id)],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VmNetworkSpec {
        VmNetworkSpec {
            vm_id: "abc".to_string(),
            tap_name: "tap0".to_string(),
            host_iface: "eth0".to_string(),
            guest_cidr: "172.16.0.0/30".to_string(),
            guest_ip: "172.16.0.2".to_string(),
        }
    }

    #[test]
    fn allowlist_plan_contains_ipset_reference() {
        let plan = build_apply_plan(&spec(), &NetworkMode::Allowlist(vec!["api.openai.com".to_string()]));
        assert!(plan.iter().any(|c| c.args.iter().any(|a| a.contains("@vm_abc_allowed"))));
    }
}
