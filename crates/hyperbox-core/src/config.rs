use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistEntry {
    Exact(String),
    WildcardSubdomain(String),
}

impl AllowlistEntry {
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        let domain = raw.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Err("allowlist domains must be non-empty".to_string());
        }
        if domain.contains('/') || domain.contains(':') || domain.contains(char::is_whitespace) {
            return Err(format!(
                "allowlist entry `{domain}` must be a bare domain (no scheme, port, path, or spaces)"
            ));
        }

        let entry = if let Some(suffix) = domain.strip_prefix("*.") {
            if suffix.is_empty() || suffix.contains('*') {
                return Err(format!(
                    "allowlist entry `{domain}` has an invalid wildcard; use leading `*.` only"
                ));
            }
            Self::WildcardSubdomain(suffix.to_string())
        } else {
            if domain.contains('*') {
                return Err(format!(
                    "allowlist entry `{domain}` has an invalid wildcard; use leading `*.` only"
                ));
            }
            Self::Exact(domain.clone())
        };

        let hostname = match &entry {
            Self::Exact(hostname) | Self::WildcardSubdomain(hostname) => hostname,
        };
        if hostname.starts_with('.') || hostname.ends_with('.') || hostname.contains("..") {
            return Err(format!("allowlist entry `{domain}` is not a valid domain"));
        }
        if !hostname
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        {
            return Err(format!(
                "allowlist entry `{domain}` contains unsupported characters"
            ));
        }

        Ok(entry)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Exact(hostname) => hostname,
            Self::WildcardSubdomain(suffix) => suffix,
        }
    }

    pub fn is_wildcard(&self) -> bool {
        matches!(self, Self::WildcardSubdomain(_))
    }

    pub fn to_pattern_string(&self) -> String {
        match self {
            Self::Exact(hostname) => hostname.clone(),
            Self::WildcardSubdomain(suffix) => format!("*.{suffix}"),
        }
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            Self::Exact(exact) => &host == exact,
            Self::WildcardSubdomain(suffix) => {
                host.ends_with(suffix)
                    && host
                        .strip_suffix(suffix)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
        }
    }
}

impl Serialize for AllowlistEntry {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_pattern_string())
    }
}

impl<'de> Deserialize<'de> for AllowlistEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Allowlist(Vec<AllowlistEntry>);

impl Allowlist {
    pub fn parse(domains: &[String]) -> std::result::Result<Self, String> {
        let mut entries = Vec::with_capacity(domains.len());
        let mut seen = HashSet::new();

        for raw in domains {
            let entry = AllowlistEntry::parse(raw)?;
            let normalized = entry.to_pattern_string();
            if seen.insert(normalized) {
                entries.push(entry);
            }
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[AllowlistEntry] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn allows(&self, host: &str) -> bool {
        self.0.iter().any(|entry| entry.matches(host))
    }

    pub fn to_strings(&self) -> Vec<String> {
        self.0
            .iter()
            .map(AllowlistEntry::to_pattern_string)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    None,
    Full,
    Allowlist(Allowlist),
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
    Allowlist::parse(domains).map(|allowlist| allowlist.to_strings())
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
    use super::{Allowlist, AllowlistEntry, normalize_allowlist_domains};

    #[test]
    fn wildcard_requires_subdomain() {
        let entry = AllowlistEntry::parse("*.github.com").expect("parse wildcard");
        assert!(entry.matches("api.github.com"));
        assert!(!entry.matches("github.com"));
    }

    #[test]
    fn allowlist_allows_exact_and_wildcard_patterns() {
        let allowlist =
            Allowlist::parse(&["api.openai.com".to_string(), "*.github.com".to_string()])
                .expect("allowlist should parse");
        assert!(allowlist.allows("api.openai.com"));
        assert!(allowlist.allows("gist.github.com"));
        assert!(!allowlist.allows("github.com"));
    }

    #[test]
    fn normalize_allowlist_accepts_wildcard_subdomains() {
        let domains = normalize_allowlist_domains(&["*.Example.com".to_string()])
            .expect("wildcard subdomains should normalize");
        assert_eq!(domains, vec!["*.example.com"]);
    }

    #[test]
    fn normalize_allowlist_rejects_invalid_wildcards() {
        let err = normalize_allowlist_domains(&["*example.com".to_string()])
            .expect_err("invalid wildcards must be rejected");
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
