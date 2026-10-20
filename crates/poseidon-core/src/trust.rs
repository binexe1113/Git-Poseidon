//! Per-repository trust for writes, which may run repository-configured programs
//! (clean/process filters such as Git LFS). Inspection never needs it (docs/09, docs/17).
use crate::{
    domain::{AppError, ErrorCode, Result},
    git::RepositoryLocation,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// A repository is its common directory, identified by path, device, inode and birth time.
/// Device+inode alone is insufficient: filesystems can reuse the same inode immediately
/// after deleting and recreating `.git`. If birth time is unavailable, fail closed for
/// persistent trust instead of potentially trusting an unrelated replacement repository.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepoIdentity {
    common_dir: String,
    file_id: String,
}

pub(crate) fn identity(location: &RepositoryLocation) -> Result<RepoIdentity> {
    let meta = std::fs::metadata(&location.common_dir).map_err(|_| unavailable())?;
    // Persisting trust requires an identity that changes when a directory is recreated.
    // Missing birth time must not fall back to path or inode alone (inode reuse).
    let birth = meta
        .created()
        .map_err(|_| unavailable())?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| unavailable())?;
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}:{}", meta.dev(), meta.ino(), birth.as_nanos())
    };
    #[cfg(not(unix))]
    let file_id = format!("created:{}", birth.as_nanos());
    Ok(RepoIdentity {
        common_dir: path_key(&location.common_dir),
        file_id,
    })
}

/// Lossless text for any path: UTF-8 as is, otherwise the bytes in hex.
fn path_key(path: &std::path::Path) -> String {
    match path.to_str() {
        Some(text) => text.to_owned(),
        #[cfg(unix)]
        None => {
            use std::os::unix::ffi::OsStrExt;
            let hex: String = path
                .as_os_str()
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("bytes:{hex}")
        }
        #[cfg(not(unix))]
        None => path.to_string_lossy().into_owned(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(flatten)]
    identity: RepoIdentity,
    granted_at: u64,
    #[serde(default)]
    commit_hooks: bool,
    #[serde(default)]
    branch_hooks: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    repositories: Vec<Entry>,
}

/// Granted identities. With a file, every check reads it again (another app instance may
/// have revoked trust); writes replace it atomically with owner-only permissions.
pub struct TrustStore {
    path: Option<PathBuf>,
    memory: Mutex<Vec<Entry>>,
}

impl TrustStore {
    /// Nothing persists; used by tests and harnesses.
    pub fn memory() -> Self {
        Self {
            path: None,
            memory: Mutex::new(Vec::new()),
        }
    }

    pub fn file(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            memory: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn is_trusted(&self, identity: &RepoIdentity) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self.load(&guard)?.iter().any(|e| e.identity == *identity))
    }

    pub(crate) fn set(&self, identity: &RepoIdentity, trusted: bool) -> Result<()> {
        let mut guard = self.memory.lock().map_err(|_| unavailable())?;
        let mut entries = self.load(&guard)?;
        entries.retain(|e| e.identity != *identity);
        if trusted {
            entries.push(Entry {
                identity: identity.clone(),
                commit_hooks: false,
                branch_hooks: false,
                granted_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
        }
        match &self.path {
            None => *guard = entries,
            Some(path) => save(path, &entries)?,
        }
        Ok(())
    }

    pub(crate) fn hooks_trusted(&self, identity: &RepoIdentity) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self
            .load(&guard)?
            .iter()
            .any(|e| e.identity == *identity && e.commit_hooks))
    }

    pub(crate) fn set_hooks(&self, identity: &RepoIdentity, trusted: bool) -> Result<()> {
        let mut guard = self.memory.lock().map_err(|_| unavailable())?;
        let mut entries = self.load(&guard)?;
        let entry = entries
            .iter_mut()
            .find(|e| e.identity == *identity)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::TrustRequired,
                    "Conceda primeiro a confiança do repositório no painel de alterações.",
                )
            })?;
        entry.commit_hooks = trusted;
        match &self.path {
            None => *guard = entries,
            Some(path) => save(path, &entries)?,
        }
        Ok(())
    }

    pub(crate) fn branch_hooks_trusted(&self, identity: &RepoIdentity) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self
            .load(&guard)?
            .iter()
            .any(|e| e.identity == *identity && e.branch_hooks))
    }

    pub(crate) fn set_branch_hooks(&self, identity: &RepoIdentity, trusted: bool) -> Result<()> {
        let mut guard = self.memory.lock().map_err(|_| unavailable())?;
        let mut entries = self.load(&guard)?;
        let entry = entries
            .iter_mut()
            .find(|e| e.identity == *identity)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::TrustRequired,
                    "Conceda primeiro a confiança do repositório no painel de alterações.",
                )
            })?;
        entry.branch_hooks = trusted;
        match &self.path {
            None => *guard = entries,
            Some(path) => save(path, &entries)?,
        }
        Ok(())
    }

    fn load(&self, memory: &[Entry]) -> Result<Vec<Entry>> {
        let Some(path) = &self.path else {
            return Ok(memory.to_vec());
        };
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<StoreFile>(&bytes)
                .map(|f| f.repositories)
                // A damaged file is never silently replaced: trust stays withheld.
                .map_err(|_| unavailable()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(unavailable()),
        }
    }
}

fn save(path: &std::path::Path, entries: &[Entry]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(unavailable)?;
    std::fs::create_dir_all(parent).map_err(|_| unavailable())?;
    let bytes = serde_json::to_vec_pretty(&StoreFile {
        version: 1,
        repositories: entries.to_vec(),
    })
    .map_err(|_| unavailable())?;
    let temp = parent.join(format!(".trust-{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temp).map_err(|_| unavailable())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| unavailable())?;
    std::fs::rename(&temp, path).map_err(|_| unavailable())
}

fn unavailable() -> AppError {
    AppError::new(
        ErrorCode::CommandFailed,
        "Não foi possível ler ou gravar as permissões de confiança do aplicativo.",
    )
}
