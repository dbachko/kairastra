use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value as JsonValue};

const GEMINI_DIR_NAME: &str = ".gemini";
const GEMINI_SETTINGS_FILE: &str = "settings.json";
const MCP_SERVER_NAME: &str = "kairastra_github";

pub fn ensure_github_mcp_server() -> Result<()> {
    let settings_path = settings_file_path()?;
    let parent = settings_path
        .parent()
        .ok_or_else(|| anyhow!("invalid_gemini_settings_path"))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    let mut root = if settings_path.exists() {
        let contents = fs::read_to_string(&settings_path)
            .with_context(|| format!("failed to read {}", settings_path.display()))?;
        serde_json::from_str::<JsonValue>(&contents)
            .with_context(|| format!("invalid JSON in {}", settings_path.display()))?
    } else {
        json!({})
    };

    let mcp_servers = ensure_object_field(&mut root, "mcpServers")?;
    let current_exe = std::env::current_exe().context("failed to resolve kairastra binary path")?;
    mcp_servers.insert(
        MCP_SERVER_NAME.to_string(),
        json!({
            "command": current_exe.display().to_string(),
            "args": ["github-mcp"],
            "env": {
                "GITHUB_TOKEN": "$GITHUB_TOKEN",
                "GH_TOKEN": "$GH_TOKEN",
                "KAIRASTRA_GITHUB_OWNER": "$KAIRASTRA_GITHUB_OWNER",
                "KAIRASTRA_GITHUB_REPO": "$KAIRASTRA_GITHUB_REPO",
                "KAIRASTRA_GITHUB_PROJECT_NUMBER": "$KAIRASTRA_GITHUB_PROJECT_NUMBER"
            },
            "trust": true,
            "timeout": 30000,
            "includeTools": ["github_graphql", "github_rest"]
        }),
    );

    let rendered =
        serde_json::to_string_pretty(&root).context("failed to render Gemini settings JSON")?;
    fs::write(&settings_path, format!("{rendered}\n"))
        .with_context(|| format!("failed to write {}", settings_path.display()))?;

    Ok(())
}

fn settings_file_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("home_directory_unavailable"))?;
    Ok(home.join(GEMINI_DIR_NAME).join(GEMINI_SETTINGS_FILE))
}

fn ensure_object_field<'a>(
    root: &'a mut JsonValue,
    field: &str,
) -> Result<&'a mut Map<String, JsonValue>> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("Gemini settings root must be a JSON object"))?;
    let entry = object.entry(field.to_string()).or_insert_with(|| json!({}));
    entry
        .as_object_mut()
        .ok_or_else(|| anyhow!("Gemini settings field `{field}` must be a JSON object"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Mutex;

    use serde_json::{json, Value as JsonValue};

    use super::ensure_github_mcp_server;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: OsString) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn merges_mcp_server_without_clobbering_existing_auth_settings() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home_dir = dir.path().join("home");
        let gemini_dir = home_dir.join(".gemini");
        std::fs::create_dir_all(&gemini_dir).unwrap();
        std::fs::write(
            gemini_dir.join("settings.json"),
            serde_json::to_string_pretty(&json!({
                "security": {
                    "auth": {
                        "selectedType": "oauth-personal"
                    }
                },
                "general": {
                    "enableAutoUpdate": false
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let _home = EnvVarGuard::set("HOME", home_dir.as_os_str().into());
        ensure_github_mcp_server().unwrap();

        let saved: JsonValue = serde_json::from_str(
            &std::fs::read_to_string(gemini_dir.join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["security"]["auth"]["selectedType"], "oauth-personal");
        assert_eq!(saved["general"]["enableAutoUpdate"], false);
        assert_eq!(
            saved["mcpServers"]["kairastra_github"]["includeTools"],
            json!(["github_graphql", "github_rest"])
        );
        assert_eq!(saved["mcpServers"]["kairastra_github"]["trust"], true);
    }
}
