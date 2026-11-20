//! Trusted sender list for Contacts Only mode (`~/.config/ac-dc/contacts.txt`).
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        });
    base.join("ac-dc/contacts.txt")
}

pub fn normalize_identifier(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    ensure!(
        !trimmed.is_empty() && trimmed.len() <= 256 && !trimmed.chars().any(char::is_control),
        "invalid contact identifier"
    );
    if trimmed.contains('@') {
        ensure!(
            !trimmed.starts_with('#'),
            "contact identifier cannot start with '#'"
        );
        Ok(trimmed.to_ascii_lowercase())
    } else {
        let digits: String = trimmed
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect();
        ensure!(!digits.is_empty(), "invalid contact identifier");
        Ok(digits)
    }
}

pub fn hash_identifier(normalized: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    hasher.finalize().into()
}

#[derive(Clone, Debug, Default)]
pub struct TrustedSenders {
    entries: BTreeSet<String>,
}

impl TrustedSenders {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let mut entries = BTreeSet::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Ok(norm) = normalize_identifier(line) {
                entries.insert(norm);
            }
        }
        Ok(Self { entries })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut content = String::new();
        for entry in &self.entries {
            content.push_str(entry);
            content.push('\n');
        }
        crate::keystore::write_private_atomic(path, content.as_bytes())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn list(&self) -> Vec<String> {
        self.entries.iter().cloned().collect()
    }

    pub fn add(&mut self, raw: &str) -> Result<bool> {
        let norm = normalize_identifier(raw)?;
        Ok(self.entries.insert(norm))
    }

    pub fn remove(&mut self, raw: &str) -> Result<bool> {
        let norm = normalize_identifier(raw)?;
        Ok(self.entries.remove(&norm))
    }

    #[allow(dead_code)]
    pub fn hashes(&self) -> Vec<[u8; 32]> {
        self.entries.iter().map(|e| hash_identifier(e)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_and_normalize_contacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.txt");

        let mut store = TrustedSenders::load(&path).unwrap();
        assert!(store.is_empty());

        assert!(store.add("User@iCloud.com").unwrap());
        assert!(!store.add("user@icloud.com").unwrap());
        assert!(store.add("+1 (555) 123-4567").unwrap());
        assert_eq!(store.len(), 2);
        store.save(&path).unwrap();

        let reloaded = TrustedSenders::load(&path).unwrap();
        assert_eq!(
            reloaded.list(),
            vec!["+15551234567".to_string(), "user@icloud.com".to_string()]
        );
        assert_eq!(reloaded.hashes().len(), 2);

        assert!(store.remove("USER@ICLOUD.COM").unwrap());
        assert!(!store.remove("missing@icloud.com").unwrap());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn rejects_invalid_or_control_identifiers() {
        assert!(normalize_identifier("   ").is_err());
        assert!(normalize_identifier("bad\nuser@icloud.com").is_err());
        assert!(normalize_identifier("abc").is_err());
    }

    #[test]
    fn rejects_identifier_that_would_load_back_as_a_comment() {
        let mut store = TrustedSenders::default();
        assert!(store.add("#alice@example.com").is_err());
        assert!(store.add("  #alice@example.com").is_err());
        // A phone-style entry keeps only its digits, so a leading '#' stays harmless.
        assert_eq!(normalize_identifier("#123").unwrap(), "123");
    }

    #[test]
    fn save_creates_private_file_and_tightens_existing_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustedSenders::default();
        store.add("a@example.com").unwrap();

        let fresh = dir.path().join("fresh.txt");
        store.save(&fresh).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let existing = dir.path().join("existing.txt");
        std::fs::write(&existing, "b@example.com\n").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.save(&existing).unwrap();
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "a@example.com\n"
        );
    }

    #[test]
    fn failed_save_keeps_the_original_list() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.txt");
        std::fs::write(&path, "keep@example.com\n").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        let mut store = TrustedSenders::default();
        store.add("new@example.com").unwrap();
        let result = store.save(&path);

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "keep@example.com\n"
        );
    }
}
