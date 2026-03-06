use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    None,
    Full,
    Allowlist(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxConfig {
    pub affinity_name: Option<String>,
    pub template: String,
    pub memory_mb: u32,
    pub vcpu_count: u8,
    pub workspace_dir: Option<String>,
    pub network: NetworkMode,
    pub env: IndexMap<String, String>,
    pub timeout_secs: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            affinity_name: None,
            template: "python:3.12".to_string(),
            memory_mb: 512,
            vcpu_count: 1,
            workspace_dir: None,
            network: NetworkMode::None,
            env: IndexMap::new(),
            timeout_secs: 60,
        }
    }
}
