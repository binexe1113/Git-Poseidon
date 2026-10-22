//! T23-B1: list, inspect and open existing worktrees (docs/23). Nothing here
//! writes to a registry, a checkout or the trust store: opening a worktree only
//! replaces the active session, after its identity is checked again.
use super::*;
use crate::git::worktrees::{stamp, Entry};

/// The last listing issued for a session. Selections name an entry by its
/// position in this listing and are refused once the registry moves on.
pub(super) struct Listing {
    session: (u32, u32),
    revision: u32,
    digest: [u8; 32],
    entries: Vec<Entry>,
}

fn listing_changed() -> AppError {
    AppError::new(
        ErrorCode::StaleSession,
        "A lista de worktrees mudou desde a leitura. Atualize a lista e selecione novamente.",
    )
}
fn identity_changed() -> AppError {
    AppError::new(
        ErrorCode::StaleSession,
        "O worktree selecionado mudou, foi movido ou removido. Atualize a lista.",
    )
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| parse::display_path(n.as_encoded_bytes()))
        .unwrap_or_else(|| parse::display_path(path.as_os_str().as_encoded_bytes()))
}

impl RepositoryService {
    /// The registry as Git reports it now. Observational: no prune, repair or
    /// maintenance runs, and no repository program can run (`worktree list`).
    pub async fn worktrees(
        &self,
        session_id: u32,
        generation: u32,
    ) -> Result<WorktreeListResponse> {
        let start = Instant::now();
        let _permit = self.worktree_queries.try_acquire().map_err(|_| busy())?;
        let location = self.session_location(session_id, generation)?;
        let git = Git::discover()?;
        let registry = git.worktree_registry(&location).await?;
        let root = location.root.canonicalize().ok();
        let worktrees = registry
            .entries
            .iter()
            .enumerate()
            .map(|(key, e)| Worktree {
                key: key as u32,
                kind: e.kind,
                name: display_name(&e.path),
                path: parse::display_path(e.path.as_os_str().as_encoded_bytes()),
                current: root.as_ref() == Some(&e.path),
                head: e.record.head.clone(),
                branch: e
                    .record
                    .branch
                    .as_deref()
                    .map(|b| parse::display_path(b.strip_prefix(b"refs/heads/").unwrap_or(b))),
                detached: e.record.detached,
                locked: e.record.locked.is_some(),
                lock_reason: e
                    .record
                    .locked
                    .as_deref()
                    .filter(|r| !r.is_empty())
                    .map(parse::display_path),
                prunable: e.record.prunable.as_deref().map(parse::display_path),
                availability: e.availability,
                reason: e.reason.map(str::to_owned),
            })
            .collect();
        let revision = self.next_plan.fetch_add(1, Ordering::Relaxed);
        // Store only if the session is still the one this listing was read for.
        self.session_location(session_id, generation)?;
        *self.worktree_listing.lock().map_err(|_| busy())? = Some(Listing {
            session: (session_id, generation),
            revision,
            digest: registry.digest,
            entries: registry.entries,
        });
        Ok(WorktreeListResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: WorktreeList {
                revision,
                worktrees,
                truncated: registry.truncated,
                trusted: self.trusted(&location),
                elapsed_ms: elapsed_ms(start),
            },
        })
    }

    /// Staged/unstaged/untracked counts of one listed worktree, read under the
    /// Inspection policy (filters neutralised), plus its effective filter permission.
    pub async fn worktree_status(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        key: u32,
    ) -> Result<WorktreeStatusResponse> {
        let _permit = self.worktree_queries.try_acquire().map_err(|_| busy())?;
        let location = self.session_location(session_id, generation)?;
        let (digest, entry) = self.listed(session_id, generation, revision, key)?;
        if entry.kind == WorktreeKind::Bare {
            return Err(unsupported("Repositório bare não tem working tree."));
        }
        let git = Git::discover()?;
        let target = self.spent(verify(&git, &location, digest, &entry).await, revision)?;
        let status = git.working_status(&target, false).await?;
        let config = git
            .execution_config(&target, &Cancellation::default())
            .await?;
        let trusted = self.trusted(&target);
        let filters = if !config
            .iter()
            .any(|(k, _)| k.as_encoded_bytes().starts_with(b"filter."))
        {
            WorktreeFilters::None
        } else if trusted
            && self.trust.filters_allowed(
                &trust::identity(&target)?,
                &git::execution::filters_digest(&config),
            )?
        {
            WorktreeFilters::Authorized
        } else {
            WorktreeFilters::ReviewRequired
        };
        Ok(WorktreeStatusResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            revision,
            data: WorktreeStatus {
                key,
                staged: status.staged,
                modified: status.modified,
                untracked: status.untracked,
                conflicts: status.conflicts,
                has_more_changes: status.has_more_changes,
                trusted,
                filters,
            },
        })
    }

    /// Makes a listed worktree the active session. Navigation only: no switch,
    /// no registry change, no new trust. Nothing is installed unless the entry is
    /// still the same worktree after its session was read.
    pub async fn open_worktree(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        key: u32,
    ) -> Result<SnapshotResponse> {
        let _mutation = self.mutation.try_acquire().map_err(|_| writing())?;
        let _permit = self.queries.try_acquire().map_err(|_| busy())?;
        let location = self.session_location(session_id, generation)?;
        let (digest, entry) = self.listed(session_id, generation, revision, key)?;
        let git = Git::discover()?;
        git.installation().await?;
        let target = self.spent(verify(&git, &location, digest, &entry).await, revision)?;
        let (session, history) = self.read_session(&git, target).await?;
        self.spent(same_worktree(&git, &location, &entry).await, revision)?;
        self.install(session, history).await
    }

    /// A refused selection is never usable again, even if the registry later
    /// returns to the listed state: the listing it came from is dropped.
    fn spent<T>(&self, result: Result<T>, revision: u32) -> Result<T> {
        if result
            .as_ref()
            .is_err_and(|e| matches!(e.code, ErrorCode::StaleSession))
        {
            let mut listing = self.worktree_listing.lock().map_err(|_| busy())?;
            if listing.as_ref().is_some_and(|l| l.revision == revision) {
                *listing = None;
            }
        }
        result
    }

    fn listed(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
        key: u32,
    ) -> Result<([u8; 32], Entry)> {
        let listing = self.worktree_listing.lock().map_err(|_| busy())?;
        let listing = listing
            .as_ref()
            .filter(|l| l.session == (session_id, generation) && l.revision == revision)
            .ok_or_else(listing_changed)?;
        let entry = listing
            .entries
            .get(key as usize)
            .ok_or_else(|| AppError::new(ErrorCode::InvalidInput, "Worktree fora da lista."))?;
        if entry.availability != WorktreeAvailability::Available {
            return Err(unsupported(
                entry
                    .reason
                    .unwrap_or("Este worktree não pode ser aberto agora."),
            ));
        }
        Ok((listing.digest, entry.clone()))
    }
}

/// Re-reads the registry and the entry: same registry identity, same directory
/// and Git directory (inodes), and Git still resolves it inside this repository.
async fn verify(
    git: &Git,
    session: &RepositoryLocation,
    digest: [u8; 32],
    entry: &Entry,
) -> Result<RepositoryLocation> {
    if git.worktree_registry(session).await?.digest != digest {
        return Err(listing_changed());
    }
    same_worktree(git, session, entry).await
}

async fn same_worktree(
    git: &Git,
    session: &RepositoryLocation,
    entry: &Entry,
) -> Result<RepositoryLocation> {
    let target = git
        .locate(&entry.path)
        .await
        .map_err(|_| identity_changed())?;
    let canonical = |p: &Path| p.canonicalize().ok();
    let expected_dir = entry.git_dir.as_deref().ok_or_else(identity_changed)?;
    let common = canonical(&session.common_dir).ok_or_else(identity_changed)?;
    if target.root != entry.path
        || canonical(&target.git_dir).as_deref() != Some(expected_dir)
        || canonical(&target.common_dir).as_ref() != Some(&common)
        || target.bare != (entry.kind == WorktreeKind::Bare)
        || stamp(&entry.path, expected_dir) != entry.stamp
        || entry.stamp.is_none()
    {
        return Err(identity_changed());
    }
    Ok(target)
}
