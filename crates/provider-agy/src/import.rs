// SPDX-License-Identifier: GPL-3.0-only

//! `ConfigImporter` reading `~/.gemini/config/mcp_config.json` and `.agents/mcp_config.json`
//! (read-only, D-017). See `docs/import.md` §1 (agy MCP row).
//!
//! Both files share the shape
//! `{"mcpServers": {name: {command,args,env,cwd | serverUrl,httpUrl,url,headers,…}}}`; this
//! importer only normalizes the URL-transport spelling (`serverUrl`/`httpUrl` → `url`) per
//! docs/import.md §2 and merges the two files (project entries override/add to user entries) —
//! it does not yet interpret `oauth`/`authProviderType`/`disabledTools` (Phase 3 work, once
//! `config::merge` understands the `ConfigFragment` shape).

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use xlightcli_protocol::{ConfigFragment, ProviderError};
use xlightcli_provider::ConfigImporter;

#[derive(Debug)]
pub(crate) struct AgyMcpImporter {
    /// `~/.gemini/config/mcp_config.json`.
    user_config: PathBuf,
    /// `<cwd>/.agents/mcp_config.json`.
    project_config: PathBuf,
}

impl AgyMcpImporter {
    pub(crate) fn new() -> Self {
        let home = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_paths(
            home.join(".gemini/config/mcp_config.json"),
            cwd.join(".agents/mcp_config.json"),
        )
    }

    /// Injectable-paths constructor for tests (tempdir fixtures) — never reads the user's real
    /// `~/.gemini` (constraint: this crate must not touch real user config in tests).
    pub(crate) fn with_paths(user_config: PathBuf, project_config: PathBuf) -> Self {
        Self {
            user_config,
            project_config,
        }
    }
}

/// Renames `serverUrl`/`httpUrl` to `url` in place (docs/import.md §1 agy MCP row: "serverUrl,
/// httpUrl, url" all appear across real configs). Leaves stdio-shaped servers
/// (`command`/`args`/`env`/`cwd`) untouched, and leaves an already-canonical `url` alone.
fn normalize_mcp_server(cfg: &mut Value) {
    let Value::Object(map) = cfg else { return };
    if map.contains_key("url") {
        return;
    }
    for key in ["serverUrl", "httpUrl"] {
        if let Some(value) = map.remove(key) {
            map.insert("url".into(), value);
            break;
        }
    }
}

/// Merges one file's `mcpServers` object into `target`, normalizing each entry. Later calls (the
/// project file) override same-named entries from earlier calls (the user file), matching
/// docs/import.md §2's project-overrides-user rule. Returns how many entries this file
/// contributed.
fn merge_mcp_servers(target: &mut Map<String, Value>, doc: &Value) -> usize {
    let Some(servers) = doc.get("mcpServers").and_then(Value::as_object) else {
        return 0;
    };
    let mut count = 0;
    for (name, cfg) in servers {
        let mut cfg = cfg.clone();
        normalize_mcp_server(&mut cfg);
        target.insert(name.clone(), cfg);
        count += 1;
    }
    count
}

#[async_trait]
impl ConfigImporter for AgyMcpImporter {
    fn source_name(&self) -> &'static str {
        "agy (~/.gemini/config/mcp_config.json + .agents/mcp_config.json)"
    }

    async fn import(&self) -> Result<ConfigFragment, ProviderError> {
        let mut servers = Map::new();
        let mut summary = Vec::new();
        for (label, path) in [
            ("user (~/.gemini/config/mcp_config.json)", &self.user_config),
            ("project (.agents/mcp_config.json)", &self.project_config),
        ] {
            match tokio::fs::read(path).await {
                Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                    Ok(doc) => {
                        let count = merge_mcp_servers(&mut servers, &doc);
                        summary.push(format!("{label}: {count} server(s) imported"));
                    }
                    Err(e) => summary.push(format!("{label}: skipped (invalid JSON: {e})")),
                },
                Err(_) => summary.push(format!("{label}: not found")),
            }
        }

        let mut sections = Map::new();
        sections.insert("mcp".into(), json!({ "servers": servers }));
        Ok(ConfigFragment {
            sections,
            summary: summary.join("; "),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn imports_and_normalizes_server_url_and_merges_project_over_user() {
        let dir = tempfile::tempdir().unwrap();
        let user_path = dir.path().join("user_mcp_config.json");
        let project_path = dir.path().join("project_mcp_config.json");
        tokio::fs::write(
            &user_path,
            r#"{"mcpServers":{"github":{"serverUrl":"https://example.invalid/mcp"},"local":{"command":"foo"}}}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            &project_path,
            r#"{"mcpServers":{"github":{"httpUrl":"https://project.invalid/mcp"}}}"#,
        )
        .await
        .unwrap();

        let importer = AgyMcpImporter::with_paths(user_path, project_path);
        let fragment = importer.import().await.unwrap();

        let servers = fragment.sections["mcp"]["servers"].as_object().unwrap();
        assert_eq!(
            servers["github"]["url"].as_str().unwrap(),
            "https://project.invalid/mcp",
            "project entry must override the user entry for the same server name"
        );
        assert_eq!(servers["local"]["command"].as_str().unwrap(), "foo");
        assert!(fragment.summary.contains("2 server(s) imported"));
    }

    #[tokio::test]
    async fn missing_files_are_reported_not_silently_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let importer = AgyMcpImporter::with_paths(
            dir.path().join("nope-user.json"),
            dir.path().join("nope-project.json"),
        );
        let fragment = importer.import().await.unwrap();
        assert!(fragment.summary.contains("not found"));
        assert!(
            fragment.sections["mcp"]["servers"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn normalize_prefers_existing_url_over_server_url() {
        let mut cfg = json!({"url": "https://a.invalid", "serverUrl": "https://b.invalid"});
        normalize_mcp_server(&mut cfg);
        assert_eq!(cfg["url"].as_str().unwrap(), "https://a.invalid");
    }

    #[test]
    fn normalize_leaves_stdio_servers_untouched() {
        let mut cfg = json!({"command": "npx", "args": ["-y", "foo"]});
        normalize_mcp_server(&mut cfg);
        assert_eq!(cfg["command"].as_str().unwrap(), "npx");
        assert!(cfg.get("url").is_none());
    }
}
