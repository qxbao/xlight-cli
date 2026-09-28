// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for config loading via `app::wiring` (`ConfigLoader` + `TrustStore`).
//! Ensures project/global config layers are resolved into `RuntimeDeps.config`, and that
//! untrusted project configurations cannot escalate permissions or enable dangerous settings.

use std::fs;
use tempfile::tempdir;
use xlightcli_config::{PermissionMode, TrustStore};
use xlightcli_protocol::{ModelId, ProviderId, TransportId};

#[tokio::test]
async fn untrusted_project_cannot_escalate_permission_mode() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    let project_dir = repo_root.join(".xlightcli");
    fs::create_dir_all(&project_dir).expect("create project dir");

    // Project config requests full-auto mode and safe tool timeout
    fs::write(
        project_dir.join("config.toml"),
        r#"
[permissions]
mode = "full-auto"

[tools]
shell_timeout_secs = 45
"#,
    )
    .expect("write config.toml");

    let db_path = temp.path().join("data").join("test.db");
    let global_config = temp.path().join("global.toml");
    let trust_path = temp.path().join("trust.toml");

    let runtime = xlightcli::wiring::build_runtime_with_paths(
        db_path,
        global_config,
        trust_path,
        Some(&repo_root),
    )
    .await
    .expect("runtime build should succeed");

    let cfg = &runtime.handle.deps().config;
    // Sensitive mode escalation must be filtered out for untrusted project -> falls back to Ask
    assert_eq!(cfg.permissions.mode, PermissionMode::Ask);
    // Safe project setting must be preserved
    assert_eq!(cfg.tools.shell_timeout_secs, 45);
}

#[tokio::test]
async fn untrusted_project_cannot_allow_command_execution() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    let project_dir = repo_root.join(".xlightcli");
    fs::create_dir_all(&project_dir).expect("create project dir");

    fs::write(
        project_dir.join("config.toml"),
        r#"
[permissions]
allow = ["command(*)", "unsandboxed"]
"#,
    )
    .expect("write config.toml");

    let db_path = temp.path().join("data").join("test.db");
    let global_config = temp.path().join("global.toml");
    let trust_path = temp.path().join("trust.toml");

    let runtime = xlightcli::wiring::build_runtime_with_paths(
        db_path,
        global_config,
        trust_path,
        Some(&repo_root),
    )
    .await
    .expect("runtime build should succeed");

    let cfg = &runtime.handle.deps().config;
    // Command and unsandboxed allow rules must be stripped from untrusted project
    assert!(cfg.permissions.allow.is_empty());
}

#[tokio::test]
async fn trusted_project_loads_permission_overrides() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    let project_dir = repo_root.join(".xlightcli");
    fs::create_dir_all(&project_dir).expect("create project dir");

    fs::write(
        project_dir.join("config.toml"),
        r#"
[permissions]
mode = "auto-edit"
allow = ["read_file(*)"]
"#,
    )
    .expect("write config.toml");

    let db_path = temp.path().join("data").join("test.db");
    let global_config = temp.path().join("global.toml");
    let trust_path = temp.path().join("trust.toml");

    // Explicitly trust the repo root
    let trust_store = TrustStore::load(&trust_path).expect("load trust store");
    trust_store.trust(&repo_root).expect("trust repo root");

    let runtime = xlightcli::wiring::build_runtime_with_paths(
        db_path,
        global_config,
        trust_path,
        Some(&repo_root),
    )
    .await
    .expect("runtime build should succeed");

    let cfg = &runtime.handle.deps().config;
    // Trusted project permissions must be honored
    assert_eq!(cfg.permissions.mode, PermissionMode::AutoEdit);
    assert_eq!(cfg.permissions.allow, vec!["read_file(*)".to_string()]);
}

#[tokio::test]
async fn project_cannot_enable_experimental_transports_even_if_trusted() {
    let temp = tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    let project_dir = repo_root.join(".xlightcli");
    fs::create_dir_all(&project_dir).expect("create project dir");

    fs::write(
        project_dir.join("config.toml"),
        r#"
[experimental]
claude_subscription = true
antigravity_subscription = true
"#,
    )
    .expect("write config.toml");

    let db_path = temp.path().join("data").join("test.db");
    let global_config = temp.path().join("global.toml");
    let trust_path = temp.path().join("trust.toml");

    let trust_store = TrustStore::load(&trust_path).expect("load trust store");
    trust_store.trust(&repo_root).expect("trust repo root");

    let runtime = xlightcli::wiring::build_runtime_with_paths(
        db_path,
        global_config,
        trust_path,
        Some(&repo_root),
    )
    .await
    .expect("runtime build should succeed");

    let cfg = &runtime.handle.deps().config;
    // Experimental flags must NEVER be enabled from project level (D-002)
    assert!(!cfg.experimental.claude_subscription);
    assert!(!cfg.experimental.antigravity_subscription);
}

#[tokio::test]
async fn global_config_loads_provider_defaults() {
    let temp = tempdir().expect("tempdir");
    let global_config = temp.path().join("global.toml");
    fs::write(
        &global_config,
        r#"
default_provider = "codex"

[provider.codex]
transport = "chatgpt"
default_model = "gpt-5-codex"
"#,
    )
    .expect("write global.toml");

    let db_path = temp.path().join("data").join("test.db");
    let trust_path = temp.path().join("trust.toml");

    let runtime =
        xlightcli::wiring::build_runtime_with_paths(db_path, global_config, trust_path, None)
            .await
            .expect("runtime build should succeed");

    let cfg = &runtime.handle.deps().config;
    assert_eq!(cfg.default_provider, Some(ProviderId::new("codex")));
    let codex_defaults = cfg.provider.get("codex").expect("codex defaults present");
    assert_eq!(codex_defaults.transport, Some(TransportId::new("chatgpt")));
    assert_eq!(
        codex_defaults.default_model,
        Some(ModelId::new("gpt-5-codex"))
    );
}

#[tokio::test]
async fn build_runtime_at_provides_test_isolation() {
    let temp = tempdir().expect("tempdir");
    let db_path = temp.path().join("isolated.db");

    let runtime = xlightcli::wiring::build_runtime_at(db_path)
        .await
        .expect("build_runtime_at should succeed with isolated db");

    let cfg = &runtime.handle.deps().config;
    // In an isolated temp path, default provider is None unless configured
    assert_eq!(cfg.permissions.mode, PermissionMode::Ask);
}
