// SPDX-License-Identifier: GPL-3.0-only

//! `ConfigImporter` reading `~/.claude` (read-only, D-017) (Phase 3 territory per CODEBASE.md
//! §7, implemented early here at the team lead's request — see final report).
//!
//! Reads project `.mcp.json` and the user-level `mcpServers` key of `~/.claude.json`
//! (docs/import.md). Read-only (D-017): never writes back, never spawns the Claude Code CLI
//! (INV-1). Does **not** yet read the nested `projects["<abs path>"].mcpServers` section of
//! `~/.claude.json` (docs/import.md calls that variant "M" confidence) — left for Phase 3 richer
//! import, noted as a deviation in the final report.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Map, Value};
use xlightcli_protocol::{ConfigFragment, ProviderError};
use xlightcli_provider::ConfigImporter;

/// Reads Claude Code's MCP config. Both paths are injectable so tests never touch a real
/// `$HOME` (AGENTS.md §7).
#[derive(Debug)]
pub(crate) struct ClaudeImporter {
    home: PathBuf,
    project_root: Option<PathBuf>,
}

impl ClaudeImporter {
    pub(crate) fn new(home: PathBuf, project_root: Option<PathBuf>) -> Self {
        Self { home, project_root }
    }
}

async fn read_mcp_servers(path: &std::path::Path) -> Option<Map<String, Value>> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "claude import: failed to parse JSON");
            return None;
        }
    };
    parsed.get("mcpServers").and_then(Value::as_object).cloned()
}

#[async_trait]
impl ConfigImporter for ClaudeImporter {
    fn source_name(&self) -> &'static str {
        "Claude Code"
    }

    async fn import(&self) -> Result<ConfigFragment, ProviderError> {
        let mut merged = Map::new();
        let mut summary_parts = Vec::new();

        if let Some(root) = &self.project_root {
            let path = root.join(".mcp.json");
            if let Some(servers) = read_mcp_servers(&path).await {
                summary_parts.push(format!(
                    "{} MCP server(s) from {}",
                    servers.len(),
                    path.display()
                ));
                for (name, def) in servers {
                    merged.insert(name, def);
                }
            }
        }

        let user_path = self.home.join(".claude.json");
        if let Some(servers) = read_mcp_servers(&user_path).await {
            summary_parts.push(format!(
                "{} MCP server(s) from {}",
                servers.len(),
                user_path.display()
            ));
            for (name, def) in servers {
                // Project-level config wins over user-level for the same server name.
                merged.entry(name).or_insert(def);
            }
        }

        let mut sections = Map::new();
        if !merged.is_empty() {
            sections.insert("mcp".to_string(), Value::Object(merged));
        }
        let summary = if summary_parts.is_empty() {
            "no Claude Code MCP config found".to_string()
        } else {
            summary_parts.join("; ")
        };
        Ok(ConfigFragment { sections, summary })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn import_merges_project_and_user_mcp_servers() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        tokio::fs::write(
            project.path().join(".mcp.json"),
            r#"{"mcpServers": {"github": {"command": "github-mcp-server"}}}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers": {"docs": {"url": "https://example.com/mcp"}}}"#,
        )
        .await
        .unwrap();

        let importer = ClaudeImporter::new(
            home.path().to_path_buf(),
            Some(project.path().to_path_buf()),
        );
        let fragment = importer.import().await.unwrap();
        let mcp = fragment.sections.get("mcp").unwrap().as_object().unwrap();
        assert!(mcp.contains_key("github"));
        assert!(mcp.contains_key("docs"));
        assert!(fragment.summary.contains("MCP server"));
    }

    #[tokio::test]
    async fn import_with_nothing_present_reports_an_empty_but_honest_summary() {
        let home = tempfile::tempdir().unwrap();
        let importer = ClaudeImporter::new(home.path().to_path_buf(), None);
        let fragment = importer.import().await.unwrap();
        assert!(fragment.sections.is_empty());
        assert_eq!(fragment.summary, "no Claude Code MCP config found");
    }

    #[tokio::test]
    async fn project_level_server_wins_over_user_level_on_name_collision() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        tokio::fs::write(
            project.path().join(".mcp.json"),
            r#"{"mcpServers": {"github": {"command": "project-version"}}}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers": {"github": {"command": "user-version"}}}"#,
        )
        .await
        .unwrap();
        let importer = ClaudeImporter::new(
            home.path().to_path_buf(),
            Some(project.path().to_path_buf()),
        );
        let fragment = importer.import().await.unwrap();
        let mcp = fragment.sections.get("mcp").unwrap().as_object().unwrap();
        assert_eq!(mcp["github"]["command"], Value::from("project-version"));
    }
}
