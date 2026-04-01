use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const SHARED_SKILLS_ROOT: &str = ".agents/skills";

#[derive(Debug, Clone, Copy)]
pub struct SharedSkillDir {
    pub relative_path: &'static str,
    pub display_name: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct SharedSkillAsset {
    pub skill_dir: &'static str,
    pub relative_path: &'static str,
    pub contents: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct SharedSkillPlan {
    pub missing_dirs: Vec<&'static SharedSkillDir>,
    pub outdated_dirs: Vec<&'static SharedSkillDir>,
}

impl SharedSkillPlan {
    pub fn is_empty(&self) -> bool {
        self.missing_dirs.is_empty() && self.outdated_dirs.is_empty()
    }

    pub fn missing_or_outdated_dirs(&self) -> Vec<&'static SharedSkillDir> {
        let mut combined = self.missing_dirs.clone();
        for dir in &self.outdated_dirs {
            if !combined
                .iter()
                .any(|existing| existing.relative_path == dir.relative_path)
            {
                combined.push(*dir);
            }
        }
        combined
    }
}

pub const SHARED_SKILL_DIRS: &[SharedSkillDir] = &[
    SharedSkillDir {
        relative_path: ".agents/skills/kairastra-commit",
        display_name: "kairastra-commit",
    },
    SharedSkillDir {
        relative_path: ".agents/skills/kairastra-debug",
        display_name: "kairastra-debug",
    },
    SharedSkillDir {
        relative_path: ".agents/skills/kairastra-github",
        display_name: "kairastra-github",
    },
    SharedSkillDir {
        relative_path: ".agents/skills/kairastra-land",
        display_name: "kairastra-land",
    },
    SharedSkillDir {
        relative_path: ".agents/skills/kairastra-pull",
        display_name: "kairastra-pull",
    },
    SharedSkillDir {
        relative_path: ".agents/skills/kairastra-push",
        display_name: "kairastra-push",
    },
];

pub const SHARED_SKILL_ASSETS: &[SharedSkillAsset] = &[
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-commit",
        relative_path: ".agents/skills/kairastra-commit/SKILL.md",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-commit/SKILL.md"
        )),
    },
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-debug",
        relative_path: ".agents/skills/kairastra-debug/SKILL.md",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-debug/SKILL.md"
        )),
    },
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-github",
        relative_path: ".agents/skills/kairastra-github/SKILL.md",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-github/SKILL.md"
        )),
    },
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-land",
        relative_path: ".agents/skills/kairastra-land/SKILL.md",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-land/SKILL.md"
        )),
    },
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-land",
        relative_path: ".agents/skills/kairastra-land/land_watch.py",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-land/land_watch.py"
        )),
    },
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-pull",
        relative_path: ".agents/skills/kairastra-pull/SKILL.md",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-pull/SKILL.md"
        )),
    },
    SharedSkillAsset {
        skill_dir: ".agents/skills/kairastra-push",
        relative_path: ".agents/skills/kairastra-push/SKILL.md",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.agents/skills/kairastra-push/SKILL.md"
        )),
    },
];

pub fn inspect_shared_skill_plan(repo_root: &Path) -> Result<SharedSkillPlan> {
    let mut missing_dirs = Vec::new();
    let mut outdated_dirs = Vec::new();

    for dir in SHARED_SKILL_DIRS {
        let repo_dir = repo_root.join(dir.relative_path);
        if !repo_dir.exists() {
            missing_dirs.push(dir);
            continue;
        }

        let mut dir_outdated = false;
        for asset in SHARED_SKILL_ASSETS
            .iter()
            .filter(|asset| asset.skill_dir == dir.relative_path)
        {
            let path = repo_root.join(asset.relative_path);
            match fs::read_to_string(&path) {
                Ok(existing) => {
                    if existing != asset.contents {
                        dir_outdated = true;
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    dir_outdated = true;
                    break;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to read {}", path.display()));
                }
            }
        }

        if dir_outdated {
            outdated_dirs.push(dir);
        }
    }

    Ok(SharedSkillPlan {
        missing_dirs,
        outdated_dirs,
    })
}

pub fn install_shared_skills(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for asset in SHARED_SKILL_ASSETS {
        let target = repo_root.join(asset.relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&target, asset.contents)
            .with_context(|| format!("failed to write {}", target.display()))?;
        written.push(target);
    }
    Ok(written)
}

pub fn missing_skill_entrypoints(repo_root: &Path) -> Vec<PathBuf> {
    SHARED_SKILL_ASSETS
        .iter()
        .filter(|asset| !repo_root.join(asset.relative_path).exists())
        .map(|asset| repo_root.join(asset.relative_path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        inspect_shared_skill_plan, install_shared_skills, missing_skill_entrypoints,
        SHARED_SKILL_DIRS,
    };
    use tempfile::tempdir;

    #[test]
    fn detects_missing_shared_skills() {
        let dir = tempdir().unwrap();
        let plan = inspect_shared_skill_plan(dir.path()).unwrap();
        assert_eq!(plan.missing_dirs.len(), SHARED_SKILL_DIRS.len());
        assert!(plan.outdated_dirs.is_empty());
    }

    #[test]
    fn installs_shared_skills_and_clears_plan() {
        let dir = tempdir().unwrap();
        install_shared_skills(dir.path()).unwrap();
        let plan = inspect_shared_skill_plan(dir.path()).unwrap();
        assert!(plan.is_empty());
        assert!(missing_skill_entrypoints(dir.path()).is_empty());
    }

    #[test]
    fn detects_outdated_skill_content() {
        let dir = tempdir().unwrap();
        install_shared_skills(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(".agents/skills/kairastra-push/SKILL.md"),
            "outdated\n",
        )
        .unwrap();
        let plan = inspect_shared_skill_plan(dir.path()).unwrap();
        assert!(plan
            .outdated_dirs
            .iter()
            .any(|dir| dir.display_name == "kairastra-push"));
    }
}
