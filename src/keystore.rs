//! Loading the Continuity BLE encryption keys that were exported from macOS.
//!
//! On macOS these live in the keychain as generic-password items with service
//! `com.apple.continuity.encryption`; the item's data is a binary plist with at
//! least `keyData`, `keyIdentifier` and `lastUsedCounter` (see seemoo-lab
//! handoff-ble-viewer `KeychainAccess.swift`). They are iCloud-synced across all
//! devices on the same Apple ID, so any one of your Apple devices carries the
//! same keys your iPhone advertises with.
//!
//! Because Asahi and macOS never run at the same time, we cannot read a live
//! keychain. Instead `macos/export-keys.sh` writes the keys to a JSON file that
//! this module loads. We accept two shapes:
//!
//!   1. Our own simple JSON: `{ "keys": [ { "id": "...", "key": "<hex>" }, ... ] }`
//!   2. A raw exported binary plist (one item), parsed via the `plist` crate.
//!
//! Every key must be 16, 24 or 32 bytes (AES-128/192/256); loading fails on any
//! other length.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::gcm;

#[derive(Debug, Clone)]
pub struct HandoffKey {
    pub id: String,
    pub key: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct JsonKeyFile {
    #[serde(default)]
    ids_device_id: Option<String>,
    keys: Vec<JsonKey>,
}

#[derive(Debug, Deserialize)]
struct JsonKey {
    #[serde(default)]
    id: String,
    /// Hex-encoded AES key bytes (16, 24 or 32 bytes).
    key: String,
}

#[derive(Debug, Deserialize)]
struct PlistKeyItem {
    #[serde(rename = "keyData")]
    key_data: plist::Value,
    #[serde(rename = "keyIdentifier", default)]
    key_identifier: Option<String>,
}

pub struct KeyStore {
    pub ids_device_id: Option<String>,
    pub keys: Vec<HandoffKey>,
}

pub fn default_data_dir() -> PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local/share")
        });
    data_home.join("ac-dc")
}

/// Replace `path` with `data` at mode 0600, keeping the old file until the new
/// one is fully written: the temp file is 0600 from creation, then renamed over.
/// A symlink at `path` is replaced by the regular file, not followed.
pub fn write_private_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file in {}", parent.display()))?;
    tmp.write_all(data)
        .with_context(|| format!("write {}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

pub fn default_keys_path() -> PathBuf {
    default_data_dir().join("keys.json")
}

pub fn default_rpidentity_path() -> PathBuf {
    default_data_dir().join("rpidentity.json")
}

/// Resolve an optional CLI path: explicit arg -> XDG data path (if present) ->
/// legacy CWD path (if present) -> XDG data path.
pub fn resolve_path(explicit: Option<PathBuf>, xdg_default: PathBuf, legacy_cwd: &str) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    if xdg_default.exists() {
        return xdg_default;
    }
    let legacy = Path::new(legacy_cwd);
    if legacy.exists() {
        return legacy.to_path_buf();
    }
    xdg_default
}

impl KeyStore {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading key file {}", path.display()))?;

        // Try our JSON format first.
        if let Ok(jf) = serde_json::from_slice::<JsonKeyFile>(&bytes) {
            let keys = jf
                .keys
                .into_iter()
                .map(|k| {
                    Ok(HandoffKey {
                        id: k.id,
                        key: hex::decode(k.key.trim()).context("decoding hex key")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Self::validated(jf.ids_device_id.filter(|s| !s.trim().is_empty()), keys);
        }

        // Fall back to a single raw binary plist keychain item.
        if let Ok(item) = plist::from_bytes::<PlistKeyItem>(&bytes) {
            let key = match item.key_data {
                plist::Value::Data(d) => d,
                other => anyhow::bail!("keyData was not binary data: {other:?}"),
            };
            return Self::validated(
                None,
                vec![HandoffKey {
                    id: item.key_identifier.unwrap_or_default(),
                    key,
                }],
            );
        }

        anyhow::bail!(
            "could not parse {} as either ac-dc JSON or a keychain plist",
            path.display()
        )
    }

    fn validated(ids_device_id: Option<String>, keys: Vec<HandoffKey>) -> Result<Self> {
        for k in &keys {
            gcm::validate_key(&k.key).with_context(|| format!("key {:?}", k.id))?;
        }
        Ok(KeyStore {
            ids_device_id,
            keys,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_json(name: &str, json: &str) -> Result<KeyStore> {
        let path =
            std::env::temp_dir().join(format!("ac-dc-keystore-{}-{name}.json", std::process::id()));
        std::fs::write(&path, json).unwrap();
        let result = KeyStore::load(&path);
        std::fs::remove_file(&path).unwrap();
        result
    }

    #[test]
    fn loads_16_and_32_byte_keys() {
        let json = format!(
            r#"{{"keys":[{{"id":"a","key":"{}"}},{{"id":"b","key":"{}"}}]}}"#,
            "11".repeat(16),
            "22".repeat(32)
        );
        let store = load_json("ok", &json).expect("load");
        assert_eq!(store.keys.len(), 2);
        assert_eq!(store.keys[0].key.len(), 16);
        assert_eq!(store.keys[1].key.len(), 32);
    }

    #[test]
    fn rejects_key_of_unsupported_length() {
        let json = format!(r#"{{"keys":[{{"id":"bad","key":"{}"}}]}}"#, "33".repeat(20));
        let err = load_json("bad", &json)
            .err()
            .expect("20-byte key must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("bad") && msg.contains("got 20"), "{msg}");
    }

    #[test]
    fn write_private_atomic_replaces_with_private_mode_and_leaves_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/secret.json");
        write_private_atomic(&path, b"one").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private_atomic(&path, b"two").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
}
