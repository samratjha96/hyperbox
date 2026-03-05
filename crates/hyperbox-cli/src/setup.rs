use std::{
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

pub fn run_setup() -> anyhow::Result<()> {
    if std::env::consts::OS != "macos" {
        bail!("`hyperbox setup` currently supports macOS only");
    }

    let caps = hyperbox_apple::detect_macos_capabilities();
    if !caps.is_apple_silicon {
        bail!("Apple backend requires Apple Silicon host");
    }
    if caps.major_version < 26 {
        bail!(
            "Apple Containerization runtime requires macOS 26+; detected {}",
            caps.version
        );
    }

    if command_exists("container") {
        println!("container CLI already installed; ensuring runtime is started...");
        ensure_container_system_started()?;
        print_setup_success();
        return Ok(());
    }

    println!("Installing Apple container runtime...");
    let release = fetch_latest_container_release()?;
    let asset = pick_signed_pkg_asset(&release)
        .ok_or_else(|| anyhow!("latest apple/container release has no signed installer pkg"))?;

    let pkg_path = std::env::temp_dir().join(format!(
        "container-{}-installer-signed.pkg",
        release.tag_name
    ));

    run_live(
        Command::new("curl")
            .arg("-fL")
            .arg(&asset.browser_download_url)
            .arg("-o")
            .arg(&pkg_path),
        "download signed Apple container installer pkg",
    )?;

    if let Some(expected_sha) = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
    {
        verify_sha256_with_shasum(&pkg_path, expected_sha)
            .with_context(|| format!("verify installer checksum for {}", pkg_path.display()))?;
    }

    println!("Installing pkg via sudo installer...");
    run_live(
        Command::new("sudo")
            .arg("installer")
            .arg("-pkg")
            .arg(&pkg_path)
            .arg("-target")
            .arg("/"),
        "install Apple container pkg",
    )?;

    ensure_container_system_started()?;
    print_setup_success();
    Ok(())
}

fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ensure_container_system_started() -> anyhow::Result<()> {
    run_live(
        Command::new("container").arg("system").arg("start"),
        "start container system",
    )?;
    run_live(
        Command::new("container").arg("system").arg("status"),
        "check container system status",
    )?;
    Ok(())
}

fn fetch_latest_container_release() -> anyhow::Result<GitHubRelease> {
    let output = Command::new("curl")
        .arg("-fsSL")
        .arg("https://api.github.com/repos/apple/container/releases/latest")
        .output()
        .context("query latest apple/container release metadata")?;

    if !output.status.success() {
        bail!(
            "failed to fetch latest apple/container release metadata: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice::<GitHubRelease>(&output.stdout)
        .context("parse latest apple/container release metadata")
}

fn pick_signed_pkg_asset(release: &GitHubRelease) -> Option<&GitHubAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with("installer-signed.pkg"))
}

fn verify_sha256_with_shasum(path: &Path, expected: &str) -> anyhow::Result<()> {
    let output = Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .with_context(|| format!("compute sha256 for {}", path.display()))?;
    if !output.status.success() {
        bail!(
            "shasum failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("unexpected shasum output"))?;

    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        bail!(
            "checksum mismatch: expected {expected}, got {actual} for {}",
            path.display()
        );
    }
}

fn run_live(cmd: &mut Command, context: &str) -> anyhow::Result<()> {
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd
        .status()
        .with_context(|| format!("{context}: spawn command"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{context}: command exited with {status}")
    }
}

fn print_setup_success() {
    println!("hyperbox setup complete.");
    println!("Next: restart server and run `hyperbox create` / `hyperbox shell`.");
}

#[cfg(test)]
mod tests {
    use super::{GitHubAsset, GitHubRelease, pick_signed_pkg_asset};

    #[test]
    fn picks_signed_pkg_from_release_assets() {
        let release = GitHubRelease {
            tag_name: "0.10.0".to_string(),
            assets: vec![
                GitHubAsset {
                    name: "container-dSYM.zip".to_string(),
                    browser_download_url: "https://example.invalid/dsym".to_string(),
                    digest: None,
                },
                GitHubAsset {
                    name: "container-0.10.0-installer-signed.pkg".to_string(),
                    browser_download_url: "https://example.invalid/pkg".to_string(),
                    digest: Some("sha256:abc".to_string()),
                },
            ],
        };

        let asset = pick_signed_pkg_asset(&release).expect("signed pkg asset");
        assert!(asset.name.ends_with("installer-signed.pkg"));
    }
}
