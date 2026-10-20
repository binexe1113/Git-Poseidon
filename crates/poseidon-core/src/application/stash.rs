use super::branches::{ensure_location, status_paths};
use super::*;
use git::branches::BranchState;

pub(super) struct Listing {
    session: (u32, u32),
    revision: u32,
    raw: Vec<u8>,
    stamp: Option<FileStamp>,
}
pub(super) struct PreparedStash {
    token: u32,
    session: (u32, u32),
    created: Instant,
    action: StashAction,
    state: BranchState,
    repo: RepoStamp,
    log: Vec<u8>,
    log_stamp: Option<FileStamp>,
    files: Vec<(PathBuf, Option<FileStamp>)>,
    identity: trust::RepoIdentity,
    block: Option<String>,
}
fn log_stamp(l: &RepositoryLocation) -> Option<FileStamp> {
    file_stamp(&l.common_dir.join("logs/refs/stash"))
}
fn entries(raw: &[u8], revision: u32) -> Result<Vec<StashEntry>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let fields: Vec<_> = raw
        .strip_suffix(&[0])
        .unwrap_or(raw)
        .split(|b| *b == 0)
        .collect();
    if !fields.len().is_multiple_of(5) {
        return Err(unsupported("Reflog de stash inválido."));
    }
    fields
        .as_chunks::<5>()
        .0
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let oid = parse::text(f[0]);
            if !parse::valid_oid(&oid) {
                return Err(unsupported("OID de stash inválido."));
            }
            let message = parse::text(f[1]);
            let branch = message
                .strip_prefix("On ")
                .or_else(|| message.strip_prefix("WIP on "))
                .and_then(|v| v.split_once(':'))
                .map(|v| v.0.to_owned())
                .filter(|branch| branch != "(no branch)");
            Ok(StashEntry {
                selection: StashSelection {
                    revision,
                    index: i as u32,
                    oid,
                },
                reference: format!("stash@{{{i}}}"),
                message,
                timestamp: parse::text(f[2])
                    .rsplit_once("@{")
                    .and_then(|(_, date)| date.strip_suffix('}'))
                    .ok_or_else(|| unsupported("Data do reflog inválida."))?
                    .to_owned(),
                branch,
            })
        })
        .collect()
}
impl RepositoryService {
    pub async fn stashes(&self, session_id: u32, generation: u32) -> Result<StashListResponse> {
        let start = Instant::now();
        let _permit = self.stash_queries.try_acquire().map_err(|_| busy())?;
        let l = self.session_location(session_id, generation)?;
        let git = Git::discover()?;
        ensure_location(&git, &l).await?;
        let stamp = log_stamp(&l);
        let raw = git.stash_log(&l).await?;
        if stamp != log_stamp(&l) {
            return Err(changed());
        }
        self.session_location(session_id, generation)?;
        let mut listing = self.stash_listing.lock().map_err(|_| busy())?;
        let revision = match listing
            .as_ref()
            .filter(|v| v.session == (session_id, generation) && v.raw == raw && v.stamp == stamp)
        {
            Some(v) => v.revision,
            None => self.next_plan.fetch_add(1, Ordering::Relaxed),
        };
        let mut data = entries(&raw, revision)?;
        let has_more = data.len() > 1000;
        data.truncate(1000);
        *listing = Some(Listing {
            session: (session_id, generation),
            revision,
            raw,
            stamp,
        });
        Ok(StashListResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: StashList {
                entries: data,
                has_more,
                elapsed_ms: elapsed_ms(start),
            },
        })
    }
    async fn resolve_stash(
        &self,
        git: &Git,
        l: &RepositoryLocation,
        session: (u32, u32),
        selection: &StashSelection,
    ) -> Result<()> {
        let (raw, stamp) = {
            let listing = self.stash_listing.lock().map_err(|_| busy())?;
            let v = listing
                .as_ref()
                .filter(|v| v.session == session && v.revision == selection.revision)
                .ok_or_else(stale)?;
            let e = entries(&v.raw, v.revision)?;
            if e.get(selection.index as usize)
                .is_none_or(|e| e.selection != *selection)
            {
                return Err(stale());
            }
            (v.raw.clone(), v.stamp.clone())
        };
        if log_stamp(l) != stamp || git.stash_log(l).await? != raw {
            return Err(stale());
        }
        Ok(())
    }
    pub async fn stash_details(
        &self,
        session_id: u32,
        generation: u32,
        selection: StashSelection,
    ) -> Result<StashDetailsResponse> {
        // Serialize metadata reads with mutations; immutable patches use the existing diff gate.
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let l = self.session_location(session_id, generation)?;
        let git = Git::discover()?;
        ensure_location(&git, &l).await?;
        self.resolve_stash(&git, &l, (session_id, generation), &selection)
            .await?;
        let parents = git.stash_parents(&l, &selection.oid).await?;
        let mut data = Vec::new();
        for oid in std::iter::once(&selection.oid).chain(parents.iter().skip(1)) {
            data.push(self.commit_files(session_id, generation, oid).await?.data);
        }
        self.resolve_stash(&git, &l, (session_id, generation), &selection)
            .await?;
        Ok(StashDetailsResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data,
        })
    }
    pub async fn prepare_stash(
        &self,
        session_id: u32,
        generation: u32,
        action: StashAction,
    ) -> Result<StashPlanResponse> {
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        // A failed preparation cannot leave an older confirmation usable.
        *self.stash_plan.lock().map_err(|_| busy())? = None;
        let l = self.session_location(session_id, generation)?;
        if l.bare {
            return Err(unsupported("Stash requer working tree."));
        }
        let git = Git::discover()?;
        ensure_location(&git, &l).await?;
        let identity = trust::identity(&l)?;
        let trusted = self.trust.is_trusted(&identity)?;
        let repo = repo_stamp(&l);
        let state = git.branch_state(&l).await?;
        let log_stamp = log_stamp(&l);
        let log = git.stash_log(&l).await?;
        let status = parse::status(&state.status)?;
        let config = git.repo_config(&l, &Cancellation::default()).await?;
        let mut block = None;
        if !trusted {
            block = Some("Conceda confiança a este repositório antes de executar.".into());
        }
        if git::operation_state(&l.git_dir) != git::NO_OPERATION || status.conflicts > 0 {
            block =
                Some("Há conflitos ou uma operação Git em andamento. Revise no terminal.".into());
        }
        if state.head.starts_with("unborn:")
            || matches!(git.head(&l.root).await?, Head::Unborn { .. })
        {
            block = Some("Crie o commit inicial antes de usar stash.".into());
        }
        if config.sparse
            || state
                .index
                .split(|b| *b == 0)
                .any(|r| r.starts_with(b"160000 "))
            || status.changes.iter().any(|c| c.submodule)
        {
            block =
                Some("Sparse checkout/index e submódulos não são suportados para stash.".into());
        }
        // Conservative T21 policy: grants for branch/commit hooks do not authorize stash.
        if !config.drivers.is_empty()
            || !state.hooks.is_empty()
            || state.config.split(|b| *b == 0).any(|r| {
                r.starts_with(b"hook.")
                    || (r.starts_with(b"merge.")
                        && r.split(|b| *b == b'\n')
                            .next()
                            .is_some_and(|k| k.ends_with(b".driver")))
            })
        {
            block = Some("Stash com filtros externos, drivers de merge ou hooks ativos não é suportado nesta fase; nenhum programa foi autorizado ou ignorado.".into());
        }
        if status.has_more_changes {
            block = Some("Mais de 500 alterações locais: use o terminal.".into());
        }
        let mut paths: Vec<String> = status
            .changes
            .iter()
            .filter(|f| match &action {
                StashAction::Create {
                    include_untracked, ..
                } => *include_untracked || !f.untracked,
                _ => true,
            })
            .map(|f| f.path.clone())
            .collect();
        match &action {
            StashAction::Create { message, .. } => {
                if message.len() > 4096 || message.contains(['\0', '\n', '\r']) {
                    return Err(AppError::new(
                        ErrorCode::InvalidInput,
                        "Mensagem deve ter uma linha e até 4096 bytes.",
                    ));
                }
                if paths.is_empty() {
                    block = Some("Nenhuma alteração abrangida para armazenar.".into());
                }
            }
            StashAction::Apply { selection, .. } | StashAction::Drop { selection } => {
                self.resolve_stash(&git, &l, (session_id, generation), selection)
                    .await?;
                let parents = git.stash_parents(&l, &selection.oid).await?;
                paths.clear();
                for oid in std::iter::once(&selection.oid).chain(parents.iter().skip(1)) {
                    let f = self.commit_files(session_id, generation, oid).await?.data;
                    // Unchanged gitlinks are absent from a diff but still belong to the
                    // stored layers. Do not accidentally expand T21 submodule support.
                    let records = git.committed_records(&l, oid).await?.0;
                    if records
                        .split(|b| *b == 0)
                        .any(|r| r.starts_with(b"160000 "))
                    {
                        block = Some("Stash contém submódulos; operação não suportada.".into());
                    }
                    if f.has_more_files {
                        block = Some(
                            "Stash excede o limite de arquivos da prévia; use o terminal.".into(),
                        );
                    }
                    if f.files
                        .iter()
                        .any(|c| c.old_mode == "160000" || c.new_mode == "160000")
                    {
                        block = Some("Stash contém submódulos; operação não suportada.".into());
                    }
                    paths.extend(
                        f.files
                            .into_iter()
                            .flat_map(|f| std::iter::once(f.path).chain(f.old_path)),
                    );
                }
                paths.sort();
                paths.dedup();
            }
        }
        let flags = git.checked(&l.root, &["ls-files", "-v", "-z"]).await?;
        if flags.split(|b| *b == 0).any(|r| {
            r.first()
                .is_some_and(|c| *c == b'S' || c.is_ascii_lowercase())
        }) {
            block = Some(
                "Index com skip-worktree ou assume-unchanged não é suportado para stash.".into(),
            );
        }
        let files = stash_files(&git, &l, &state.status).await?;
        if repo != repo_stamp(&l)
            || state != git.branch_state(&l).await?
            || log != git.stash_log(&l).await?
            || log_stamp != self::log_stamp(&l)
            || files.iter().any(|(p, s)| file_stamp(p) != *s)
        {
            return Err(changed());
        }
        let token = self.next_plan.fetch_add(1, Ordering::Relaxed);
        let data = StashPlan {
            token,
            action: action.clone(),
            paths,
            trusted,
            block: block.clone(),
        };
        *self.stash_plan.lock().map_err(|_| busy())? = Some(PreparedStash {
            token,
            session: (session_id, generation),
            created: Instant::now(),
            action,
            state,
            repo,
            log,
            log_stamp,
            files,
            identity,
            block,
        });
        Ok(StashPlanResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data,
        })
    }
    pub async fn execute_stash(
        &self,
        session_id: u32,
        generation: u32,
        token: u32,
    ) -> Result<BranchChangeResponse> {
        let start = Instant::now();
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let l = self.session_location(session_id, generation)?;
        let p = self
            .stash_plan
            .lock()
            .map_err(|_| busy())?
            .take()
            .filter(|p| {
                p.token == token
                    && p.session == (session_id, generation)
                    && p.created.elapsed() < Duration::from_secs(120)
            })
            .ok_or_else(stale)?;
        if let Some(b) = p.block {
            return Err(unsupported(&b));
        }
        if trust::identity(&l)? != p.identity || !self.trust.is_trusted(&p.identity)? {
            return Err(AppError::new(
                ErrorCode::TrustRequired,
                "Confiança revogada. Revise novamente.",
            ));
        }
        let git = Git::discover()?;
        ensure_location(&git, &l).await?;
        if p.repo != repo_stamp(&l)
            || p.state != git.branch_state(&l).await?
            || p.log != git.stash_log(&l).await?
            || p.log_stamp != log_stamp(&l)
            || p.files != stash_files(&git, &l, &p.state.status).await?
        {
            return Err(changed());
        }
        if git::operation_state(&l.git_dir) != git::NO_OPERATION {
            return Err(changed());
        }
        if l.git_dir.join("index.lock").exists()
            || l.common_dir.join("refs/stash.lock").exists()
            || l.common_dir.join("logs/refs/stash.lock").exists()
        {
            return Err(git::index_locked());
        }
        // Recheck state after the potentially large file inspection. This narrows
        // but cannot close the gap before native Git acquires its own locks.
        ensure_location(&git, &l).await?;
        if p.repo != repo_stamp(&l)
            || p.state != git.branch_state(&l).await?
            || p.log != git.stash_log(&l).await?
            || p.log_stamp != log_stamp(&l)
            || p.files
                .iter()
                .any(|(path, stamp)| file_stamp(path) != *stamp)
        {
            return Err(changed());
        }
        let git_start = Instant::now();
        let result = git.stash_write(&l, &p.action).await;
        let git_ms = elapsed_ms(git_start);
        let after_log = git.stash_log(&l).await;
        let after = git.branch_state(&l).await;
        let acknowledged = result.as_ref().is_ok_and(|r| r.success);
        let expected_log = after_log.as_ref().is_ok_and(|raw| match &p.action {
            StashAction::Create { .. } => {
                let Ok(e) = entries(raw, 0) else {
                    return false;
                };
                let Ok(old) = entries(&p.log, 0) else {
                    return false;
                };
                e.len() == old.len() + 1
                    && raw.splitn(6, |b| *b == 0).nth(5) == Some(p.log.as_slice())
            }
            StashAction::Apply { .. } => *raw == p.log,
            StashAction::Drop { selection } => {
                let fields: Vec<_> = p
                    .log
                    .strip_suffix(&[0])
                    .unwrap_or(&p.log)
                    .split(|b| *b == 0)
                    .collect();
                let expected: Vec<u8> = fields
                    .as_chunks::<5>()
                    .0
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != selection.index as usize)
                    .flat_map(|(_, record)| {
                        record
                            .iter()
                            .flat_map(|field| field.iter().copied().chain(std::iter::once(0)))
                    })
                    .collect();
                *raw == expected
            }
        });
        let unchanged = after_log.as_ref().is_ok_and(|s| *s == p.log)
            && after.as_ref().is_ok_and(|s| *s == p.state)
            && stash_files(&git, &l, &p.state.status)
                .await
                .is_ok_and(|f| f == p.files);
        let expected_state = match &p.action {
            StashAction::Create {
                include_untracked, ..
            } => {
                let clean = after.as_ref().is_ok_and(|s| {
                    parse::status(&s.status).is_ok_and(|status| {
                        status.conflicts == 0
                            && status.staged == 0
                            && status.modified == 0
                            && (!include_untracked || status.untracked == 0)
                    })
                });
                let saved_index = async {
                    let raw = after_log.as_ref().ok()?;
                    let e = entries(raw, 0).ok()?;
                    let oid = &e.first()?.selection.oid;
                    let parents = git.stash_parents(&l, oid).await.ok()?;
                    let records = git.committed_records(&l, &parents[1]).await.ok()?.0;
                    let expected_head = p.state.head.rsplit(':').next()?;
                    Some(records == p.state.index && parents[0] == expected_head)
                }
                .await
                .unwrap_or(false);
                clean && saved_index
            }
            StashAction::Drop { .. } => {
                after
                    .as_ref()
                    .is_ok_and(|s| s.index == p.state.index && s.status == p.state.status)
                    && stash_files(&git, &l, &p.state.status)
                        .await
                        .is_ok_and(|f| f == p.files)
            }
            StashAction::Apply { .. } => after
                .as_ref()
                .is_ok_and(|s| parse::status(&s.status).is_ok_and(|status| status.conflicts == 0)),
        };
        let outcome = if acknowledged
            && expected_log
            && expected_state
            && after.as_ref().is_ok_and(|s| {
                s.head == p.state.head && s.config == p.state.config && s.hooks == p.state.hooks
            }) {
            BranchOutcome::Applied
        } else if unchanged && result.as_ref().is_ok_and(|r| r.code.is_some()) {
            BranchOutcome::Rejected
        } else {
            BranchOutcome::Unknown
        };
        let refresh = Instant::now();
        *self.diffs.working.lock().map_err(|_| busy())? = None;
        self.diffs.patches.lock().map_err(|_| busy())?.clear();
        self.diffs.lists.lock().map_err(|_| busy())?.clear();
        *self.commit_plan.lock().map_err(|_| busy())? = None;
        *self.branch_plan.lock().map_err(|_| busy())? = None;
        let snapshot = self.rebuild_snapshot(session_id).await.ok();
        let mut message = match outcome {
            BranchOutcome::Applied => "Operação concluída. O estado final e o reflog foram relidos; apply mantém o stash.",
            BranchOutcome::Rejected => "Git recusou a operação; estado conferido sem mudanças. Revise a seleção antes de uma nova tentativa.",
            BranchOutcome::Unknown => "Resultado incerto ou aplicação com conflitos/efeitos parciais. Revise os arquivos e o index atuais. O stash não foi removido por apply. Nenhuma repetição ou rollback automático.",
        }.to_owned();
        if snapshot.is_none() {
            message.push_str(" Atualização falhou; atualize manualmente.");
        }
        Ok(BranchChangeResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: BranchChange {
                outcome,
                message,
                git_ms,
                refresh_ms: elapsed_ms(refresh),
                elapsed_ms: elapsed_ms(start),
                snapshot,
            },
        })
    }
}
async fn stash_files(
    git: &Git,
    l: &RepositoryLocation,
    status: &[u8],
) -> Result<Vec<(PathBuf, Option<FileStamp>)>> {
    // Include clean tracked files and ignored/untracked: apply can affect previously clean paths.
    let raw = git
        .checked(&l.root, &["ls-files", "-z", "--cached", "--others"])
        .await?;
    let mut paths: Vec<Vec<u8>> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(Vec::from)
        .chain(status_paths(status))
        .collect();
    paths.sort();
    paths.dedup();
    if paths.len() > 20000 {
        return Err(unsupported(
            "Mais de 20.000 arquivos na inspeção de segurança; use o terminal.",
        ));
    }
    paths
        .into_iter()
        .map(|p| {
            let p = worktree_path(l, &p)?;
            let meta = std::fs::symlink_metadata(&p).ok();
            if meta.is_some_and(|m| !m.is_file() && !m.file_type().is_symlink()) {
                return Err(unsupported("Diretórios de repositórios aninhados ou arquivos especiais não são suportados para stash."));
            }
            Ok((p.clone(), file_stamp(&p)))
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
mod test_support;
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn background_listing_does_not_contend_with_mutation_queue() {
        let f = super::test_support::Fixture::new();
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "dirty\n");
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        s.set_trust(r.session_id, r.generation, true).await.unwrap();
        let held = s.mutation.acquire().await.unwrap();
        assert!(s
            .stashes(r.session_id, r.generation)
            .await
            .unwrap()
            .data
            .entries
            .is_empty());
        drop(held);
        let _read = s.stash_queries.acquire().await.unwrap();
        let plan = s
            .prepare_stash(
                r.session_id,
                r.generation,
                StashAction::Create {
                    message: "independent queue".into(),
                    include_untracked: false,
                },
            )
            .await
            .unwrap();
        assert!(plan.data.block.is_none());
    }
    #[tokio::test]
    async fn expired_confirmation_is_consumed_without_writing() {
        let f = super::test_support::Fixture::new();
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "dirty\n");
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        s.set_trust(r.session_id, r.generation, true).await.unwrap();
        let p = s
            .prepare_stash(
                r.session_id,
                r.generation,
                StashAction::Create {
                    message: "expiry".into(),
                    include_untracked: false,
                },
            )
            .await
            .unwrap()
            .data;
        s.stash_plan.lock().unwrap().as_mut().unwrap().created =
            Instant::now() - Duration::from_secs(121);
        assert!(s
            .execute_stash(r.session_id, r.generation, p.token)
            .await
            .is_err());
        assert!(s.stash_plan.lock().unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(f.root.join("a")).unwrap(),
            "dirty\n"
        );
        assert!(!f
            .output(&["show-ref", "--verify", "refs/stash"])
            .status
            .success());
    }
}
