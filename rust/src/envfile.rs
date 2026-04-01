use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

pub fn load_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read env file {}", path.display()))?;
    parse_env_file(&content)
}

pub fn parse_env_file(content: &str) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();

    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(anyhow!(
                "invalid_env_line:{}: expected KEY=VALUE",
                index + 1
            ));
        };

        let key = raw_key.trim();
        if key.is_empty() {
            return Err(anyhow!("invalid_env_line:{}: missing key", index + 1));
        }

        let value = parse_value(raw_value.trim())?;
        values.insert(key.to_string(), value);
    }

    Ok(values)
}

pub fn apply_env(values: &HashMap<String, String>) {
    for (key, value) in values {
        std::env::set_var(key, value);
    }
}

pub fn apply_env_if_missing(values: &HashMap<String, String>) {
    for (key, value) in values {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

fn parse_value(raw: &str) -> Result<String> {
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        return Ok(raw[1..raw.len() - 1]
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\"));
    }

    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return Ok(raw[1..raw.len() - 1].to_string());
    }

    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::parse_env_file;

    #[test]
    fn parses_basic_env_lines() {
        let parsed = parse_env_file("FOO=bar\nBAR=\"baz qux\"\n# noop\n").unwrap();
        assert_eq!(parsed.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(parsed.get("BAR").map(String::as_str), Some("baz qux"));
    }

    #[test]
    fn rejects_lines_without_assignment() {
        let error = parse_env_file("NOPE\n").unwrap_err().to_string();
        assert!(error.contains("invalid_env_line:1"));
    }
}
