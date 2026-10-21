//! Per-command configuration isolation. This is not a sandbox for authorized code.
use super::*;
use crate::process::Output;

#[derive(Clone, Copy)]
pub(crate) enum GitExecutionPolicy {
    Inspection,
    IndexWrite,
    CommitGuarded,
    BranchWrite,
}
pub(crate) struct ExecutionContext {
    _directory: tempfile::TempDir,
    pub env: Vec<(OsString, OsString)>,
    pub config: Vec<(OsString, OsString)>,
}
pub(crate) fn config_records(raw: &[u8]) -> Result<Vec<(OsString, OsString)>> {
    raw.split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|row| {
            let (key, value) = row
                .iter()
                .position(|b| *b == b'\n')
                .map_or((row, b"true".as_slice()), |i| (&row[..i], &row[i + 1..]));
            Ok((os_path(key)?, os_path(value)?))
        })
        .collect()
}
impl Git {
    pub(crate) async fn execution_config(
        &self,
        location: &RepositoryLocation,
        cancel: &Cancellation,
    ) -> Result<Vec<(OsString, OsString)>> {
        let raw = self
            .process
            .run_with(
                &location.root,
                &["config", "--null", "--list", "--includes"],
                Limits::default(),
                cancel,
            )
            .await?;
        if !raw.success {
            return Err(local_failure());
        }
        config_records(&raw.stdout)
    }
    pub(crate) fn execution_context(
        &self,
        location: &RepositoryLocation,
        source: &[(OsString, OsString)],
        policy: GitExecutionPolicy,
    ) -> Result<ExecutionContext> {
        if source.iter().any(|(k, v)| k.len() + v.len() > 64 * 1024)
            || source.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > 256 * 1024
        {
            return Err(AppError::new(
                ErrorCode::OutputLimit,
                "Configuração excede o limite do contexto protegido; nenhuma escrita foi iniciada.",
            ));
        }
        let mut config = source
            .iter()
            .filter(|(key, _)| {
                let key = key.as_encoded_bytes();
                // Includes have already been expanded in the real repository context.
                !key.starts_with(b"include.")
                    && !key.starts_with(b"includeif.")
                    && !key.starts_with(b"hook.")
                    && key != b"extensions.worktreeconfig"
                    && key != b"core.worktree"
            })
            .cloned()
            .collect::<Vec<_>>();
        if matches!(policy, GitExecutionPolicy::Inspection) {
            let mut drivers = Vec::new();
            for (key, _) in &config {
                if let Some(rest) = key.as_encoded_bytes().strip_prefix(b"filter.") {
                    if let Some(dot) = rest.iter().rposition(|b| *b == b'.') {
                        drivers.push(rest[..dot].to_vec());
                    }
                }
            }
            config.extend(inert_filters(&drivers)?);
        }
        if matches!(
            policy,
            GitExecutionPolicy::IndexWrite | GitExecutionPolicy::BranchWrite
        ) {
            for driver in unsupported_drivers(&config) {
                for (field, value) in [
                    ("clean", ""),
                    ("smudge", ""),
                    ("process", ""),
                    ("required", "true"),
                ] {
                    let mut key = b"filter.".to_vec();
                    key.extend_from_slice(&driver);
                    key.extend_from_slice(format!(".{field}").as_bytes());
                    config.push((os_path(&key)?, value.into()));
                }
            }
        }
        config.push(("extensions.worktreeConfig".into(), "false".into()));
        config.push((
            "core.bare".into(),
            if location.bare { "true" } else { "false" }.into(),
        ));
        let dir = tempfile::Builder::new()
            .prefix("poseidon-execution-")
            .tempdir()
            .map_err(|_| local_failure())?;
        let path = dir.path();
        std::fs::create_dir(path.join("refs")).map_err(|_| local_failure())?;
        std::fs::create_dir(path.join("objects")).map_err(|_| local_failure())?;
        std::fs::create_dir(path.join("hooks")).map_err(|_| local_failure())?;
        config.push(("core.hooksPath".into(), path.join("hooks").into_os_string()));
        std::fs::copy(location.git_dir.join("HEAD"), path.join("HEAD"))
            .map_err(|_| local_failure())?;
        if matches!(
            policy,
            GitExecutionPolicy::CommitGuarded | GitExecutionPolicy::BranchWrite
        ) {
            ensure_parent_within(
                &location.common_dir,
                &location.common_dir.join("logs/refs/placeholder"),
            )?;
            ensure_parent_within(&location.git_dir, &location.git_dir.join("logs/HEAD"))?;
        }
        let reftable = source
            .iter()
            .any(|(k, v)| k == "extensions.refstorage" && v == "reftable");
        let mut minimal = String::from("[core]\nrepositoryformatversion=1\n[extensions]\n");
        minimal.push_str(&format!("objectformat={}\n", location.object_format));
        if reftable {
            minimal.push_str("refstorage=reftable\n");
        }
        std::fs::write(path.join("config"), minimal).map_err(|_| local_failure())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            std::fs::remove_dir(path.join("refs")).map_err(|_| local_failure())?;
            symlink(location.common_dir.join("refs"), path.join("refs"))
                .map_err(|_| local_failure())?;
            std::fs::create_dir(path.join("logs")).map_err(|_| local_failure())?;
            symlink(
                location.common_dir.join("logs/refs"),
                path.join("logs/refs"),
            )
            .map_err(|_| local_failure())?;
            symlink(location.git_dir.join("logs/HEAD"), path.join("logs/HEAD"))
                .map_err(|_| local_failure())?;
            for name in ["packed-refs", "reftable", "shallow", "info", "worktrees"] {
                symlink(location.common_dir.join(name), path.join(name))
                    .map_err(|_| local_failure())?;
            }
        }
        #[cfg(not(unix))]
        return Err(AppError::new(
            ErrorCode::UnsupportedState,
            "Execução protegida de alterações locais requer o contexto Unix nesta versão.",
        ));
        #[allow(unreachable_code)]
        Ok(ExecutionContext {
            env: vec![
                ("GIT_DIR".into(), path.as_os_str().to_owned()),
                ("GIT_COMMON_DIR".into(), path.as_os_str().to_owned()),
                ("GIT_WORK_TREE".into(), location.root.as_os_str().to_owned()),
                (
                    "GIT_INDEX_FILE".into(),
                    location.git_dir.join("index").into_os_string(),
                ),
                (
                    "GIT_OBJECT_DIRECTORY".into(),
                    location.common_dir.join("objects").into_os_string(),
                ),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                (
                    "GIT_CONFIG_GLOBAL".into(),
                    path.join("no-global-config").into_os_string(),
                ),
            ],
            config,
            _directory: dir,
        })
    }
    pub(crate) async fn inspect<S: AsRef<std::ffi::OsStr>>(
        &self,
        location: &RepositoryLocation,
        args: &[S],
        overrides: &[(OsString, OsString)],
        limits: Limits,
        cancel: &Cancellation,
    ) -> Result<Output> {
        let config = if overrides.is_empty() {
            self.execution_config(location, cancel).await?
        } else {
            overrides.to_vec()
        };
        let context = self.execution_context(location, &config, GitExecutionPolicy::Inspection)?;
        self.process
            .run_input_env(
                &location.root,
                args,
                &context.config,
                None,
                limits,
                cancel,
                &context.env,
            )
            .await
    }
}

/// Snapshot executable hook bytes without blocking on a concurrently substituted FIFO.
pub(crate) fn read_hook(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|_| local_failure())?;
    let before = file.metadata().map_err(|_| local_failure())?;
    if !before.is_file() {
        return Err(AppError::new(
            ErrorCode::UnsupportedState,
            "Arquivo de política não é regular.",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| local_failure())?;
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::new(
            ErrorCode::OutputLimit,
            "Arquivo de política excede 1 MiB.",
        ));
    }
    let after = file.metadata().map_err(|_| local_failure())?;
    if file_version(&before) != file_version(&after) {
        return Err(AppError::new(
            ErrorCode::WorkingTreeChanged,
            "Arquivo de política mudou durante a leitura.",
        ));
    }
    Ok(bytes)
}
fn file_version(metadata: &std::fs::Metadata) -> String {
    let mut value = format!("{}:{:?}", metadata.len(), metadata.modified());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        value.push_str(&format!(
            ":{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.mode()
        ));
    }
    value
}

impl ExecutionContext {
    pub(crate) fn hooks(&self, hooks: &[(String, Vec<u8>)]) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for (name, bytes) in hooks {
                let path = self._directory.path().join("hooks").join(name);
                std::fs::write(&path, bytes).map_err(|_| local_failure())?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| local_failure())?;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = hooks;
            Err(local_failure())
        }
    }
}

pub(crate) fn filters_digest(config: &[(OsString, OsString)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for (key, value) in config
        .iter()
        .filter(|(k, _)| k.as_encoded_bytes().starts_with(b"filter."))
    {
        for bytes in [key.as_encoded_bytes(), value.as_encoded_bytes()] {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
    }
    format!("{:x}", hash.finalize())
}
pub(crate) fn hooks_digest(hooks: &[(String, Vec<u8>)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for (name, bytes) in hooks {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}

/// Uses the same lock name as native Git. Internal semaphores do not exclude it.
/// A replaced/modified lock is left untouched rather than removed as ours.
pub(crate) struct SharedLock {
    path: PathBuf,
    file: std::fs::File,
    marker: Vec<u8>,
    active: bool,
}
impl SharedLock {
    pub(crate) fn acquire(path: PathBuf) -> Result<Self> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).read(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                index_locked()
            } else {
                local_failure()
            }
        })?;
        let marker = format!("poseidon:{}:{:?}", std::process::id(), Instant::now()).into_bytes();
        file.write_all(&marker).map_err(|_| local_failure())?;
        Ok(Self {
            path,
            file,
            marker,
            active: true,
        })
    }
    fn owns(&self) -> bool {
        let Ok(actual) = std::fs::symlink_metadata(&self.path) else {
            return false;
        };
        let Ok(own) = self.file.metadata() else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if actual.dev() != own.dev() || actual.ino() != own.ino() {
                return false;
            }
        }
        actual.is_file() && std::fs::read(&self.path).is_ok_and(|bytes| bytes == self.marker)
    }
    pub(crate) fn publish(&mut self, destination: &Path, bytes: &[u8]) -> Result<()> {
        use std::io::{Seek, Write};
        if !self.owns() {
            return Err(index_locked());
        }
        self.file.rewind().map_err(|_| local_failure())?;
        self.file.set_len(0).map_err(|_| local_failure())?;
        self.file
            .write_all(bytes)
            .and_then(|_| self.file.sync_all())
            .map_err(|_| local_failure())?;
        self.marker = bytes.to_vec();
        if !self.owns() {
            return Err(index_locked());
        }
        std::fs::rename(&self.path, destination).map_err(|_| local_failure())?;
        self.active = false;
        Ok(())
    }
}
impl Drop for SharedLock {
    fn drop(&mut self) {
        if self.active && self.owns() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) struct IndexTransaction {
    _directory: tempfile::TempDir,
    pub index: PathBuf,
    lock: SharedLock,
    destination: PathBuf,
}
impl IndexTransaction {
    pub(crate) fn acquire(location: &RepositoryLocation) -> Result<Self> {
        let lock = SharedLock::acquire(location.git_dir.join("index.lock"))?;
        let directory = tempfile::Builder::new()
            .prefix("poseidon-index-")
            .tempdir()
            .map_err(|_| local_failure())?;
        let index = directory.path().join("index");
        let destination = location.git_dir.join("index");
        match std::fs::copy(&destination, &index) {
            Ok(_) => {
                // Git compares cached entry timestamps to the index file's own
                // timestamp to detect racy-clean files. A fresh copy timestamp
                // would make old ambiguous entries appear reliably clean.
                let modified = std::fs::metadata(&destination)
                    .and_then(|metadata| metadata.modified())
                    .map_err(|_| local_failure())?;
                std::fs::File::open(&index)
                    .and_then(|file| file.set_modified(modified))
                    .map_err(|_| local_failure())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(local_failure()),
        }
        Ok(Self {
            _directory: directory,
            index,
            lock,
            destination,
        })
    }
    pub(crate) fn publish(&mut self) -> Result<()> {
        match std::fs::read(&self.index) {
            Ok(bytes) => {
                // A rejected command may never have rewritten its private
                // index. Republishing identical bytes with a newer timestamp
                // would lose Git's racy-clean protection on the real index.
                if std::fs::read(&self.destination).is_ok_and(|original| original == bytes) {
                    return Ok(());
                }
                self.lock.publish(&self.destination, &bytes)
            }
            // No index existed and Git did not create one (early refusal).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(local_failure()),
        }
    }
}
impl ExecutionContext {
    pub(crate) fn index(&mut self, path: &Path) {
        self.env
            .push(("GIT_INDEX_FILE".into(), path.as_os_str().to_owned()));
    }
}

impl ExecutionContext {
    pub(crate) fn git_dir(&self) -> &Path {
        self._directory.path()
    }
    pub(crate) fn head(&self) -> Result<Vec<u8>> {
        std::fs::read(self._directory.path().join("HEAD")).map_err(|_| local_failure())
    }
    pub(crate) fn local_config(&self, bytes: &[u8]) -> Result<PathBuf> {
        let path = self._directory.path().join("local.config");
        std::fs::write(&path, bytes).map_err(|_| local_failure())?;
        Ok(path)
    }
}

impl Git {
    /// Commit creation must not let a newly symbolic branch redirect publication.
    /// Git writes only private refs; the reviewed direct ref is published while
    /// its real, shared files-backend lock remains held by the backend.
    pub(crate) async fn private_commit_refs(
        &self,
        location: &RepositoryLocation,
        context: &ExecutionContext,
    ) -> Result<()> {
        let raw = self
            .checked(
                &location.root,
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(symref)%00",
                ],
            )
            .await?;
        let refs = context._directory.path().join("refs");
        std::fs::remove_file(&refs).map_err(|_| local_failure())?;
        std::fs::create_dir(&refs).map_err(|_| local_failure())?;
        std::fs::remove_file(context._directory.path().join("packed-refs"))
            .map_err(|_| local_failure())?;
        for row in raw.split(|b| *b == b'\n').filter(|r| !r.is_empty()) {
            let fields = row.split(|b| *b == 0).collect::<Vec<_>>();
            if fields.len() != 4
                || !fields[0].starts_with(b"refs/")
                || fields[0]
                    .split(|b| *b == b'/')
                    .any(|p| p.is_empty() || p == b".." || p == b".")
            {
                return Err(AppError::protocol());
            }
            let path = context._directory.path().join(os_path(fields[0])?);
            std::fs::create_dir_all(path.parent().ok_or_else(local_failure)?)
                .map_err(|_| local_failure())?;
            let value = if fields[2].is_empty() {
                fields[1].to_vec()
            } else {
                let mut value = b"ref: ".to_vec();
                value.extend_from_slice(fields[2]);
                value
            };
            std::fs::write(path, [value, b"\n".to_vec()].concat()).map_err(|_| local_failure())?;
        }
        let logs = context._directory.path().join("logs");
        std::fs::remove_file(logs.join("refs")).map_err(|_| local_failure())?;
        std::fs::create_dir(logs.join("refs")).map_err(|_| local_failure())?;
        std::fs::remove_file(logs.join("HEAD")).map_err(|_| local_failure())?;
        // Reflogs are appended privately, then copied under the real ref/HEAD locks.
        // No original reflog is changed before the reviewed ref is published.
        Ok(())
    }
}
impl ExecutionContext {
    pub(crate) fn publish_log(
        &self,
        location: &RepositoryLocation,
        reference: &Path,
    ) -> Result<()> {
        use std::io::Write;
        let private = self._directory.path().join("logs").join(reference);
        let entry = match std::fs::read(private) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(local_failure()),
        };
        let base = if reference == Path::new("HEAD") {
            &location.git_dir
        } else {
            &location.common_dir
        };
        let path = base.join("logs").join(reference);
        ensure_parent_within(base, &path)?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(path).map_err(|_| local_failure())?;
        if !file.metadata().map_err(|_| local_failure())?.is_file() {
            return Err(local_failure());
        }
        file.write_all(&entry)
            .and_then(|_| file.sync_all())
            .map_err(|_| local_failure())
    }
}

fn ensure_parent_within(base: &Path, path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(local_failure)?;
    let mut existing = parent;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(local_failure)?
            }
            Err(_) => return Err(local_failure()),
        }
    }
    if !existing
        .canonicalize()
        .map_err(|_| local_failure())?
        .starts_with(base)
    {
        return Err(AppError::new(
            ErrorCode::UnsupportedState,
            "O armazenamento de metadados sai do diretório Git; escrita recusada.",
        ));
    }
    std::fs::create_dir_all(parent).map_err(|_| local_failure())?;
    if !parent
        .canonicalize()
        .map_err(|_| local_failure())?
        .starts_with(base)
    {
        return Err(local_failure());
    }
    Ok(())
}

pub(crate) fn ref_path(location: &RepositoryLocation, reference: &[u8]) -> Result<PathBuf> {
    if !reference.starts_with(b"refs/heads/")
        || reference
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b".." || p == b".")
    {
        return Err(AppError::protocol());
    }
    let path = location.common_dir.join(os_path(reference)?);
    ensure_parent_within(&location.common_dir, &path)?;
    Ok(path)
}
pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}
pub(crate) struct HeadGuard {
    pub head_lock: SharedLock,
    pub ref_lock: Option<SharedLock>,
    pub reference: Option<Vec<u8>>,
    pub head: Vec<u8>,
}
impl HeadGuard {
    pub(crate) fn acquire(location: &RepositoryLocation) -> Result<Self> {
        let head_lock = SharedLock::acquire(location.git_dir.join("HEAD.lock"))?;
        if std::fs::symlink_metadata(location.git_dir.join("HEAD"))
            .map_err(|_| local_failure())?
            .is_symlink()
        {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "HEAD legado por symlink não é suportado para escrita protegida.",
            ));
        }
        let head = std::fs::read(location.git_dir.join("HEAD")).map_err(|_| local_failure())?;
        let reference = head
            .strip_prefix(b"ref: ")
            .map(|r| r.trim_ascii_end().to_vec());
        let ref_lock = if let Some(reference) = &reference {
            let path = ref_path(location, reference)?;
            let lock = SharedLock::acquire(lock_path(&path))?;
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink())
                || std::fs::read(&path).is_ok_and(|bytes| bytes.starts_with(b"ref: "))
            {
                return Err(AppError::new(
                    ErrorCode::UnsupportedState,
                    "Branch simbólica não é suportada para escrita protegida.",
                ));
            }
            Some(lock)
        } else {
            None
        };
        Ok(Self {
            head_lock,
            ref_lock,
            reference,
            head,
        })
    }
}
impl Git {
    pub(crate) async fn ensure_write_format(
        &self,
        location: &RepositoryLocation,
        index: bool,
    ) -> Result<()> {
        if !cfg!(target_os = "linux") {
            return Err(AppError::new(ErrorCode::UnsupportedState,"Escritas protegidas estão disponíveis somente no Linux nesta versão; use o terminal neste sistema."));
        }
        if self
            .checked(&location.root, &["rev-parse", "--show-ref-format"])
            .await?
            .trim_ascii_end()
            != b"files"
        {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "Escritas protegidas requerem o backend de refs files nesta versão.",
            ));
        }
        let config = self
            .execution_config(location, &Cancellation::default())
            .await?;
        if config.iter().any(|(k, v)| {
            k == "core.sharedrepository" && !matches!(v.to_str(), Some("false" | "0" | "umask"))
        }) {
            return Err(AppError::new(ErrorCode::UnsupportedState,"Repositórios compartilhados entre usuários ainda não são suportados pela publicação protegida de metadados."));
        }
        if index
            && !self
                .checked(&location.root, &["rev-parse", "--shared-index-path"])
                .await?
                .trim_ascii_end()
                .is_empty()
        {
            return Err(AppError::new(ErrorCode::UnsupportedState,"Split index ainda não é suportado pelo index privado; use o terminal sem alterar filtros."));
        }
        Ok(())
    }
}

impl Git {
    pub(crate) async fn attributes_snapshot(
        &self,
        location: &RepositoryLocation,
        config: &[(OsString, OsString)],
        paths: &[Vec<u8>],
    ) -> Result<Vec<(PathBuf, Option<String>)>> {
        use sha2::{Digest, Sha256};
        let mut files = std::collections::BTreeSet::new();
        files.insert(location.common_dir.join("info/attributes"));
        files.insert(location.root.join(".gitattributes"));
        for path in paths {
            let path = os_path(path)?;
            let mut parent = Path::new(&path).parent();
            while let Some(directory) = parent {
                if directory.as_os_str().is_empty() {
                    break;
                }
                files.insert(location.root.join(directory).join(".gitattributes"));
                parent = directory.parent();
            }
        }
        let global = self
            .inspect(
                location,
                &["config", "--path", "--get", "core.attributesfile"],
                config,
                Limits::default(),
                &Cancellation::default(),
            )
            .await?;
        if global.success {
            let path = PathBuf::from(os_path(trim_lf(&global.stdout))?);
            files.insert(if path.is_absolute() {
                path
            } else {
                location.root.join(path)
            });
        } else if global.code == Some(1) {
            if let Some(path) =
                std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty())
            {
                files.insert(PathBuf::from(path).join("git/attributes"));
            } else if let Some(path) = std::env::var_os("HOME") {
                files.insert(PathBuf::from(path).join(".config/git/attributes"));
            }
        } else {
            return Err(local_failure());
        }
        files
            .into_iter()
            .map(|path| {
                let digest = match std::fs::symlink_metadata(&path) {
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) =>
                    {
                        None
                    }
                    Err(_) => return Err(local_failure()),
                    Ok(metadata) => {
                        let bytes = read_hook(&path)?;
                        let mut hash = Sha256::new();
                        hash.update(bytes);
                        hash.update(format!("{:?}", metadata.modified()).as_bytes());
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            hash.update(metadata.ctime().to_le_bytes());
                            hash.update(metadata.ctime_nsec().to_le_bytes());
                        }
                        Some(format!("{:x}", hash.finalize()))
                    }
                };
                Ok((path, digest))
            })
            .collect()
    }
}

impl ExecutionContext {
    /// Git invokes this after taking its reference locks, before committing the
    /// transaction. This closes the direct-ref -> symref TOCTOU, not just detects it.
    pub(crate) fn guard_reference(&mut self, git: &Path, reference: &str) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let hooks = self._directory.path().join("hooks");
            let hook = hooks.join("reference-transaction");
            let delegate = hooks.join("authorized-reference-transaction");
            if hook.exists() {
                std::fs::rename(&hook, &delegate).map_err(|_| local_failure())?;
            }
            std::fs::write(&hook, REFERENCE_GUARD).map_err(|_| local_failure())?;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| local_failure())?;
            self.env.extend([
                ("POSEIDON_REF_GIT".into(), git.as_os_str().to_owned()),
                ("POSEIDON_REF_SOURCE".into(), reference.into()),
                ("POSEIDON_REF_DELEGATE".into(), delegate.into_os_string()),
            ]);
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = (git, reference);
            Err(local_failure())
        }
    }
}
#[cfg(unix)]
const REFERENCE_GUARD: &str = r#"#!/bin/sh
created=0
if [ "$1" = prepared ]; then
 "$POSEIDON_REF_GIT" symbolic-ref --quiet "$POSEIDON_REF_SOURCE" >/dev/null
 status=$?
 if [ "$status" != 1 ]; then exit 1; fi
 if [ "$POSEIDON_REF_LOG_EXISTS" = 1 ]; then
  /usr/bin/cmp -s "$POSEIDON_REF_LOG_EXPECTED" "$POSEIDON_REF_LOG_SOURCE" || exit 1
 elif [ -e "$POSEIDON_REF_LOG_SOURCE" ] || [ -L "$POSEIDON_REF_LOG_SOURCE" ]; then
  exit 1
 fi
 if [ -n "$POSEIDON_REF_LOG_DESTINATION" ]; then
  if [ -e "$POSEIDON_REF_LOG_DESTINATION" ] || [ -L "$POSEIDON_REF_LOG_DESTINATION" ]; then exit 1; fi
  if [ "$POSEIDON_REF_LOG_EXISTS" = 1 ]; then
   # Seed history under Git's source/destination locks before it appends.
   (set -C; /usr/bin/cat "$POSEIDON_REF_LOG_EXPECTED" > "$POSEIDON_REF_LOG_DESTINATION") || exit 1
   created=1
  fi
 fi
fi
status=0
if [ -x "$POSEIDON_REF_DELEGATE" ]; then
 "$POSEIDON_REF_DELEGATE" "$@"
 status=$?
fi
if [ "$status" != 0 ] && [ "$created" = 1 ]; then
 # Still inside the prepared hook, with Git's reference locks held.
 if /usr/bin/cmp -s "$POSEIDON_REF_LOG_EXPECTED" "$POSEIDON_REF_LOG_DESTINATION"; then
  /usr/bin/rm -f -- "$POSEIDON_REF_LOG_DESTINATION"
 fi
fi
exit "$status"
"#;

fn unsupported_drivers(config: &[(OsString, OsString)]) -> Vec<Vec<u8>> {
    let mut drivers = std::collections::BTreeSet::from([b"lfs".to_vec()]);
    for (key, value) in config {
        let Some(rest) = key.as_encoded_bytes().strip_prefix(b"filter.") else {
            continue;
        };
        let Some(dot) = rest.iter().rposition(|b| *b == b'.') else {
            continue;
        };
        let driver = &rest[..dot];
        let lfs = value
            .as_encoded_bytes()
            .split(|b| b.is_ascii_whitespace())
            .any(|token| {
                token.split(|b| *b == b'/').next_back().is_some_and(|name| {
                    name.trim_ascii().strip_suffix(b"\"").unwrap_or(name) == b"git-lfs"
                })
            });
        if driver == b"lfs" || lfs {
            drivers.insert(driver.to_vec());
        }
    }
    drivers.into_iter().collect()
}
impl Git {
    pub(crate) async fn reject_unsupported_filters(
        &self,
        location: &RepositoryLocation,
        config: &[(OsString, OsString)],
        paths: &[Vec<u8>],
        source: Option<&str>,
    ) -> Result<()> {
        let blocked = unsupported_drivers(config);
        if blocked.is_empty() || paths.is_empty() {
            return Ok(());
        }
        let context = self.execution_context(location, config, GitExecutionPolicy::Inspection)?;
        let mut args = vec![OsString::from("check-attr"), "-z".into(), "--stdin".into()];
        if let Some(source) = source {
            args.push(format!("--source={source}").into());
        }
        args.push("filter".into());
        let input = paths
            .iter()
            .flat_map(|p| p.iter().copied().chain(std::iter::once(0)))
            .collect::<Vec<_>>();
        let output = self
            .process
            .run_input_env(
                &location.root,
                &args,
                &context.config,
                Some(&input),
                Limits::default(),
                &Cancellation::default(),
                &context.env,
            )
            .await?;
        if !output.success {
            return Err(local_failure());
        }
        let fields = output.stdout.split(|b| *b == 0).collect::<Vec<_>>();
        if fields
            .as_chunks::<3>()
            .0
            .iter()
            .any(|row| blocked.iter().any(|d| d == row[2]))
        {
            return Err(AppError::new(ErrorCode::UnsupportedState,"Git LFS ainda não é suportado no contexto de escrita protegido. Use o terminal; nenhum filtro foi executado."));
        }
        Ok(())
    }
}

pub(crate) fn optional_log(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(local_failure()),
    };
    if !file.metadata().map_err(|_| local_failure())?.is_file() {
        return Err(local_failure());
    }
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| local_failure())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(AppError::new(
            ErrorCode::OutputLimit,
            "Arquivo de metadados excede 8 MiB; escrita recusada.",
        ));
    }
    Ok(Some(bytes))
}
impl ExecutionContext {
    pub(crate) fn guard_reflog(
        &mut self,
        location: &RepositoryLocation,
        source: &str,
        destination: Option<&str>,
        bytes: Option<&[u8]>,
    ) -> Result<()> {
        let expected = self._directory.path().join("expected-reflog");
        std::fs::write(&expected, bytes.unwrap_or_default()).map_err(|_| local_failure())?;
        let target = if let Some(name) = destination {
            let path = location.common_dir.join("logs/refs/heads").join(name);
            ensure_parent_within(&location.common_dir, &path)?;
            path.into_os_string()
        } else {
            OsString::new()
        };
        self.env.extend([
            (
                "POSEIDON_REF_LOG_SOURCE".into(),
                location
                    .common_dir
                    .join("logs")
                    .join(source)
                    .into_os_string(),
            ),
            (
                "POSEIDON_REF_LOG_EXPECTED".into(),
                expected.into_os_string(),
            ),
            (
                "POSEIDON_REF_LOG_EXISTS".into(),
                if bytes.is_some() { "1" } else { "0" }.into(),
            ),
            ("POSEIDON_REF_LOG_DESTINATION".into(), target),
        ]);
        Ok(())
    }
}

impl Git {
    pub(crate) async fn private_other_refs(
        &self,
        location: &RepositoryLocation,
        context: &ExecutionContext,
        exclude: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let output = self
            .process
            .run_input_env(
                &location.root,
                &[
                    "for-each-ref",
                    "--sort=refname",
                    "--format=%(refname)%00%(objectname)%00%(symref)%00",
                ],
                &context.config,
                None,
                Limits::default(),
                &Cancellation::default(),
                &context.env,
            )
            .await?;
        if !output.success {
            return Err(local_failure());
        }
        Ok(output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|row| !row.is_empty() && row.split(|b| *b == 0).next() != exclude)
            .flat_map(|row| row.iter().copied().chain(std::iter::once(b'\n')))
            .collect())
    }
}

/// Stabilizes occupancy of already registered worktrees using native HEAD.lock
/// files. Git has no global lock preventing a new worktree registration.
pub(crate) struct OtherHeadGuards {
    _locks: Vec<SharedLock>,
}
impl OtherHeadGuards {
    pub(crate) fn acquire(location: &RepositoryLocation) -> Result<Self> {
        let mut paths = std::collections::BTreeSet::new();
        if location.git_dir != location.common_dir {
            paths.insert(location.common_dir.join("HEAD.lock"));
        }
        match std::fs::read_dir(location.common_dir.join("worktrees")) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.map_err(|_| local_failure())?.path();
                    if path.canonicalize().map_err(|_| local_failure())? == location.git_dir {
                        continue;
                    }
                    if std::fs::symlink_metadata(path.join("HEAD")).is_ok() {
                        paths.insert(path.join("HEAD.lock"));
                    }
                    if paths.len() > 256 {
                        return Err(AppError::new(
                            ErrorCode::UnsupportedState,
                            "Mais de 256 worktrees registrados; operação de branch recusada.",
                        ));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(local_failure()),
        }
        let mut locks = Vec::new();
        for path in paths {
            ensure_parent_within(&location.common_dir, &path)?;
            locks.push(SharedLock::acquire(path)?);
        }
        Ok(Self { _locks: locks })
    }
}
