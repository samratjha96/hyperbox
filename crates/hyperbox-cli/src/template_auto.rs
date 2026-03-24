use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateReason {
    Explicit,
    CommandHintRust,
    CommandHintGo,
    CommandHintNode,
    CommandHintPython,
    WorkspaceCargoToml,
    WorkspaceGoMod,
    WorkspaceNodeManifest,
    WorkspacePythonManifest,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TemplateRule {
    template: &'static str,
    reason: TemplateReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandRule {
    matchers: &'static [&'static str],
    template: TemplateRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceRule {
    manifests: &'static [&'static str],
    template: TemplateRule,
}

impl TemplateReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::CommandHintRust => "command_hint_rust",
            Self::CommandHintGo => "command_hint_go",
            Self::CommandHintNode => "command_hint_node",
            Self::CommandHintPython => "command_hint_python",
            Self::WorkspaceCargoToml => "workspace_cargo_toml",
            Self::WorkspaceGoMod => "workspace_go_mod",
            Self::WorkspaceNodeManifest => "workspace_node_manifest",
            Self::WorkspacePythonManifest => "workspace_python_manifest",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateResolution {
    pub template: String,
    pub reason: TemplateReason,
}

const COMMAND_RULES: &[CommandRule] = &[
    CommandRule {
        matchers: &["cargo ", "cargo\n", "cargo\t", "cargo-", "rustc"],
        template: TemplateRule {
            template: "rust:1.75",
            reason: TemplateReason::CommandHintRust,
        },
    },
    CommandRule {
        matchers: &["go ", "go\n", "go\t", "go test", "go build", "go run"],
        template: TemplateRule {
            template: "golang:1.22",
            reason: TemplateReason::CommandHintGo,
        },
    },
    CommandRule {
        matchers: &["npm ", "node ", "pnpm ", "yarn ", "npx ", "bun ", "tsx "],
        template: TemplateRule {
            template: "node:20",
            reason: TemplateReason::CommandHintNode,
        },
    },
    CommandRule {
        matchers: &["python", "pip ", "pytest", "uv ", "poetry ", "ipython"],
        template: TemplateRule {
            template: "python:3.12",
            reason: TemplateReason::CommandHintPython,
        },
    },
];

const WORKSPACE_RULES: &[WorkspaceRule] = &[
    WorkspaceRule {
        manifests: &["Cargo.toml"],
        template: TemplateRule {
            template: "rust:1.75",
            reason: TemplateReason::WorkspaceCargoToml,
        },
    },
    WorkspaceRule {
        manifests: &["go.mod"],
        template: TemplateRule {
            template: "golang:1.22",
            reason: TemplateReason::WorkspaceGoMod,
        },
    },
    WorkspaceRule {
        manifests: &[
            "package.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "package-lock.json",
            "bun.lockb",
        ],
        template: TemplateRule {
            template: "node:20",
            reason: TemplateReason::WorkspaceNodeManifest,
        },
    },
    WorkspaceRule {
        manifests: &[
            "pyproject.toml",
            "requirements.txt",
            "requirements-dev.txt",
            "setup.py",
            "Pipfile",
            "poetry.lock",
        ],
        template: TemplateRule {
            template: "python:3.12",
            reason: TemplateReason::WorkspacePythonManifest,
        },
    },
];

pub fn resolve_template(
    template_arg: &str,
    workspace: &str,
    command_hint: &str,
) -> TemplateResolution {
    if !template_arg.eq_ignore_ascii_case("auto") {
        return resolved(template_arg, TemplateReason::Explicit);
    }

    if let Some(detected) = detect_from_command_hint(command_hint) {
        return detected;
    }

    if let Some(detected) = detect_from_workspace(Path::new(workspace)) {
        return detected;
    }

    resolved("python:3.12", TemplateReason::Fallback)
}

fn resolved(template: &str, reason: TemplateReason) -> TemplateResolution {
    TemplateResolution {
        template: template.to_string(),
        reason,
    }
}

fn detect_from_command_hint(command_hint: &str) -> Option<TemplateResolution> {
    if command_hint.is_empty() {
        return None;
    }
    let hint = command_hint.to_ascii_lowercase();

    for rule in COMMAND_RULES {
        if contains_any(&hint, rule.matchers) {
            return Some(resolved(rule.template.template, rule.template.reason));
        }
    }

    None
}

fn detect_from_workspace(root: &Path) -> Option<TemplateResolution> {
    for rule in WORKSPACE_RULES {
        if rule
            .manifests
            .iter()
            .any(|manifest| root.join(manifest).exists())
        {
            return Some(resolved(rule.template.template, rule.template.reason));
        }
    }

    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::{TemplateReason, resolve_template};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn resolves_explicit_template_without_detection() {
        let resolved = resolve_template("python:3.11", ".", "cargo test");
        assert_eq!(resolved.template, "python:3.11");
        assert_eq!(resolved.reason, TemplateReason::Explicit);
    }

    #[test]
    fn resolves_auto_template_from_command_hint() {
        let resolved = resolve_template("auto", ".", "cargo test --workspace");
        assert_eq!(resolved.template, "rust:1.75");
        assert_eq!(resolved.reason, TemplateReason::CommandHintRust);
    }

    #[test]
    fn resolves_auto_template_from_workspace_manifest() {
        let workspace = unique_temp_dir("hyperbox-template-detect");
        fs::write(workspace.join("go.mod"), "module example.com/demo\n").expect("write go.mod");
        let resolved = resolve_template("auto", workspace.to_string_lossy().as_ref(), "echo hi");
        assert_eq!(resolved.template, "golang:1.22");
        assert_eq!(resolved.reason, TemplateReason::WorkspaceGoMod);
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn resolves_auto_template_to_python_fallback() {
        let workspace = unique_temp_dir("hyperbox-template-fallback");
        let resolved = resolve_template("auto", workspace.to_string_lossy().as_ref(), "echo hi");
        assert_eq!(resolved.template, "python:3.12");
        assert_eq!(resolved.reason, TemplateReason::Fallback);
        let _ = fs::remove_dir_all(workspace);
    }
}
