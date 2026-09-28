// SPDX-License-Identifier: GPL-3.0-only

//! `ConfigImporter` reading `~/.codex/config.toml` (read-only, D-017). Phase 0 scope only:
//! `[mcp_servers.<id>]` → `ConfigFragment { sections: {"mcp": ...} }` (docs/import.md §1). Never
//! writes back to `config.toml`, never spawns `codex` (INV-1).
//!
//! Secret handling (turning a literal token in `env`/a bearer header into a `${VAR}` reference or
//! xlightcli's own MCP credential store) is `config::merge`'s job in Phase 3 (docs/import.md §2);
//! this importer only produces the canonical fragment, unmodified.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use xlightcli_protocol::{ConfigFragment, ProviderError};
use xlightcli_provider::ConfigImporter;

#[derive(Debug)]
pub struct CodexImporter {
    codex_home: PathBuf,
}

impl Default for CodexImporter {
    fn default() -> Self {
        Self {
            codex_home: crate::default_codex_home(),
        }
    }
}

impl CodexImporter {
    #[cfg(test)]
    fn with_codex_home(home: PathBuf) -> Self {
        Self { codex_home: home }
    }

    fn config_path(&self) -> PathBuf {
        self.codex_home.join("config.toml")
    }
}

#[derive(Debug, Default, Deserialize)]
struct CodexConfigFile {
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServerEntry>,
}

/// Fields per docs/import.md §1 (`command,args,env,cwd,url`, bearer token env var,
/// `default_tools_approval_mode`).
#[derive(Debug, Default, Deserialize, Serialize)]
struct McpServerEntry {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    default_tools_approval_mode: Option<String>,
}

#[async_trait]
impl ConfigImporter for CodexImporter {
    fn source_name(&self) -> &'static str {
        "Codex CLI"
    }

    async fn import(&self) -> Result<ConfigFragment, ProviderError> {
        let path = self.config_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(ConfigFragment {
                sections: Default::default(),
                summary: format!("no Codex config found at {}", path.display()),
            });
        };
        let parsed: CodexConfigFile = toml::from_str(&text).map_err(|e| {
            ProviderError::InvalidRequest(format!("invalid {}: {e}", path.display()))
        })?;
        if parsed.mcp_servers.is_empty() {
            return Ok(ConfigFragment {
                sections: Default::default(),
                summary: format!("{} has no [mcp_servers.*] entries", path.display()),
            });
        }
        let mcp_value = serde_json::to_value(&parsed.mcp_servers)
            .map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
        let mut sections = serde_json::Map::new();
        sections.insert("mcp".to_string(), mcp_value);
        let summary = format!(
            "imported {} MCP server(s) from {}",
            parsed.mcp_servers.len(),
            path.display()
        );
        Ok(ConfigFragment { sections, summary })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn missing_config_is_reported_not_errored() {
        let dir = tempfile::tempdir().unwrap();
        let importer = CodexImporter::with_codex_home(dir.path().to_path_buf());
        let fragment = importer.import().await.unwrap();
        assert!(fragment.sections.is_empty());
        assert!(fragment.summary.contains("no Codex config found"));
    }

    #[tokio::test]
    async fn config_without_mcp_servers_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "model = \"gpt-5-codex\"\n").unwrap();
        let importer = CodexImporter::with_codex_home(dir.path().to_path_buf());
        let fragment = importer.import().await.unwrap();
        assert!(fragment.sections.is_empty());
        assert!(fragment.summary.contains("no [mcp_servers.*]"));
    }

    #[tokio::test]
    async fn mcp_servers_are_imported_into_the_mcp_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }
default_tools_approval_mode = "always_allow"
"#,
        )
        .unwrap();
        let importer = CodexImporter::with_codex_home(dir.path().to_path_buf());
        let fragment = importer.import().await.unwrap();
        assert!(fragment.summary.contains("imported 1 MCP server"));
        let mcp = fragment.sections.get("mcp").unwrap();
        let github = mcp.get("github").unwrap();
        assert_eq!(github.get("command").unwrap(), "npx");
        assert_eq!(
            github.get("default_tools_approval_mode").unwrap(),
            "always_allow"
        );
    }

    #[tokio::test]
    async fn malformed_toml_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "not = [valid toml").unwrap();
        let importer = CodexImporter::with_codex_home(dir.path().to_path_buf());
        assert!(importer.import().await.is_err());
    }
}
