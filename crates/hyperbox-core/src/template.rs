use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{HyperboxError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateManifest {
    pub name: String,
    pub description: String,
    pub rootfs: String,
    pub kernel: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TemplateRegistry {
    templates: BTreeMap<String, Template>,
}

impl TemplateRegistry {
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        for (name, desc) in [
            ("python:3.11", "Python 3.11 with pip"),
            ("python:3.12", "Python 3.12 with pip"),
            ("node:18", "Node.js 18 with npm"),
            ("node:20", "Node.js 20 with npm"),
            ("golang:1.22", "Go 1.22 toolchain"),
            ("rust:1.75", "Rust stable toolchain"),
            ("ubuntu:22.04", "Ubuntu base userspace"),
        ] {
            registry.insert(Template {
                name: name.to_string(),
                description: desc.to_string(),
            });
        }

        registry
    }

    pub fn insert(&mut self, template: Template) {
        self.templates.insert(template.name.clone(), template);
    }

    pub fn get(&self, name: &str) -> Option<&Template> {
        self.templates.get(name)
    }

    pub fn list(&self) -> Vec<&Template> {
        self.templates.values().collect()
    }

    pub fn ensure_exists(&self, name: &str) -> Result<()> {
        if self.templates.contains_key(name) {
            Ok(())
        } else {
            Err(HyperboxError::TemplateNotFound(name.to_string()))
        }
    }
}

pub fn load_template_manifests(root: &Path) -> Result<Vec<TemplateManifest>> {
    let mut manifests = Vec::new();

    let entries = fs::read_dir(root)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("template.json");
        if !manifest_path.exists() {
            continue;
        }

        let raw = fs::read_to_string(&manifest_path)?;
        let manifest: TemplateManifest = serde_json::from_str(&raw)?;
        manifests.push(manifest);
    }

    manifests.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn has_expected_default_template() {
        let templates = TemplateRegistry::with_defaults();
        assert!(templates.get("python:3.12").is_some());
    }

    #[test]
    fn unknown_template_returns_error() {
        let templates = TemplateRegistry::with_defaults();
        let err = templates.ensure_exists("missing:1").unwrap_err();
        assert_eq!(format!("{err}"), "template not found: missing:1");
    }

    #[test]
    fn loads_manifests_from_disk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let t_dir = temp.path().join("python-3.12");
        std::fs::create_dir_all(&t_dir).expect("create template dir");
        let mut file = std::fs::File::create(t_dir.join("template.json")).expect("create manifest");
        write!(
            file,
            "{{\"name\":\"python:3.12\",\"description\":\"Python\",\"rootfs\":\"rootfs.ext4\",\"kernel\":null,\"tags\":[\"python\"]}}"
        )
        .expect("write manifest");

        let manifests = load_template_manifests(temp.path()).expect("load manifests");
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "python:3.12");
    }
}
