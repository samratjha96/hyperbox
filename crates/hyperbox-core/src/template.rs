use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{HyperboxError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub description: String,
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

#[cfg(test)]
mod tests {
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
}
