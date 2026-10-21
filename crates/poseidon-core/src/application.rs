use crate::{
    diff::{self, ParsedChanges, ParsedPatch},
    domain::*,
    git::{self, Git, RepositoryLocation},
    history::History,
    parse,
    process::Cancellation,
    trust::{self, TrustStore},
};
use std::{
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::Semaphore;
mod branches;
mod creation;
mod stash;

#[derive(Clone)]
struct Session {
    id: u32,
    generation: u32,
    location: RepositoryLocation,
    snapshot: RepositorySnapshot,
}

/// One active repository. No lock guard is held across a process await.
pub struct RepositoryService {
    active: Mutex<Option<Session>>,
    next_id: AtomicU32,
    queries: Semaphore,
    history: tokio::sync::Mutex<Option<History>>,
    diffs: DiffState,
    trust: TrustStore,
    /// One index write at a time (docs/04: one mutation per common dir; a single
    /// repository session exists).
    mutation: Semaphore,
    commit_plan: Mutex<Option<creation::PreparedCommit>>,
    next_plan: AtomicU32,
    branch_plan: Mutex<Option<branches::PreparedBranch>>,
    stash_listing: Mutex<Option<stash::Listing>>,
    stash_queries: Semaphore,
}

/// Commit diffs read immutable objects, so they do not wait for history queries.
/// The newest request cancels the previous one (selection changed); one Git diff
/// runs at a time.
struct DiffState {
    current: Mutex<Option<Cancellation>>,
    /// Working-change listings supersede each other, not the open diff.
    listing: Mutex<Option<Cancellation>>,
    gate: Semaphore,
    lists: Mutex<VecDeque<(ListKey, Arc<CommitChanges>)>>,
    patches: Mutex<VecDeque<(PatchKey, Arc<ParsedPatch>)>>,
    /// Latest working-change listing; only its revision is served.
    working: Mutex<Option<Arc<WorkingList>>>,
    revisions: AtomicU32,
}
type ListKey = (u32, u32, String);
type PatchKey = (u32, u32, Target, u32, bool);

#[derive(Clone, PartialEq, Eq)]
enum Target {
    Commit(String),
    Working(u32, WorkingArea),
}

/// Local changes at one moment, with the stamps that prove they are still current.
struct WorkingList {
    session: (u32, u32),
    revision: u32,
    head: Option<String>,
    /// HEAD commit or the empty tree: what staged files are compared with.
    base: String,
    /// Read configuration: neutralised filter drivers when untrusted (`git::inert_filters`).
    config: Vec<(OsString, OsString)>,
    trusted: bool,
    write_block: Option<String>,
    drivers: Vec<String>,
    staged: ParsedChanges,
    unstaged: ParsedChanges,
    untracked: ParsedChanges,
    repo: RepoStamp,
    /// Worktree stamps of every unstaged/untracked entry, by group index.
    unstaged_files: Vec<Option<FileStamp>>,
    untracked_files: Vec<Option<FileStamp>>,
}

impl WorkingList {
    fn group(&self, area: WorkingArea) -> &ParsedChanges {
        match area {
            WorkingArea::Staged => &self.staged,
            WorkingArea::Unstaged => &self.unstaged,
            WorkingArea::Untracked => &self.untracked,
        }
    }

    fn stamp(&self, area: WorkingArea, index: usize) -> Option<&Option<FileStamp>> {
        match area {
            WorkingArea::Staged => None,
            WorkingArea::Unstaged => self.unstaged_files.get(index),
            WorkingArea::Untracked => self.untracked_files.get(index),
        }
    }

    /// Same content and stamps: a new listing can keep the previous revision.
    fn same_as(&self, other: &WorkingList) -> bool {
        self.session == other.session
            && self.trusted == other.trusted
            && self.write_block == other.write_block
            && self.head == other.head
            && self.repo == other.repo
            && self.unstaged_files == other.unstaged_files
            && self.untracked_files == other.untracked_files
            && self.staged.files == other.staged.files
            && self.unstaged.files == other.unstaged.files
            && self.untracked.files == other.untracked.files
            && self.staged.entries.len() == other.staged.entries.len()
            && self.unstaged.entries.len() == other.unstaged.entries.len()
            && self.untracked.entries.len() == other.untracked.entries.len()
    }
}

/// Identity of a file without reading it. ctime catches same-size rewrites within one
/// mtime tick on file systems with fine-grained change times.
#[derive(Clone, PartialEq, Eq, Debug)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    unix: (u64, i64, i64, u32),
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        unix: {
            use std::os::unix::fs::MetadataExt;
            (meta.ino(), meta.ctime(), meta.ctime_nsec(), meta.mode())
        },
    })
}

/// Index and HEAD/ref storage. A commit, reset, stage or checkout from another
/// program changes at least one of these files.
#[derive(Clone, PartialEq, Eq, Debug)]
struct RepoStamp {
    index: Option<FileStamp>,
    head: Option<Vec<u8>>,
    refs: Vec<Option<FileStamp>>,
}

fn repo_stamp(location: &RepositoryLocation) -> RepoStamp {
    let head = std::fs::read(location.git_dir.join("HEAD")).ok();
    let mut refs = vec![
        file_stamp(&location.common_dir.join("packed-refs")),
        file_stamp(&location.common_dir.join("reftable").join("tables.list")),
    ];
    if let Some(name) = head
        .as_deref()
        .and_then(|h| h.strip_prefix(b"ref: "))
        .and_then(|r| std::str::from_utf8(r.trim_ascii_end()).ok())
        .filter(|r| r.starts_with("refs/") && !r.split('/').any(|p| p == ".."))
    {
        refs.push(file_stamp(&location.common_dir.join(name)));
    }
    RepoStamp {
        index: file_stamp(&location.git_dir.join("index")),
        head,
        refs,
    }
}

fn worktree_path(location: &RepositoryLocation, path: &[u8]) -> Result<PathBuf> {
    Ok(location.root.join(git::os_path(path)?))
}
const LIST_CACHE: usize = 8;
/// docs/04: global diff LRU of 16 MiB (the newest entry is always kept).
const PATCH_CACHE_BYTES: usize = 16 * 1024 * 1024;

struct CommitChanges {
    base: DiffBase,
    parsed: ParsedChanges,
    total: u32,
}

impl Default for RepositoryService {
    /// Trust kept in memory only (tests, harnesses).
    fn default() -> Self {
        Self::with_trust(TrustStore::memory())
    }
}

impl RepositoryService {
    /// The desktop app passes a file in its data directory.
    pub fn with_trust(trust: TrustStore) -> Self {
        Self {
            trust,
            mutation: Semaphore::new(1),
            commit_plan: Mutex::new(None),
            branch_plan: Mutex::new(None),
            stash_listing: Mutex::new(None),
            stash_queries: Semaphore::new(1),
            next_plan: AtomicU32::new(1),
            active: Mutex::new(None),
            next_id: AtomicU32::new(1),
            queries: Semaphore::new(1),
            history: tokio::sync::Mutex::new(None),
            diffs: DiffState {
                current: Mutex::new(None),
                listing: Mutex::new(None),
                gate: Semaphore::new(1),
                lists: Mutex::new(VecDeque::new()),
                patches: Mutex::new(VecDeque::new()),
                working: Mutex::new(None),
                revisions: AtomicU32::new(0),
            },
        }
    }

    /// Whether this repository's identity is trusted. An unreadable store reads as "not
    /// trusted": inspection stays protected and writes report the store error themselves.
    fn trusted(&self, location: &RepositoryLocation) -> bool {
        trust::identity(location)
            .and_then(|id| self.trust.is_trusted(&id))
            .unwrap_or(false)
    }
}

impl DiffState {
    /// Supersedes the in-flight diff request, if any.
    fn begin(&self) -> Result<Cancellation> {
        Self::supersede(&self.current)
    }

    fn supersede(slot: &Mutex<Option<Cancellation>>) -> Result<Cancellation> {
        let token = Cancellation::default();
        if let Some(previous) = slot.lock().map_err(|_| busy())?.replace(token.clone()) {
            previous.cancel();
        }
        Ok(token)
    }

    async fn run<T>(
        &self,
        token: &Cancellation,
        work: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let _permit = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(cancelled()),
            permit = self.gate.acquire() => permit.map_err(|_| busy())?,
        };
        if token.is_cancelled() {
            return Err(cancelled());
        }
        work.await
    }
}

impl RepositoryService {
    pub async fn check_git(&self) -> Result<GitInstallation> {
        Git::discover()?.installation().await
    }

    pub async fn open(&self, path: &Path) -> Result<SnapshotResponse> {
        let _mutation = self.mutation.try_acquire().map_err(|_| writing())?;
        let _permit = self.queries.try_acquire().map_err(|_| busy())?;
        if path.as_os_str().len() > 32_768 {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "O caminho excede o limite aceito.",
            ));
        }
        let git = Git::discover()?;
        git.installation().await?;
        let location = git.locate(path).await?;
        let mut history = History::new(git.clone(), location.clone()).await?;
        let mut snapshot = git.snapshot(&location, self.trusted(&location)).await?;
        apply_page(&mut snapshot, history.page(0).await?);
        let session = Session {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            generation: 1,
            location,
            snapshot,
        };
        let response = response(&session);
        *self.active.lock().map_err(|_| busy())? = Some(session);
        *self.history.lock().await = Some(history);
        Ok(response)
    }

    pub async fn refresh(&self, session_id: u32) -> Result<SnapshotResponse> {
        let _mutation = self.mutation.try_acquire().map_err(|_| writing())?;
        self.rebuild_snapshot(session_id).await
    }

    async fn rebuild_snapshot(&self, session_id: u32) -> Result<SnapshotResponse> {
        let _permit = self.queries.acquire().await.map_err(|_| busy())?;
        let mut session = self.session(session_id, None)?;
        let git = Git::discover()?;
        let location = git.locate(&session.location.root).await?;
        if location.git_dir != session.location.git_dir
            || location.common_dir != session.location.common_dir
        {
            return Err(stale());
        }
        let mut history = History::new(git.clone(), location.clone()).await?;
        session.snapshot = git.snapshot(&location, self.trusted(&location)).await?;
        apply_page(&mut session.snapshot, history.page(0).await?);
        session.location = location;
        session.generation = session.generation.checked_add(1).ok_or_else(stale)?;
        let response = response(&session);
        *self.active.lock().map_err(|_| busy())? = Some(session);
        *self.history.lock().await = Some(history);
        Ok(response)
    }

    pub async fn commit(
        &self,
        session_id: u32,
        generation: u32,
        oid: &str,
    ) -> Result<DetailResponse> {
        self.commit_at(session_id, generation, oid, None).await
    }

    pub async fn commit_at(
        &self,
        session_id: u32,
        generation: u32,
        oid: &str,
        offset: Option<u32>,
    ) -> Result<DetailResponse> {
        let _permit = self.queries.try_acquire().map_err(|_| busy())?;
        let location = {
            let active = self.active.lock().map_err(|_| busy())?;
            let session = active
                .as_ref()
                .filter(|s| s.id == session_id && s.generation == generation)
                .ok_or_else(stale)?;
            session.location.clone()
        };
        let commit = {
            let mut history = self.history.lock().await;
            let active = history.as_mut().ok_or_else(stale)?;
            if let Some(commit) = active.commit(oid) {
                commit
            } else {
                let offset = offset.ok_or_else(|| {
                    AppError::new(ErrorCode::InvalidInput, "Recarregue a página do commit.")
                })?;
                match active.page(offset).await {
                    Ok(page) => {
                        page.commits
                            .into_iter()
                            .find(|c| c.oid == oid)
                            .ok_or_else(|| {
                                AppError::new(
                                    ErrorCode::InvalidInput,
                                    "O commit não pertence à página informada.",
                                )
                            })?
                    }
                    Err(error) => {
                        if !matches!(error.code, ErrorCode::InvalidInput) {
                            self.invalidate_history(&mut history)?;
                        }
                        return Err(error);
                    }
                }
            }
        };
        let data = Git::discover()?.detail(&location, commit).await?;
        Ok(DetailResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data,
        })
    }

    pub async fn history_page(
        &self,
        session_id: u32,
        generation: u32,
        offset: u32,
    ) -> Result<HistoryResponse> {
        let _permit = self.queries.try_acquire().map_err(|_| busy())?;
        {
            let active = self.active.lock().map_err(|_| busy())?;
            if !active
                .as_ref()
                .is_some_and(|s| s.id == session_id && s.generation == generation)
            {
                return Err(stale());
            }
        }
        let mut history = self.history.lock().await;
        let result = history.as_mut().ok_or_else(stale)?.page(offset).await;
        match result {
            Ok(data) => Ok(HistoryResponse {
                schema_version: SCHEMA_VERSION,
                session_id,
                generation,
                data,
            }),
            Err(error) => {
                // Invalid input does not destroy a healthy cursor. Process/snapshot errors do.
                if !matches!(error.code, ErrorCode::InvalidInput) {
                    self.invalidate_history(&mut history)?;
                }
                Err(error)
            }
        }
    }

    /// Changed files of one commit against its first parent (or the empty tree).
    pub async fn commit_files(
        &self,
        session_id: u32,
        generation: u32,
        oid: &str,
    ) -> Result<CommitFilesResponse> {
        let start = Instant::now();
        let location = self.location(session_id, generation, oid)?;
        let token = self.diffs.begin()?;
        let changes = self
            .changes(&location, (session_id, generation, oid.to_owned()), &token)
            .await?;
        Ok(CommitFilesResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: CommitFiles {
                oid: oid.to_owned(),
                base: changes.base.clone(),
                files: changes.parsed.files.clone(),
                total_files: changes.total,
                has_more_files: changes.total as usize > changes.parsed.files.len(),
                additions: changes.parsed.additions,
                deletions: changes.parsed.deletions,
                elapsed_ms: elapsed_ms(start),
            },
        })
    }

    /// One page of a file's patch. The file is addressed by its index in `commit_files`.
    pub async fn file_diff(
        &self,
        session_id: u32,
        generation: u32,
        oid: &str,
        index: u32,
        offset: u32,
        allow_large: bool,
    ) -> Result<FileDiffResponse> {
        let start = Instant::now();
        let location = self.location(session_id, generation, oid)?;
        let token = self.diffs.begin()?;
        let changes = self
            .changes(&location, (session_id, generation, oid.to_owned()), &token)
            .await?;
        let (Some(file), Some(change)) = (
            changes.parsed.files.get(index as usize),
            changes.parsed.entries.get(index as usize),
        ) else {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Arquivo fora da lista deste commit.",
            ));
        };
        let key: PatchKey = (
            session_id,
            generation,
            Target::Commit(oid.to_owned()),
            index,
            allow_large,
        );
        let patch = match self.cached_patch(&key)? {
            Some(hit) => hit,
            None => {
                let old_blob = blob(&change.old_oid, change.is_gitlink());
                let new_blob = blob(&change.new_oid, change.is_gitlink());
                let git = Git::discover()?;
                let (old_size, new_size) = self
                    .diffs
                    .run(&token, async {
                        tokio::try_join!(
                            async {
                                match old_blob {
                                    Some(b) => git.blob_size(&location, b, &token).await.map(Some),
                                    None => Ok(None),
                                }
                            },
                            async {
                                match new_blob {
                                    Some(b) => git.blob_size(&location, b, &token).await.map(Some),
                                    None => Ok(None),
                                }
                            }
                        )
                    })
                    .await?;
                let mut patch = match gate(old_size, new_size, file.binary, allow_large) {
                    Some(outcome) => ParsedPatch::empty(outcome),
                    None => {
                        let limit = line_limit(allow_large);
                        self.diffs
                            .run(
                                &token,
                                git.file_patch(
                                    &location,
                                    oid,
                                    &changes.base,
                                    change,
                                    limit,
                                    &token,
                                ),
                            )
                            .await?
                    }
                };
                patch.old_size = old_size;
                patch.new_size = new_size;
                let patch = Arc::new(patch);
                self.store_patch(key, patch.clone())?;
                patch
            }
        };
        Ok(FileDiffResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: page(
                DiffTarget::Commit {
                    oid: oid.to_owned(),
                },
                file,
                &patch,
                offset,
                start,
            ),
        })
    }

    /// Staged, unstaged and untracked files, read now. Each entry is stamped so a later
    /// diff request can prove the listing is still current.
    pub async fn working_changes(
        &self,
        session_id: u32,
        generation: u32,
    ) -> Result<WorkingChangesResponse> {
        let start = Instant::now();
        let location = self.session_location(session_id, generation)?;
        if location.bare {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Repositório bare não tem working tree.",
            ));
        }
        let token = DiffState::supersede(&self.diffs.listing)?;
        let git = Git::discover()?;
        let trusted = self.trusted(&location);
        let mut attempt = 0;
        let list = loop {
            let list = self
                .diffs
                .run(&token, read_working(&git, &location, &token, trusted, None))
                .await?;
            // A file written while Git was reading may be listed with its old content
            // but stamped with its new state: list again (bounded).
            attempt += 1;
            if !list.racy || attempt == WORKING_ATTEMPTS {
                break list;
            }
        };
        let list = self.store_listing(session_id, generation, list.list)?;
        Ok(WorkingChangesResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: working_data(&list, start),
        })
    }

    /// Grants or revokes trust for the open repository's identity. Listings read under the
    /// previous policy are dropped, so the next request reads again.
    pub async fn set_trust(&self, session_id: u32, generation: u32, trusted: bool) -> Result<()> {
        let location = self.session_location(session_id, generation)?;
        if location.bare {
            return Err(unsupported("Repositório bare não tem working tree."));
        }
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        self.trust.set(&trust::identity(&location)?, trusted)?;
        *self.commit_plan.lock().map_err(|_| busy())? = None;
        *self.diffs.working.lock().map_err(|_| busy())? = None;
        Ok(())
    }

    /// Stages a whole unstaged or untracked file (`git add`). The index version of a file
    /// with staged changes is replaced by the working tree version.
    pub async fn stage_file(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        area: WorkingArea,
        index: u32,
    ) -> Result<IndexChangeResponse> {
        if area == WorkingArea::Staged {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Este arquivo já está preparado.",
            ));
        }
        self.change_index(session_id, generation, revision, area, index)
            .await
    }

    /// Returns a staged file's index entry to HEAD (or removes it when HEAD has none).
    /// The working tree is not touched.
    pub async fn unstage_file(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        index: u32,
    ) -> Result<IndexChangeResponse> {
        self.change_index(session_id, generation, revision, WorkingArea::Staged, index)
            .await
    }

    /// Preflight under the mutation permit, one Git write, then a re-read of the
    /// touched paths. No rollback: on failure the re-read state is what the user sees.
    async fn change_index(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        area: WorkingArea,
        index: u32,
    ) -> Result<IndexChangeResponse> {
        let start = Instant::now();
        let location = self.session_location(session_id, generation)?;
        if location.bare {
            return Err(unsupported("Repositório bare não tem working tree."));
        }
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        if !self.trust.is_trusted(&trust::identity(&location)?)? {
            return Err(AppError::new(
                ErrorCode::TrustRequired,
                "Confie neste repositório para preparar ou retirar arquivos do index.",
            ));
        }
        let list = self
            .diffs
            .working
            .lock()
            .map_err(|_| busy())?
            .clone()
            .filter(|l| l.session == (session_id, generation) && l.revision == revision)
            .ok_or_else(changed)?;
        // A listing read before trust was granted used neutralised filters.
        if !list.trusted {
            return Err(changed());
        }
        if [&list.staged, &list.unstaged]
            .iter()
            .any(|g| g.entries.iter().any(|e| e.status == b'U'))
        {
            return Err(unsupported(
                "Index com conflitos: preparar e retirar ficam bloqueados nesta fase.",
            ));
        }
        let group = list.group(area);
        let (Some(file), Some(change)) = (
            group.files.get(index as usize),
            group.entries.get(index as usize),
        ) else {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Arquivo fora da lista de alterações locais.",
            ));
        };
        // Repository state now, not as listed.
        let git = Git::discover()?;
        let token = Cancellation::default();
        if let Some(reason) = git::operation_blocker(&location.git_dir) {
            return Err(unsupported(&reason));
        }
        let config = git.repo_config(&location, &token).await?;
        if config.index_hook {
            return Err(unsupported("Hook post-index-change ativo: operações de index ainda não suportam hooks. Use o terminal."));
        }
        if config.sparse {
            return Err(unsupported(
                "Sparse checkout ainda não é suportado para preparar ou retirar arquivos.",
            ));
        }
        if file.kind == ChangeKind::Unmerged {
            return Err(unsupported(
                "Arquivo em conflito: resolva-o no editor ou no terminal.",
            ));
        }
        if change.is_gitlink() || change.new_path.ends_with(b"/") {
            return Err(unsupported(
                "Submódulos e repositórios aninhados ainda não podem ser preparados aqui.",
            ));
        }
        // The selection must still describe the repository.
        let path = worktree_path(&location, &change.new_path)?;
        let current = repo_stamp(&location) == list.repo
            && list
                .stamp(area, index as usize)
                .is_none_or(|stamp| *stamp == file_stamp(&path));
        if !current {
            return Err(changed());
        }
        if location.git_dir.join("index.lock").exists() {
            return Err(git::index_locked());
        }
        let paths: Vec<Vec<u8>> = change.pathspec().into_iter().map(<[u8]>::to_vec).collect();
        let written = match area {
            WorkingArea::Staged => {
                git.set_index_entries(&location, &unstage_records(&location, change)?)
                    .await
            }
            _ => git.stage(&location, &paths).await,
        };
        // Success or not, the index is read again before answering.
        let relist_start = Instant::now();
        let relisted = self
            .relist(&git, &location, (session_id, generation), &list, &paths)
            .await;
        let git_time = written?;
        let (list, incremental) = relisted?;
        Ok(IndexChangeResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: IndexChange {
                path: file.path.clone(),
                working: working_data(&list, start),
                git_ms: git_time.as_millis().min(u32::MAX as u128) as u32,
                relist_ms: elapsed_ms(relist_start),
                incremental,
                elapsed_ms: elapsed_ms(start),
            },
        })
    }

    /// Listing after a write: worktree groups re-read only for the touched paths when the
    /// previous listing is complete; otherwise (or if the read raced a write) in full.
    async fn relist(
        &self,
        git: &Git,
        location: &RepositoryLocation,
        session: (u32, u32),
        previous: &WorkingList,
        paths: &[Vec<u8>],
    ) -> Result<(Arc<WorkingList>, bool)> {
        // Not registered as the current listing: a newer selection must not cancel it.
        let token = Cancellation::default();
        let mut incremental = [&previous.unstaged, &previous.untracked].iter().all(|g| {
            g.entries.len() == g.files.len() && g.entries.len() + paths.len() < DIFF_FILE_LIMIT
        });
        let mut attempt = 0;
        let listing = loop {
            let touched = incremental.then_some(Touched { previous, paths });
            let listing = self
                .diffs
                .run(&token, read_working(git, location, &token, true, touched))
                .await?;
            attempt += 1;
            if !listing.racy || attempt == WORKING_ATTEMPTS {
                break listing;
            }
            incremental = false;
        };
        Ok((
            self.store_listing(session.0, session.1, listing.list)?,
            incremental,
        ))
    }

    /// Publishes a listing; one identical to the current keeps its revision.
    fn store_listing(
        &self,
        session_id: u32,
        generation: u32,
        list: WorkingList,
    ) -> Result<Arc<WorkingList>> {
        self.session_location(session_id, generation)?;
        let mut current = self.diffs.working.lock().map_err(|_| busy())?;
        let mut list = WorkingList {
            session: (session_id, generation),
            revision: 0,
            ..list
        };
        Ok(match current.as_ref() {
            Some(previous) if previous.same_as(&list) => previous.clone(),
            _ => {
                list.revision = self.diffs.revisions.fetch_add(1, Ordering::Relaxed) + 1;
                let list = Arc::new(list);
                *current = Some(list.clone());
                list
            }
        })
    }

    /// One page of a local file's patch, addressed by listing revision, group and index.
    /// Fails with `WorkingTreeChanged` when the index, HEAD or that file changed after the
    /// listing, including while the patch was being read.
    #[allow(clippy::too_many_arguments)]
    pub async fn working_diff(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        area: WorkingArea,
        index: u32,
        offset: u32,
        allow_large: bool,
    ) -> Result<FileDiffResponse> {
        let start = Instant::now();
        let location = self.session_location(session_id, generation)?;
        let token = self.diffs.begin()?;
        let list = self
            .diffs
            .working
            .lock()
            .map_err(|_| busy())?
            .clone()
            .filter(|l| l.session == (session_id, generation) && l.revision == revision)
            .ok_or_else(changed)?;
        let group = list.group(area);
        let (Some(file), Some(change)) = (
            group.files.get(index as usize),
            group.entries.get(index as usize),
        ) else {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Arquivo fora da lista de alterações locais.",
            ));
        };
        let path = worktree_path(&location, &change.new_path)?;
        let current = |list: &WorkingList| -> Result<()> {
            let fresh = repo_stamp(&location) == list.repo
                && list
                    .stamp(area, index as usize)
                    .is_none_or(|stamp| *stamp == file_stamp(&path));
            fresh.then_some(()).ok_or_else(changed)
        };
        current(&list)?;
        let key: PatchKey = (
            session_id,
            generation,
            Target::Working(revision, area),
            index,
            allow_large,
        );
        let patch = match self.cached_patch(&key)? {
            Some(hit) => hit,
            None => {
                let git = Git::discover()?;
                let patch = self
                    .diffs
                    .run(&token, async {
                        local_patch(
                            &git,
                            &location,
                            &list,
                            area,
                            change,
                            file,
                            &path,
                            allow_large,
                            &token,
                        )
                        .await
                    })
                    .await?;
                // Anything written while Git or the file read ran invalidates the result.
                current(&list)?;
                let patch = Arc::new(patch);
                self.store_patch(key, patch.clone())?;
                patch
            }
        };
        Ok(FileDiffResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: page(
                DiffTarget::Working { revision, area },
                file,
                &patch,
                offset,
                start,
            ),
        })
    }

    async fn changes(
        &self,
        location: &RepositoryLocation,
        key: ListKey,
        token: &Cancellation,
    ) -> Result<Arc<CommitChanges>> {
        if let Some((_, hit)) = self
            .diffs
            .lists
            .lock()
            .map_err(|_| busy())?
            .iter()
            .find(|(k, _)| *k == key)
        {
            return Ok(hit.clone());
        }
        let git = Git::discover()?;
        let oid = key.2.as_str();
        let changes = self
            .diffs
            .run(token, async {
                let base = git.diff_base(location, oid, token).await?;
                let parsed = git.changed_files(location, oid, &base, token).await?;
                let total = parsed.entries.len() as u32;
                Ok(CommitChanges {
                    base,
                    parsed,
                    total,
                })
            })
            .await?;
        let changes = Arc::new(changes);
        let mut lists = self.diffs.lists.lock().map_err(|_| busy())?;
        lists.retain(|(k, _)| *k != key);
        lists.push_back((key, changes.clone()));
        while lists.len() > LIST_CACHE {
            lists.pop_front();
        }
        Ok(changes)
    }

    fn cached_patch(&self, key: &PatchKey) -> Result<Option<Arc<ParsedPatch>>> {
        let mut patches = self.diffs.patches.lock().map_err(|_| busy())?;
        let position = patches.iter().position(|(k, _)| k == key);
        Ok(position.map(|i| {
            let entry = patches.remove(i).unwrap();
            let patch = entry.1.clone();
            patches.push_back(entry);
            patch
        }))
    }

    fn store_patch(&self, key: PatchKey, patch: Arc<ParsedPatch>) -> Result<()> {
        let mut patches = self.diffs.patches.lock().map_err(|_| busy())?;
        patches.retain(|(k, _)| *k != key);
        patches.push_back((key, patch));
        while patches.len() > 1
            && patches.iter().map(|(_, p)| p.bytes).sum::<usize>() > PATCH_CACHE_BYTES
        {
            patches.pop_front();
        }
        Ok(())
    }

    fn location(&self, session_id: u32, generation: u32, oid: &str) -> Result<RepositoryLocation> {
        if !parse::valid_oid(oid) {
            return Err(AppError::new(ErrorCode::InvalidInput, "OID inválido."));
        }
        self.session_location(session_id, generation)
    }

    fn session_location(&self, session_id: u32, generation: u32) -> Result<RepositoryLocation> {
        let active = self.active.lock().map_err(|_| busy())?;
        Ok(active
            .as_ref()
            .filter(|s| s.id == session_id && s.generation == generation)
            .ok_or_else(stale)?
            .location
            .clone())
    }

    fn invalidate_history(&self, history: &mut Option<History>) -> Result<()> {
        *history = None;
        if let Some(session) = self.active.lock().map_err(|_| busy())?.as_mut() {
            session.generation = session.generation.saturating_add(1);
        }
        Ok(())
    }

    fn session(&self, id: u32, generation: Option<u32>) -> Result<Session> {
        self.active
            .lock()
            .map_err(|_| busy())?
            .as_ref()
            .filter(|s| s.id == id && generation.is_none_or(|g| s.generation == g))
            .cloned()
            .ok_or_else(stale)
    }
}

fn response(session: &Session) -> SnapshotResponse {
    SnapshotResponse {
        schema_version: SCHEMA_VERSION,
        session_id: session.id,
        generation: session.generation,
        data: session.snapshot.clone(),
    }
}

fn busy() -> AppError {
    AppError::new(
        ErrorCode::Busy,
        "Uma consulta já está em andamento. Aguarde e tente novamente.",
    )
}
fn cancelled() -> AppError {
    AppError::new(
        ErrorCode::Cancelled,
        "Consulta substituída por uma seleção mais recente.",
    )
}
fn elapsed_ms(start: Instant) -> u32 {
    start.elapsed().as_millis().min(u32::MAX as u128) as u32
}
fn stale() -> AppError {
    AppError::new(
        ErrorCode::StaleSession,
        "Esta sessão não está mais atualizada. Abra ou atualize o repositório.",
    )
}
fn changed() -> AppError {
    AppError::new(
        ErrorCode::WorkingTreeChanged,
        "As alterações locais mudaram desde a listagem. Atualizando a lista.",
    )
}

const WORKING_ATTEMPTS: u32 = 3;
/// Coarse file-system clocks can stamp a write slightly before the moment it happened.
const RACY_MARGIN: Duration = Duration::from_millis(50);

struct Listing {
    list: WorkingList,
    racy: bool,
}

/// Paths a write touched and the listing it started from: `diff-files` and `ls-files`
/// (which walk the worktree) read only those paths again; `diff-index --cached` reads only
/// the index and stays complete, so staged renames are still paired.
struct Touched<'a> {
    previous: &'a WorkingList,
    paths: &'a [Vec<u8>],
}

async fn read_working(
    git: &Git,
    location: &RepositoryLocation,
    token: &Cancellation,
    trusted: bool,
    touched: Option<Touched<'_>>,
) -> Result<Listing> {
    let started = SystemTime::now();
    let before = repo_stamp(location);
    let repo_config = git.repo_config(location, token).await?;
    let config = if trusted {
        Vec::new()
    } else {
        git::inert_filters(&repo_config.drivers)?
    };
    let head = git.head_oid(location, token).await?;
    let base = head
        .clone()
        .unwrap_or_else(|| git::empty_tree(location).to_owned());
    let paths: &[Vec<u8>] = touched.as_ref().map_or(&[], |t| t.paths);
    let (staged, unstaged, untracked) = tokio::try_join!(
        git.local_changes(location, WorkingArea::Staged, &base, &config, &[], token),
        git.local_changes(
            location,
            WorkingArea::Unstaged,
            &base,
            &config,
            paths,
            token
        ),
        git.local_changes(
            location,
            WorkingArea::Untracked,
            &base,
            &config,
            paths,
            token
        ),
    )?;
    // Only listed entries can be requested, so only they are stamped.
    let stamp = |e: &diff::RawChange| -> Result<Option<FileStamp>> {
        Ok(file_stamp(&worktree_path(location, &e.new_path)?))
    };
    let (unstaged, unstaged_files, untracked, untracked_files, checked) = match &touched {
        None => {
            let stamps = |c: &ParsedChanges| -> Result<Vec<Option<FileStamp>>> {
                c.entries[..c.files.len()].iter().map(stamp).collect()
            };
            let (u, t) = (stamps(&unstaged)?, stamps(&untracked)?);
            let checked: Vec<Option<FileStamp>> = u.iter().chain(&t).cloned().collect();
            (unstaged, u, untracked, t, checked)
        }
        // Untouched entries keep the stamps of the listing they come from: a file changed
        // by someone else since then must still be detected.
        Some(t) => {
            let (u, us, uc) = splice(
                &t.previous.unstaged,
                &t.previous.unstaged_files,
                unstaged,
                t.paths,
                &stamp,
            )?;
            let (n, ns, nc) = splice(
                &t.previous.untracked,
                &t.previous.untracked_files,
                untracked,
                t.paths,
                &stamp,
            )?;
            (u, us, n, ns, uc.into_iter().chain(nc).collect())
        }
    };
    let repo = repo_stamp(location);
    let threshold = started.checked_sub(RACY_MARGIN).unwrap_or(started);
    let racy = repo != before
        || checked
            .iter()
            .flatten()
            .any(|s| s.modified.is_none_or(|m| m >= threshold));
    let write_block = git::operation_blocker(&location.git_dir).or_else(|| {
        if [&staged, &unstaged].iter().any(|g| g.entries.iter().any(|e| e.status == b'U')) {
            return Some("Index com conflitos: preparar e retirar ficam bloqueados nesta fase.".into());
        }
        if repo_config.index_hook {
            return Some("Hook post-index-change ativo: operações de index ainda não suportam hooks. Use o terminal.".into());
        }
        repo_config.sparse.then(|| {
            "Sparse checkout: preparar e retirar do index ainda não são suportados.".into()
        })
    });
    Ok(Listing {
        list: WorkingList {
            session: (0, 0),
            revision: 0,
            head,
            base,
            config,
            trusted,
            write_block,
            drivers: repo_config
                .drivers
                .iter()
                .map(|d| String::from_utf8_lossy(d).into_owned())
                .collect(),
            staged,
            unstaged,
            untracked,
            repo,
            unstaged_files,
            untracked_files,
        },
        racy,
    })
}

type Stamper<'a> = dyn Fn(&diff::RawChange) -> Result<Option<FileStamp>> + 'a;

/// Previous entries outside `paths` plus the fresh entries for `paths`, in Git's order
/// (byte order of the path), renumbered. Returns the stamps aligned with the result and
/// the fresh stamps alone (for the racy check).
#[allow(clippy::type_complexity)]
fn splice(
    previous: &ParsedChanges,
    stamps: &[Option<FileStamp>],
    fresh: ParsedChanges,
    paths: &[Vec<u8>],
    stamp: &Stamper<'_>,
) -> Result<(
    ParsedChanges,
    Vec<Option<FileStamp>>,
    Vec<Option<FileStamp>>,
)> {
    let touched = |e: &diff::RawChange| paths.iter().any(|p| *p == e.new_path || *p == e.old_path);
    let mut rows: Vec<(diff::RawChange, ChangedFile, Option<FileStamp>)> = previous
        .entries
        .iter()
        .zip(&previous.files)
        .zip(stamps)
        .filter(|((e, _), _)| !touched(e))
        .map(|((e, f), s)| (e.clone(), f.clone(), s.clone()))
        .collect();
    let mut fresh_stamps = Vec::new();
    for (entry, file) in fresh.entries.into_iter().zip(fresh.files) {
        let s = stamp(&entry)?;
        fresh_stamps.push(s.clone());
        rows.push((entry, file, s));
    }
    rows.sort_by(|a, b| a.0.new_path.cmp(&b.0.new_path));
    let mut result = ParsedChanges {
        entries: Vec::with_capacity(rows.len()),
        files: Vec::with_capacity(rows.len()),
        additions: 0,
        deletions: 0,
    };
    let mut aligned = Vec::with_capacity(rows.len());
    for (index, (entry, mut file, s)) in rows.into_iter().enumerate() {
        file.index = index as u32;
        result.additions = result.additions.saturating_add(file.additions.unwrap_or(0));
        result.deletions = result.deletions.saturating_add(file.deletions.unwrap_or(0));
        result.entries.push(entry);
        result.files.push(file);
        aligned.push(s);
    }
    Ok((result, aligned, fresh_stamps))
}

/// `update-index --index-info` records that put HEAD's version of a staged change back:
/// HEAD's mode and object for paths HEAD has, removal (mode 0) for paths it lacks. A
/// rename restores the old path and removes the new one.
fn unstage_records(location: &RepositoryLocation, change: &diff::RawChange) -> Result<Vec<u8>> {
    let zero = "0".repeat(if location.object_format == "sha256" {
        64
    } else {
        40
    });
    let valid = |mode: &str, oid: &str| {
        mode.len() == 6 && mode.bytes().all(|b| (b'0'..=b'7').contains(&b)) && parse::valid_oid(oid)
    };
    if !valid(&change.old_mode, &change.old_oid) {
        return Err(AppError::protocol());
    }
    let mut records = Vec::new();
    let mut push = |mode: &str, oid: &str, path: &[u8]| {
        records.extend_from_slice(format!("{mode} {oid}\t").as_bytes());
        records.extend_from_slice(path);
        records.push(0);
    };
    let in_head = change.old_mode != "000000";
    match change.status {
        b'R' => {
            push(&change.old_mode, &change.old_oid, &change.old_path);
            push("0", &zero, &change.new_path);
        }
        b'C' | b'A' => push("0", &zero, &change.new_path),
        _ if in_head => push(&change.old_mode, &change.old_oid, &change.new_path),
        _ => push("0", &zero, &change.new_path),
    }
    Ok(records)
}

fn working_data(list: &WorkingList, start: Instant) -> WorkingChanges {
    let group = |changes: &ParsedChanges| changes.files.clone();
    WorkingChanges {
        revision: list.revision,
        head_oid: list.head.clone(),
        staged: group(&list.staged),
        unstaged: group(&list.unstaged),
        untracked: group(&list.untracked),
        total_staged: list.staged.entries.len() as u32,
        total_unstaged: list.unstaged.entries.len() as u32,
        total_untracked: list.untracked.entries.len() as u32,
        has_more_files: [&list.staged, &list.unstaged, &list.untracked]
            .iter()
            .any(|g| g.entries.len() > g.files.len()),
        trusted: list.trusted,
        write_block: list.write_block.clone(),
        filter_drivers: list.drivers.clone(),
        elapsed_ms: elapsed_ms(start),
    }
}

fn unsupported(reason: &str) -> AppError {
    AppError::new(ErrorCode::UnsupportedState, reason)
}

fn writing() -> AppError {
    AppError::new(
        ErrorCode::Busy,
        "Outra alteração do index está em andamento. Aguarde ela terminar.",
    )
}

#[allow(clippy::too_many_arguments)]
async fn local_patch(
    git: &Git,
    location: &RepositoryLocation,
    list: &WorkingList,
    area: WorkingArea,
    change: &diff::RawChange,
    file: &ChangedFile,
    path: &Path,
    allow_large: bool,
    token: &Cancellation,
) -> Result<ParsedPatch> {
    // Conflicts are shown as combined diffs by Git; resolving them is a later phase.
    if file.kind == ChangeKind::Unmerged {
        return Ok(ParsedPatch::empty(DiffContent::NoContent));
    }
    let gitlink = change.is_gitlink();
    let index_size = |oid: &str| {
        let oid = blob(oid, gitlink).map(str::to_owned);
        async move {
            match oid {
                Some(oid) => git.blob_size(location, &oid, token).await.map(Some),
                None => Ok(None),
            }
        }
    };
    let worktree_size = || {
        std::fs::symlink_metadata(path)
            .ok()
            .filter(|m| !m.is_dir() && !gitlink)
            .map(|m| m.len())
    };
    let (old_size, new_size) = match area {
        WorkingArea::Staged => {
            tokio::try_join!(index_size(&change.old_oid), index_size(&change.new_oid))?
        }
        WorkingArea::Unstaged => (index_size(&change.old_oid).await?, worktree_size()),
        WorkingArea::Untracked => (None, worktree_size()),
    };
    let mut patch = match gate(old_size, new_size, file.binary, allow_large) {
        Some(outcome) => ParsedPatch::empty(outcome),
        None if area == WorkingArea::Untracked => untracked_content(path, allow_large).await?,
        None => {
            git.local_patch(
                location,
                area,
                &list.base,
                change,
                &list.config,
                line_limit(allow_large),
                token,
            )
            .await?
        }
    };
    patch.old_size = old_size;
    patch.new_size = new_size;
    Ok(patch)
}

/// Reads an untracked file directly (no Git filter runs). A directory is a nested
/// repository; other special files have no content preview.
async fn untracked_content(path: &Path, allow_large: bool) -> Result<ParsedPatch> {
    let path = path.to_owned();
    let read = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
        use std::io::Read;
        let meta = std::fs::symlink_metadata(&path).map_err(|_| changed())?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path).map_err(|_| changed())?;
            #[cfg(unix)]
            return Ok(Some(std::os::unix::ffi::OsStringExt::into_vec(
                target.into_os_string(),
            )));
            #[cfg(not(unix))]
            return Ok(Some(target.to_string_lossy().into_owned().into_bytes()));
        }
        if !meta.is_file() {
            return Ok(None);
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        // Never block on a file swapped for a FIFO after the check, never follow a link.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            libc::O_NONBLOCK | libc::O_NOFOLLOW,
        );
        let file = options.open(&path).map_err(|_| changed())?;
        if !file.metadata().is_ok_and(|m| m.is_file()) {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        file.take(DIFF_HARD_BLOB_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| changed())?;
        Ok(Some(bytes))
    })
    .await
    .map_err(|_| changed())??;
    match read {
        None => Ok(ParsedPatch::empty(DiffContent::NoContent)),
        Some(bytes) if bytes.len() as u64 > DIFF_HARD_BLOB_BYTES => {
            Ok(ParsedPatch::empty(DiffContent::TooLarge))
        }
        Some(bytes) => diff::untracked_patch(&bytes, line_limit(allow_large)),
    }
}

/// All-zero OIDs mark an absent side; gitlinks point at commits, not blobs.
fn blob(oid: &str, gitlink: bool) -> Option<&str> {
    (!gitlink && !oid.is_empty() && !oid.bytes().all(|b| b == b'0')).then_some(oid)
}

/// Size gates before any patch is read (docs/04).
fn gate(
    old: Option<u64>,
    new: Option<u64>,
    binary: bool,
    allow_large: bool,
) -> Option<DiffContent> {
    let largest = old.max(new).unwrap_or(0);
    if largest > DIFF_HARD_BLOB_BYTES {
        Some(DiffContent::TooLarge)
    } else if largest > DIFF_SOFT_BLOB_BYTES && !allow_large {
        Some(DiffContent::Large)
    } else if binary {
        Some(DiffContent::Binary)
    } else {
        None
    }
}

fn line_limit(allow_large: bool) -> usize {
    if allow_large {
        DIFF_LINE_HARD_LIMIT
    } else {
        DIFF_LINE_LIMIT
    }
}

/// One IPC page of a parsed patch.
fn page(
    target: DiffTarget,
    file: &ChangedFile,
    patch: &ParsedPatch,
    offset: u32,
    start: Instant,
) -> FileDiff {
    let total = patch.lines.len();
    let from = (offset as usize).min(total);
    let to = (from + DIFF_PAGE_LINES).min(total);
    FileDiff {
        target,
        file: file.clone(),
        content: patch.content,
        offset: from as u32,
        page_lines: DIFF_PAGE_LINES as u32,
        lines: patch.lines[from..to].to_vec(),
        total_lines: total as u32,
        hunk_starts: patch.hunk_starts.clone(),
        max_columns: patch.max_columns,
        truncated: patch.truncated,
        lossy: patch.lossy,
        old_size: patch.old_size,
        new_size: patch.new_size,
        elapsed_ms: elapsed_ms(start),
    }
}

fn apply_page(snapshot: &mut RepositorySnapshot, page: HistoryPage) {
    snapshot.commits = page.commits;
    snapshot.graph = page.graph;
    snapshot.has_more_commits = !page.complete;
    snapshot.read_elapsed_ms = snapshot
        .read_elapsed_ms
        .saturating_add(page.read_elapsed_ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_diff_request_cancels_the_previous_token() {
        let service = RepositoryService::default();
        let first = service.diffs.begin().unwrap();
        let second = service.diffs.begin().unwrap();
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
    }

    #[tokio::test]
    async fn a_superseded_request_waiting_for_the_gate_returns_cancelled() {
        let service = std::sync::Arc::new(RepositoryService::default());
        let held = service.diffs.gate.acquire().await.unwrap();
        let token = service.diffs.begin().unwrap();
        let waiting = {
            let service = service.clone();
            tokio::spawn(async move { service.diffs.run(&token, async { Ok(()) }).await })
        };
        tokio::task::yield_now().await;
        service.diffs.begin().unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("superseded request kept waiting")
            .unwrap();
        assert!(matches!(result.unwrap_err().code, ErrorCode::Cancelled));
        drop(held);
    }

    #[test]
    fn patch_cache_is_bounded_by_bytes_but_keeps_the_newest() {
        let service = RepositoryService::default();
        let patch = |bytes| {
            Arc::new(ParsedPatch {
                bytes,
                ..ParsedPatch::empty(DiffContent::Text)
            })
        };
        let key = |i| (1, 1, Target::Commit("a".repeat(40)), i, false);
        for i in 0..4 {
            service.store_patch(key(i), patch(6 * 1024 * 1024)).unwrap();
        }
        assert_eq!(service.diffs.patches.lock().unwrap().len(), 2);
        service
            .store_patch(key(9), patch(40 * 1024 * 1024))
            .unwrap();
        assert_eq!(service.diffs.patches.lock().unwrap().len(), 1);
        assert!(service.cached_patch(&key(9)).unwrap().is_some());
    }
}
