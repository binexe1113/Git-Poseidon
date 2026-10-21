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
    let birth = birth_stamp(meta.created())?;
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}:{}", meta.dev(), meta.ino(), birth)
    };
    #[cfg(not(unix))]
    let file_id = format!("created:{birth}");
    Ok(RepoIdentity {
        common_dir: path_key(&location.common_dir),
        file_id,
    })
}

fn birth_stamp(created: std::io::Result<SystemTime>) -> Result<u128> {
    created
        .map_err(|_| unavailable())?
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| unavailable())
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
    #[serde(default)]
    filters_policy: Option<String>,
    #[serde(default)]
    commit_policy: Option<String>,
    #[serde(default)]
    branch_policy: Option<String>,
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

/// A permission lease excludes accepted grant/revoke changes until the write
/// finishes. Memory stores are already serialized by RepositoryService.
pub(crate) struct TrustLease {
    _file: Option<TrustFileGuard>,
}

struct TrustFileGuard {
    file: std::fs::File,
}
impl Drop for TrustFileGuard {
    fn drop(&mut self) {
        // flock-style locks belong to the open-file description. A subprocess
        // fork may inherit it before exec closes CLOEXEC descriptors; close
        // alone would then leave a completed update spuriously locked.
        let _ = self.file.unlock();
    }
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

    fn file_guard(&self, shared: bool) -> Result<Option<TrustFileGuard>> {
        let Some(path) = &self.path else {
            return Ok(None);
        };
        let parent = path.parent().ok_or_else(unavailable)?;
        std::fs::create_dir_all(parent).map_err(|_| unavailable())?;
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(std::path::PathBuf::from(lock_path))
            .map_err(|_| unavailable())?;
        if !file.metadata().map_err(|_| unavailable())?.is_file() {
            return Err(unavailable());
        }
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(())=>Ok(Some(TrustFileGuard { file })),
            Err(std::fs::TryLockError::WouldBlock)=>Err(AppError::new(ErrorCode::Busy,"As permissões estão em uso por outra operação ou instância. A alteração não foi aplicada; aguarde e tente novamente.")),
            Err(_)=>Err(unavailable()),
        }
    }
    pub(crate) fn lease(&self) -> Result<TrustLease> {
        Ok(TrustLease {
            _file: self.file_guard(true)?,
        })
    }

    pub(crate) fn is_trusted(&self, identity: &RepoIdentity) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self.load(&guard)?.iter().any(|e| e.identity == *identity))
    }

    #[cfg(test)]
    pub(crate) fn set(&self, identity: &RepoIdentity, trusted: bool) -> Result<()> {
        self.set_policy(identity, trusted, None)
    }
    pub(crate) fn set_policy(
        &self,
        identity: &RepoIdentity,
        trusted: bool,
        policy: Option<String>,
    ) -> Result<()> {
        let _file_guard = self.file_guard(false)?;
        let mut guard = self.memory.lock().map_err(|_| unavailable())?;
        let mut entries = self.load(&guard)?;
        entries.retain(|e| e.identity != *identity);
        if trusted {
            entries.push(Entry {
                identity: identity.clone(),
                commit_hooks: false,
                branch_hooks: false,
                filters_policy: policy,
                commit_policy: None,
                branch_policy: None,
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

    pub(crate) fn filters_allowed(&self, identity: &RepoIdentity, policy: &str) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self
            .load(&guard)?
            .iter()
            .any(|e| e.identity == *identity && e.filters_policy.as_deref() == Some(policy)))
    }
    pub(crate) fn scoped_hooks(
        &self,
        identity: &RepoIdentity,
        policy: &str,
        branch: bool,
    ) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self.load(&guard)?.iter().any(|e| {
            e.identity == *identity
                && if branch {
                    e.branch_hooks && e.branch_policy.as_deref() == Some(policy)
                } else {
                    e.commit_hooks && e.commit_policy.as_deref() == Some(policy)
                }
        }))
    }
    #[cfg(test)]
    pub(crate) fn hooks_trusted(&self, identity: &RepoIdentity) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self
            .load(&guard)?
            .iter()
            .any(|e| e.identity == *identity && e.commit_hooks))
    }

    pub(crate) fn set_hooks(
        &self,
        identity: &RepoIdentity,
        trusted: bool,
        policy: String,
    ) -> Result<()> {
        let _file_guard = self.file_guard(false)?;
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
        entry.commit_policy = trusted.then_some(policy);
        match &self.path {
            None => *guard = entries,
            Some(path) => save(path, &entries)?,
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn branch_hooks_trusted(&self, identity: &RepoIdentity) -> Result<bool> {
        let guard = self.memory.lock().map_err(|_| unavailable())?;
        Ok(self
            .load(&guard)?
            .iter()
            .any(|e| e.identity == *identity && e.branch_hooks))
    }

    pub(crate) fn set_branch_hooks(
        &self,
        identity: &RepoIdentity,
        trusted: bool,
        policy: String,
    ) -> Result<()> {
        let _file_guard = self.file_guard(false)?;
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
        entry.branch_policy = trusted.then_some(policy);
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
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| unavailable())?;
    temp.write_all(&bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| unavailable())?;
    temp.persist(path).map_err(|_| unavailable())?;
    Ok(())
}

fn unavailable() -> AppError {
    AppError::new(
        ErrorCode::CommandFailed,
        "Não foi possível ler ou gravar as permissões de confiança do aplicativo.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_invalid_birth_time_cannot_establish_identity() {
        assert!(birth_stamp(Err(std::io::Error::from(std::io::ErrorKind::Unsupported))).is_err());
        assert!(birth_stamp(Ok(UNIX_EPOCH - std::time::Duration::from_secs(1))).is_err());
        assert_eq!(
            birth_stamp(Ok(UNIX_EPOCH + std::time::Duration::from_nanos(123))).unwrap(),
            123
        );
    }

    #[test]
    fn legacy_inode_identity_never_grants_new_identity_or_hook_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trust.json");
        let legacy = br#"{"version":1,"repositories":[{"commonDir":"/repo/.git","fileId":"1:2","grantedAt":1,"commitHooks":true,"branchHooks":true}]}"#;
        std::fs::write(&path, legacy).unwrap();
        let store = TrustStore::file(path.clone());
        let current = RepoIdentity {
            common_dir: "/repo/.git".into(),
            file_id: "1:2:123".into(),
        };
        assert!(!store.is_trusted(&current).unwrap());
        assert!(!store.hooks_trusted(&current).unwrap());
        assert!(!store.branch_hooks_trusted(&current).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        store.set(&current, true).unwrap();
        let reopened = TrustStore::file(path);
        assert!(reopened.is_trusted(&current).unwrap());
        assert!(!reopened.hooks_trusted(&current).unwrap());
        assert!(!reopened.branch_hooks_trusted(&current).unwrap());
        // Same device/inode, different birth time must never inherit this grant.
        let replacement = RepoIdentity {
            file_id: "1:2:124".into(),
            ..current.clone()
        };
        assert!(!reopened.is_trusted(&replacement).unwrap());
        reopened.set(&current, false).unwrap();
        assert!(!store.is_trusted(&current).unwrap());
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[test]
    fn duplicated_descriptor_cannot_extend_a_completed_permission_update() {
        let temp = tempfile::tempdir().unwrap();
        let store = TrustStore::file(temp.path().join("trust.json"));
        let update = store.file_guard(false).unwrap().unwrap();
        // A concurrent subprocess fork can briefly inherit this open-file
        // description before CLOEXEC closes its descriptor. Closing just the
        // parent's descriptor must not extend the completed update's lock.
        let inherited = update.file.try_clone().unwrap();
        drop(update);
        let next_write = store.lease().unwrap();
        drop(next_write);
        drop(inherited);
    }

    #[test]
    fn accepted_revocation_excludes_writes_and_busy_revocation_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trust.json");
        let identity = RepoIdentity {
            common_dir: "/owned-fixture/.git".into(),
            file_id: "synthetic:fixture".into(),
        };
        let writer = TrustStore::file(path.clone());
        let settings = TrustStore::file(path.clone());
        writer.set(&identity, true).unwrap();
        let before = std::fs::read(&path).unwrap();
        let lease = writer.lease().unwrap();
        assert!(writer.is_trusted(&identity).unwrap());
        // Independent instance cannot acknowledge revocation while an authorized
        // write holds its permission lease. A second reader may still inspect it.
        let other_reader = settings.lease().unwrap();
        let error = settings.set(&identity, false).unwrap_err();
        assert!(matches!(error.code, ErrorCode::Busy));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(other_reader);
        drop(lease);
        settings.set(&identity, false).unwrap();
        let next_write = writer.lease().unwrap();
        assert!(!writer.is_trusted(&identity).unwrap());
        drop(next_write);
        // Conversely, a permission update already in progress excludes new
        // write leases and all competing grants, without replacing the store.
        let exclusive = settings.file_guard(false).unwrap();
        assert!(matches!(
            writer.lease().err().unwrap().code,
            ErrorCode::Busy
        ));
        assert!(matches!(
            writer.set(&identity, true).unwrap_err().code,
            ErrorCode::Busy
        ));
        assert!(!writer.is_trusted(&identity).unwrap());
        drop(exclusive);
    }

    #[cfg(unix)]
    #[test]
    fn lock_path_symlink_cannot_redirect_permission_writes() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trust.json");
        let victim = temp.path().join("victim");
        std::fs::write(&victim, "unchanged").unwrap();
        symlink(&victim, temp.path().join("trust.json.lock")).unwrap();
        let store = TrustStore::file(path.clone());
        let identity = RepoIdentity {
            common_dir: "/owned-fixture/.git".into(),
            file_id: "synthetic:fixture".into(),
        };
        assert!(store.set(&identity, true).is_err());
        assert!(store.lease().is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
    }

    #[test]
    fn a_concurrent_hook_grant_cannot_restore_another_repositorys_revocation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trust.json");
        let identity = |name: &str| RepoIdentity {
            common_dir: name.into(),
            file_id: format!("synthetic:{name}"),
        };
        let revoked = identity("revoked");
        let other = identity("other");
        let seed = TrustStore::file(path.clone());
        seed.set(&revoked, true).unwrap();
        seed.set(&other, true).unwrap();
        let initial = seed.load(&[]).unwrap();
        for round in 0..16 {
            let mut entries = initial.clone();
            // Widen the real read/modify/write window without sleeps or mocks.
            for index in 0..2048 {
                let mut entry = initial[1].clone();
                entry.identity = identity(&format!("filler-{index}"));
                entries.push(entry);
            }
            save(&path, &entries).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let p = path.clone();
            let id = revoked.clone();
            let gate = barrier.clone();
            let revoke = std::thread::spawn(move || {
                let store = TrustStore::file(p);
                gate.wait();
                store.set(&id, false)
            });
            let p = path.clone();
            let id = other.clone();
            let gate = barrier.clone();
            let grant = std::thread::spawn(move || {
                let store = TrustStore::file(p);
                gate.wait();
                store.set_hooks(&id, true, "reviewed-hook".into())
            });
            let result = revoke.join().unwrap();
            let granted = grant.join().unwrap();
            if let Err(error) = granted {
                assert!(matches!(error.code, ErrorCode::Busy));
            }
            if let Err(error) = result {
                assert!(matches!(error.code, ErrorCode::Busy));
                // A busy refusal never claims revocation; retry only as an explicit
                // caller action after both operations have returned.
                TrustStore::file(path.clone()).set(&revoked, false).unwrap();
            }
            assert!(
                !TrustStore::file(path.clone()).is_trusted(&revoked).unwrap(),
                "accepted revocation was restored by an unrelated grant in round {round}"
            );
        }
    }
}
