use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateResolution {
    pub template: String,
    pub reason: String,
}

pub fn resolve_template(
    template_arg: &str,
    workspace: &str,
    command_hint: &str,
) -> TemplateResolution {
    if !template_arg.eq_ignore_ascii_case("auto") {
        return TemplateResolution {
            template: template_arg.to_string(),
            reason: "explicit".to_string(),
        };
    }

    if let Some((template, reason)) = detect_from_command_hint(command_hint) {
        return TemplateResolution {
            template: template.to_string(),
            reason: reason.to_string(),
        };
    }

    if let Some((template, reason)) = detect_from_workspace(Path::new(workspace)) {
        return TemplateResolution {
            template: template.to_string(),
            reason: reason.to_string(),
        };
    }

    TemplateResolution {
        template: "python:3.12".to_string(),
        reason: "fallback".to_string(),
    }
}

fn detect_from_command_hint(command_hint: &str) -> Option<(&'static str, &'static str)> {
    if command_hint.is_empty() {
        return None;
    }
    let hint = command_hint.to_ascii_lowercase();

    if contains_any(&hint, &["cargo ", "cargo\n", "cargo\t", "cargo-", "rustc"]) {
        return Some(("rust:1.75", "command_hint_rust"));
    }
    if contains_any(
        &hint,
        &["go ", "go\n", "go\t", "go test", "go build", "go run"],
    ) {
        return Some(("golang:1.22", "command_hint_go"));
    }
    if contains_any(
        &hint,
        &["npm ", "node ", "pnpm ", "yarn ", "npx ", "bun ", "tsx "],
    ) {
        return Some(("node:20", "command_hint_node"));
    }
    if contains_any(
        &hint,
        &["python", "pip ", "pytest", "uv ", "poetry ", "ipython"],
    ) {
        return Some(("python:3.12", "command_hint_python"));
    }

    None
}

fn detect_from_workspace(root: &Path) -> Option<(&'static str, &'static str)> {
    if root.join("Cargo.toml").exists() {
        return Some(("rust:1.75", "workspace_cargo_toml"));
    }
    if root.join("go.mod").exists() {
        return Some(("golang:1.22", "workspace_go_mod"));
    }
    if root.join("package.json").exists()
        || root.join("pnpm-lock.yaml").exists()
        || root.join("yarn.lock").exists()
        || root.join("package-lock.json").exists()
        || root.join("bun.lockb").exists()
    {
        return Some(("node:20", "workspace_node_manifest"));
    }
    if root.join("pyproject.toml").exists()
        || root.join("requirements.txt").exists()
        || root.join("requirements-dev.txt").exists()
        || root.join("setup.py").exists()
        || root.join("Pipfile").exists()
        || root.join("poetry.lock").exists()
    {
        return Some(("python:3.12", "workspace_python_manifest"));
    }

    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::resolve_template;
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
        assert_eq!(resolved.reason, "explicit");
    }

    #[test]
    fn resolves_auto_template_from_command_hint() {
        let resolved = resolve_template("auto", ".", "cargo test --workspace");
        assert_eq!(resolved.template, "rust:1.75");
        assert_eq!(resolved.reason, "command_hint_rust");
    }

    #[test]
    fn resolves_auto_template_from_workspace_manifest() {
        let workspace = unique_temp_dir("hyperbox-template-detect");
        fs::write(workspace.join("go.mod"), "module example.com/demo\n").expect("write go.mod");
        let resolved = resolve_template("auto", workspace.to_string_lossy().as_ref(), "echo hi");
        assert_eq!(resolved.template, "golang:1.22");
        assert_eq!(resolved.reason, "workspace_go_mod");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn resolves_auto_template_to_python_fallback() {
        let workspace = unique_temp_dir("hyperbox-template-fallback");
        let resolved = resolve_template("auto", workspace.to_string_lossy().as_ref(), "echo hi");
        assert_eq!(resolved.template, "python:3.12");
        assert_eq!(resolved.reason, "fallback");
        let _ = fs::remove_dir_all(workspace);
    }
}
