// SPDX-License-Identifier: GPL-3.0-only

//! Secret persistence (OS keyring / file fallback, D-019).
//!
//! This is the only module allowed to turn a `CredentialSet`'s secret into on-disk/keyring bytes.
//! Keeping the (de)serialization here — rather than deriving `Serialize` on `CredentialSet`
//! itself — means nothing outside this module can accidentally serialize a live secret into a
//! log, snapshot, or debug dump (PATTERNS.md §4).
//!
//! Two backends:
//! - [`KeyringStore`]: OS keyring via the `keyring` crate (Secret Service on Linux via
//!   `zbus-secret-service-keyring-store`, Keychain on macOS), service `"xlightcli"`, one entry per
//!   `(provider, transport, account_id)`.
//! - [`FileStore`]: D-019 fallback for Linux systems without a working Secret Service — one file
//!   per account under a directory, dir `0700` / file `0600`, atomic replace via temp-file +
//!   rename, and refuses to *read* a file whose permissions have been loosened.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use xlightcli_protocol::{ProviderId, TransportId};

use crate::credential::{AccountInfo, CredentialSecret, CredentialSet};
use crate::error::AuthError;

/// Persists/loads credentials for one `(provider, transport, account_id)`.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn save(&self, credential: &CredentialSet) -> Result<(), AuthError>;

    async fn load(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<Option<CredentialSet>, AuthError>;

    async fn delete(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<(), AuthError>;
}

/// Safe default until a real backend is wired in: reports "unavailable" instead of silently
/// no-op-ing persistence (which would look like a successful login that vanishes on restart).
#[derive(Debug, Default)]
pub struct UnimplementedStore;

#[async_trait]
impl SecretStore for UnimplementedStore {
    async fn save(&self, _credential: &CredentialSet) -> Result<(), AuthError> {
        Err(AuthError::StoreUnavailable(
            "secret store not configured (pick StoreKind::Keyring or StoreKind::File)".into(),
        ))
    }

    async fn load(
        &self,
        _provider: &ProviderId,
        _transport: &TransportId,
        _account_id: &str,
    ) -> Result<Option<CredentialSet>, AuthError> {
        Err(AuthError::StoreUnavailable(
            "secret store not configured (pick StoreKind::Keyring or StoreKind::File)".into(),
        ))
    }

    async fn delete(
        &self,
        _provider: &ProviderId,
        _transport: &TransportId,
        _account_id: &str,
    ) -> Result<(), AuthError> {
        Err(AuthError::StoreUnavailable(
            "secret store not configured (pick StoreKind::Keyring or StoreKind::File)".into(),
        ))
    }
}

// --- shared (de)serialization -------------------------------------------------------------

/// On-disk/keyring shape of a `CredentialSecret`. Private: never leaves this module. Field names
/// are part of the persisted format — changing them is a migration, not a refactor.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredSecret {
    Bearer {
        access_token: String,
        refresh_token: Option<String>,
        /// RFC 3339, if the credential carries an expiry.
        expires_at: Option<String>,
    },
    Header {
        header_name: String,
        value: String,
    },
}

/// On-disk/keyring shape of a full `CredentialSet`. `account` is non-secret (`AccountInfo`
/// derives `Serialize`/`Deserialize` directly); only the secret half needs the `StoredSecret`
/// indirection.
#[derive(Debug, Serialize, Deserialize)]
struct StoredCredential {
    account: AccountInfo,
    secret: StoredSecret,
}

fn to_stored(credential: &CredentialSet) -> Result<StoredCredential, AuthError> {
    let secret = match &credential.secret {
        CredentialSecret::Bearer {
            access_token,
            refresh_token,
            expires_at,
        } => StoredSecret::Bearer {
            access_token: access_token.expose_secret().to_owned(),
            refresh_token: refresh_token.as_ref().map(|t| t.expose_secret().to_owned()),
            expires_at: expires_at
                .map(|t| t.format(&Rfc3339))
                .transpose()
                .map_err(|e| AuthError::StoreUnavailable(format!("format expires_at: {e}")))?,
        },
        CredentialSecret::Header { header_name, value } => StoredSecret::Header {
            header_name: header_name.clone(),
            value: value.expose_secret().to_owned(),
        },
    };
    Ok(StoredCredential {
        account: credential.account.clone(),
        secret,
    })
}

fn from_stored(stored: StoredCredential) -> Result<CredentialSet, AuthError> {
    let secret = match stored.secret {
        StoredSecret::Bearer {
            access_token,
            refresh_token,
            expires_at,
        } => CredentialSecret::Bearer {
            access_token: SecretString::from(access_token),
            refresh_token: refresh_token.map(SecretString::from),
            expires_at: expires_at
                .map(|s| OffsetDateTime::parse(&s, &Rfc3339))
                .transpose()
                .map_err(|e| AuthError::StoreUnavailable(format!("parse expires_at: {e}")))?,
        },
        StoredSecret::Header { header_name, value } => CredentialSecret::Header {
            header_name,
            value: SecretString::from(value),
        },
    };
    Ok(CredentialSet {
        account: stored.account,
        secret,
    })
}

fn join_panic(e: tokio::task::JoinError) -> AuthError {
    AuthError::StoreUnavailable(format!("secret store worker task panicked: {e}"))
}

// --- KeyringStore --------------------------------------------------------------------------

/// OS-keyring-backed `SecretStore` (D-019 primary path). Uses the `keyring` crate's default `v1`
/// backend selection: Secret Service (via zbus) on Linux/BSD, Keychain Services on macOS.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// `service` is the keyring service name; entries are keyed by
    /// `"<provider>:<transport>:<account_id>"` within it. Defaults to `"xlightcli"`
    /// (CODEBASE.md §6) via [`KeyringStore::default`].
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new("xlightcli")
    }
}

fn keyring_account_key(provider: &ProviderId, transport: &TransportId, account_id: &str) -> String {
    format!("{provider}:{transport}:{account_id}")
}

/// Maps a `keyring` error to `AuthError`, giving an actionable hint for the class of failure
/// D-019 exists to route around (no Secret Service / no default store available at all).
fn map_keyring_error(err: keyring::Error) -> AuthError {
    match &err {
        keyring::Error::NoDefaultStore
        | keyring::Error::PlatformFailure(_)
        | keyring::Error::NoStorageAccess(_) => AuthError::StoreUnavailable(format!(
            "OS keyring unavailable ({err}); set `auth.store = \"file\"` to use the file-based \
             fallback instead (D-019)"
        )),
        other => AuthError::StoreUnavailable(format!("keyring error: {other}")),
    }
}

#[async_trait]
impl SecretStore for KeyringStore {
    async fn save(&self, credential: &CredentialSet) -> Result<(), AuthError> {
        let service = self.service.clone();
        let key = keyring_account_key(
            &credential.account.provider,
            &credential.account.transport,
            &credential.account.account_id,
        );
        let payload = serde_json::to_string(&to_stored(credential)?)
            .map_err(|e| AuthError::StoreUnavailable(format!("serialize credential: {e}")))?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key).map_err(map_keyring_error)?;
            entry.set_password(&payload).map_err(map_keyring_error)
        })
        .await
        .map_err(join_panic)?
    }

    async fn load(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<Option<CredentialSet>, AuthError> {
        let service = self.service.clone();
        let key = keyring_account_key(provider, transport, account_id);
        let payload = tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key).map_err(map_keyring_error)?;
            match entry.get_password() {
                Ok(payload) => Ok(Some(payload)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(map_keyring_error(e)),
            }
        })
        .await
        .map_err(join_panic)??;
        match payload {
            None => Ok(None),
            Some(payload) => {
                let stored: StoredCredential = serde_json::from_str(&payload).map_err(|e| {
                    AuthError::StoreUnavailable(format!("corrupt keyring payload: {e}"))
                })?;
                from_stored(stored).map(Some)
            }
        }
    }

    async fn delete(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<(), AuthError> {
        let service = self.service.clone();
        let key = keyring_account_key(provider, transport, account_id);
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key).map_err(map_keyring_error)?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(map_keyring_error(e)),
            }
        })
        .await
        .map_err(join_panic)?
    }
}

// --- FileStore -----------------------------------------------------------------------------

/// File-based `SecretStore` fallback (D-019): one JSON file per account under `dir`. Only used
/// when the user opts in (`auth.store = "file"`) because it's strictly weaker than the OS
/// keyring — always warn the caller of that when offering it.
#[derive(Debug, Clone)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// `dir` is created (`0700`) on first `save` if missing. Conventionally
    /// `$XDG_DATA_HOME/xlightcli/credentials` (CODEBASE.md §6); callers pass the concrete path so
    /// this crate doesn't have to depend on `xlightcli-config`'s path policy directly here.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

/// Deterministic, filesystem-safe file name for `(provider, transport, account_id)`. Hashing
/// (rather than sanitizing the account id) sidesteps every path-traversal / invalid-character
/// question at once — account ids are opaque upstream strings we don't otherwise validate.
fn credential_file_name(
    provider: &ProviderId,
    transport: &TransportId,
    account_id: &str,
) -> String {
    let key = format!("{provider}:{transport}:{account_id}");
    let digest = sha2::Sha256::digest(key.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // A `write!` into a `String` cannot fail; unwrap-free by construction.
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{hex}.json")
}

fn ensure_dir_0700(dir: &Path) -> Result<(), AuthError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AuthError::StoreUnavailable(format!("create {}: {e}", dir.display())))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| AuthError::StoreUnavailable(format!("chmod 0700 {}: {e}", dir.display())))?;
    Ok(())
}

/// Writes `contents` to `path` atomically: write to a sibling temp file with `mode` permissions,
/// `fsync`, then `rename` over the destination (rename is atomic on the same filesystem).
fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("credential file path has no parent directory"))?;
    let tmp_name = format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("credential"),
        uuid::Uuid::new_v4()
    );
    let tmp_path = dir.join(tmp_name);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    // Belt-and-suspenders: `mode()` above is subject to umask, so set the exact bits explicitly
    // before the file becomes visible at its final name.
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Refuses to read a credential file whose permissions grant group/other any access (D-019,
/// docs/PLAN.md §14): logs a `warn` and returns an actionable error instead of silently trusting
/// a file that e.g. got copied with looser permissions.
fn check_file_permissions(path: &Path) -> Result<(), AuthError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| AuthError::StoreUnavailable(format!("stat {}: {e}", path.display())))?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{mode:03o}"),
            "refusing to read credential file with group/other permissions"
        );
        return Err(AuthError::StoreUnavailable(format!(
            "credential file {} has unsafe permissions {mode:03o} (expected 600); fix with \
             `chmod 600 {}`",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

#[async_trait]
impl SecretStore for FileStore {
    async fn save(&self, credential: &CredentialSet) -> Result<(), AuthError> {
        let dir = self.dir.clone();
        let file_name = credential_file_name(
            &credential.account.provider,
            &credential.account.transport,
            &credential.account.account_id,
        );
        let payload = serde_json::to_vec(&to_stored(credential)?)
            .map_err(|e| AuthError::StoreUnavailable(format!("serialize credential: {e}")))?;
        tokio::task::spawn_blocking(move || {
            ensure_dir_0700(&dir)?;
            write_atomic(&dir.join(file_name), &payload, 0o600)
                .map_err(|e| AuthError::StoreUnavailable(format!("write credential file: {e}")))
        })
        .await
        .map_err(join_panic)?
    }

    async fn load(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<Option<CredentialSet>, AuthError> {
        let path = self
            .dir
            .join(credential_file_name(provider, transport, account_id));
        let bytes = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>, AuthError> {
            if !path.exists() {
                return Ok(None);
            }
            check_file_permissions(&path)?;
            std::fs::read(&path)
                .map(Some)
                .map_err(|e| AuthError::StoreUnavailable(format!("read credential file: {e}")))
        })
        .await
        .map_err(join_panic)??;
        match bytes {
            None => Ok(None),
            Some(bytes) => {
                let stored: StoredCredential = serde_json::from_slice(&bytes).map_err(|e| {
                    AuthError::StoreUnavailable(format!("corrupt credential file: {e}"))
                })?;
                from_stored(stored).map(Some)
            }
        }
    }

    async fn delete(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<(), AuthError> {
        let path = self
            .dir
            .join(credential_file_name(provider, transport, account_id));
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::StoreUnavailable(format!(
                "delete credential file: {e}"
            ))),
        })
        .await
        .map_err(join_panic)?
    }
}

// --- selection -------------------------------------------------------------------------------

/// Which `SecretStore` backend to use. Keyring is the default/preferred path (D-019); file is an
/// explicit opt-in fallback for systems without a working Secret Service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Keyring,
    File,
}

impl StoreKind {
    /// Builds the corresponding store. `file_dir` is only used for `StoreKind::File` (pass
    /// `xlightcli_config::paths::data_dir().join("credentials")` in `app::wiring`).
    pub fn build(self, file_dir: PathBuf) -> Arc<dyn SecretStore> {
        match self {
            StoreKind::Keyring => Arc::new(KeyringStore::default()),
            StoreKind::File => Arc::new(FileStore::new(file_dir)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::AuthKind;

    use super::*;

    fn sample_credential() -> CredentialSet {
        CredentialSet {
            account: AccountInfo {
                provider: ProviderId::new("codex"),
                transport: TransportId::new("chatgpt"),
                account_id: "acc-1".into(),
                label: Some("me@example.com".into()),
                auth_kind: AuthKind::Subscription,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: SecretString::from("XLC-SENTINEL-SECRET".to_string()),
                refresh_token: Some(SecretString::from("XLC-SENTINEL-REFRESH".to_string())),
                expires_at: Some(OffsetDateTime::now_utc()),
            },
        }
    }

    #[test]
    fn stored_roundtrip_preserves_bearer_secret() {
        let credential = sample_credential();
        let stored = to_stored(&credential).unwrap();
        let json = serde_json::to_string(&stored).unwrap();
        let parsed: StoredCredential = serde_json::from_str(&json).unwrap();
        let restored = from_stored(parsed).unwrap();

        match restored.secret {
            CredentialSecret::Bearer {
                access_token,
                refresh_token,
                ..
            } => {
                assert_eq!(access_token.expose_secret(), "XLC-SENTINEL-SECRET");
                assert_eq!(
                    refresh_token.unwrap().expose_secret(),
                    "XLC-SENTINEL-REFRESH"
                );
            }
            CredentialSecret::Header { .. } => panic!("expected Bearer"),
        }
        assert_eq!(restored.account.account_id, "acc-1");
    }

    #[test]
    fn stored_json_does_not_use_bearer_field_names_as_account_metadata() {
        // Sanity check that the persisted JSON actually contains the secret under `secret`, not
        // flattened into `account` (defense against a future accidental `#[serde(flatten)]`).
        let stored = to_stored(&sample_credential()).unwrap();
        let value = serde_json::to_value(&stored).unwrap();
        assert!(value.get("secret").is_some());
        assert!(value.get("account").is_some());
    }

    #[tokio::test]
    async fn file_store_roundtrips_and_permissions_are_0600() {
        let temp = tempfile::tempdir().unwrap();
        let store = FileStore::new(temp.path().join("credentials"));
        let credential = sample_credential();

        store.save(&credential).await.unwrap();

        let entries: Vec<_> = std::fs::read_dir(temp.path().join("credentials"))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let mode = entries[0].metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let dir_mode = std::fs::metadata(temp.path().join("credentials"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);

        let loaded = store
            .load(
                &credential.account.provider,
                &credential.account.transport,
                &credential.account.account_id,
            )
            .await
            .unwrap()
            .unwrap();
        match loaded.secret {
            CredentialSecret::Bearer { access_token, .. } => {
                assert_eq!(access_token.expose_secret(), "XLC-SENTINEL-SECRET");
            }
            CredentialSecret::Header { .. } => panic!("expected Bearer"),
        }
    }

    #[tokio::test]
    async fn file_store_load_missing_is_none() {
        let temp = tempfile::tempdir().unwrap();
        let store = FileStore::new(temp.path().to_path_buf());
        let found = store
            .load(
                &ProviderId::new("codex"),
                &TransportId::new("chatgpt"),
                "nope",
            )
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn file_store_refuses_to_read_world_readable_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = FileStore::new(temp.path().to_path_buf());
        let credential = sample_credential();
        store.save(&credential).await.unwrap();

        let file_name = credential_file_name(
            &credential.account.provider,
            &credential.account.transport,
            &credential.account.account_id,
        );
        let path = temp.path().join(file_name);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = store
            .load(
                &credential.account.provider,
                &credential.account.transport,
                &credential.account.account_id,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::StoreUnavailable(_)));
    }

    #[tokio::test]
    async fn file_store_delete_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let store = FileStore::new(temp.path().to_path_buf());
        let credential = sample_credential();
        store.save(&credential).await.unwrap();
        store
            .delete(
                &credential.account.provider,
                &credential.account.transport,
                &credential.account.account_id,
            )
            .await
            .unwrap();
        // Deleting again must not error.
        store
            .delete(
                &credential.account.provider,
                &credential.account.transport,
                &credential.account.account_id,
            )
            .await
            .unwrap();
        assert!(
            store
                .load(
                    &credential.account.provider,
                    &credential.account.transport,
                    &credential.account.account_id,
                )
                .await
                .unwrap()
                .is_none()
        );
    }

    // Real keyring tests need a running Secret Service (D-Bus session bus); not available in the
    // sandbox this crate is developed/tested in. Kept `#[ignore]` so they document intent and
    // still run for anyone with a real desktop session (`cargo test -- --ignored`).
    #[tokio::test]
    #[ignore = "needs a running OS keyring / Secret Service (sandbox)"]
    async fn keyring_store_roundtrip() {
        let store = KeyringStore::new("xlightcli-test");
        let credential = sample_credential();
        store.save(&credential).await.unwrap();
        let loaded = store
            .load(
                &credential.account.provider,
                &credential.account.transport,
                &credential.account.account_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.account.account_id, "acc-1");
        store
            .delete(
                &credential.account.provider,
                &credential.account.transport,
                &credential.account.account_id,
            )
            .await
            .unwrap();
    }
}
