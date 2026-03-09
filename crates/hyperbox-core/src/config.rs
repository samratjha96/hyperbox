use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

pub fn normalize_allowlist_domains(domains: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut normalized = Vec::with_capacity(domains.len());
    let mut seen = HashSet::new();

    for raw in domains {
        let domain = raw.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Err("allowlist domains must be non-empty".to_string());
        }
        if domain.contains('*') {
            return Err("wildcard allowlist entries are not supported; use explicit domains".to_string());
        }
        if domain.contains('/') || domain.contains(':') || domain.contains(char::is_whitespace) {
            return Err(format!(
                "allowlist entry `{domain}` must be a bare domain (no scheme, port, path, or spaces)"
            ));
        }
        if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
            return Err(format!("allowlist entry `{domain}` is not a valid domain"));
        }
        if !domain
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        {
            return Err(format!(
                "allowlist entry `{domain}` contains unsupported characters"
            ));
        }
        if seen.insert(domain.clone()) {
            normalized.push(domain);
        }
    }

    Ok(normalized)
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

#[cfg(test)]
mod tests {
    use super::normalize_allowlist_domains;

    #[test]
    fn normalize_allowlist_rejects_wildcards() {
        let err = normalize_allowlist_domains(&["*.example.com".to_string()])
            .expect_err("wildcards must be rejected");
        assert!(err.contains("wildcard"));
    }

    #[test]
    fn normalize_allowlist_rejects_scheme_and_port() {
        let err = normalize_allowlist_domains(&["https://example.com:443".to_string()])
            .expect_err("scheme/port must be rejected");
        assert!(err.contains("bare domain"));
    }

    #[test]
    fn normalize_allowlist_normalizes_case_and_dedupes() {
        let domains = normalize_allowlist_domains(&[
            "Example.com".to_string(),
            "example.com".to_string(),
            "pypi.org".to_string(),
        ])
        .expect("domains should normalize");
        assert_eq!(domains, vec!["example.com", "pypi.org"]);
    }
}
