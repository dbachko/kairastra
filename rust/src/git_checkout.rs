use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

pub fn checkout_git_common_dir(checkout: &Path) -> Result<Option<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .with_context(|| format!("failed to inspect {}", checkout.display()))?;
    if !output.status.success() {
        return Ok(None);
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }

    let common_dir = if Path::new(&value).is_absolute() {
        PathBuf::from(value)
    } else {
        checkout.join(value)
    };

    Ok(Some(common_dir.canonicalize().ok().unwrap_or(common_dir)))
}
