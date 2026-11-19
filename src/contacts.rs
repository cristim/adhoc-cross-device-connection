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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = String::new();
        for entry in &self.entries {
            content.push_str(entry);
            content.push('\n');
        }
        std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
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
}
