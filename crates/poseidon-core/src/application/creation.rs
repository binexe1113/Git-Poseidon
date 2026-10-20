use super::*;

pub(super) struct PreparedCommit {
    token: u32,
    session: (u32, u32),
    created: Instant,
    repo: RepoStamp,
    index: Vec<u8>,
    policy: git::CommitPolicy,
    hooks: Vec<Option<FileStamp>>,
    head: Option<String>,
    block: Option<String>,
}
fn hook_stamps(policy: &git::CommitPolicy) -> Vec<Option<FileStamp>> {
    git::commit::HOOKS
        .iter()
        .map(|name| file_stamp(&policy.hook_dir.join(name)))
        .collect()
}
impl RepositoryService {
    pub async fn set_commit_hook_trust(
        &self,
        session_id: u32,
        generation: u32,
        trusted: bool,
    ) -> Result<()> {
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let location = self.session_location(session_id, generation)?;
        self.trust
            .set_hooks(&trust::identity(&location)?, trusted)?;
        *self.commit_plan.lock().map_err(|_| busy())? = None;
        Ok(())
    }

    pub async fn prepare_commit(
        &self,
        session_id: u32,
        generation: u32,
        revision: u32,
    ) -> Result<CommitPlanResponse> {
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let location = self.session_location(session_id, generation)?;
        if location.bare {
            return Err(unsupported(
                "Repositórios bare não podem criar commits nesta interface.",
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
        let repo = repo_stamp(&location);
        if repo != list.repo {
            return Err(changed());
        }
        let git = Git::discover()?;
        let policy = git.commit_policy(&location).await?;
        let index = git.index_records(&location).await?;
        let identity = trust::identity(&location)?;
        let hooks_trusted = self.trust.hooks_trusted(&identity)?;
        let config = git.repo_config(&location, &Cancellation::default()).await?;
        let mut block = git::operation_blocker(&location.git_dir);
        if block.is_none()
            && [&list.staged, &list.unstaged]
                .iter()
                .any(|g| g.entries.iter().any(|e| e.status == b'U'))
        {
            block = Some(
                "O index contém conflitos. Resolva-os no terminal antes de criar commits.".into(),
            );
        }
        if config.sparse {
            block = Some("Sparse checkout/index ainda não é suportado para criar commits.".into());
        }
        if list.staged.entries.iter().any(|e| e.is_gitlink()) {
            block = Some("Commits de alterações em submódulos ainda não são suportados.".into());
        }
        if list.staged.entries.is_empty() {
            block = Some(
                "Nenhum arquivo preparado. Prepare as alterações antes de criar o commit.".into(),
            );
        }
        if policy.author.is_none() || policy.committer.is_none() {
            block = Some(
                "Identidade Git ausente: configure user.name e user.email no terminal.".into(),
            );
        }
        if let Some(signing) = &policy.signing {
            block = Some(signing.clone());
        }
        if policy.unsupported_hooks {
            block = Some("Configuração hook.* ainda não suportada. Use o terminal; nenhum hook será ignorado silenciosamente.".into());
        }
        if !self.trust.is_trusted(&identity)? {
            block = Some(
                "Conceda confiança ao repositório no painel de alterações antes de criar commits."
                    .into(),
            );
        }
        #[cfg(not(unix))]
        {
            block = Some("Criação de commits ainda requer o guard Unix nesta fase.".into());
        }
        if repo != repo_stamp(&location)
            || git.head_oid(&location, &Cancellation::default()).await? != list.head
        {
            return Err(changed());
        }
        let token = self.next_plan.fetch_add(1, Ordering::Relaxed);
        let data = CommitPlan {
            token,
            head_oid: list.head.clone(),
            author: policy.author.clone(),
            committer: policy.committer.clone(),
            staged_count: list.staged.entries.len() as u32,
            additions: list.staged.additions,
            deletions: list.staged.deletions,
            hooks: policy.hooks.clone(),
            hooks_path: parse::display_path(policy.hook_dir.as_os_str().as_encoded_bytes()),
            hooks_trusted,
            signing: policy.signing.clone(),
            block: block.clone(),
        };
        let hooks = hook_stamps(&policy);
        *self.commit_plan.lock().map_err(|_| busy())? = Some(PreparedCommit {
            token,
            session: (session_id, generation),
            created: Instant::now(),
            repo,
            index,
            policy,
            hooks,
            head: list.head.clone(),
            block,
        });
        Ok(CommitPlanResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data,
        })
    }

    pub async fn create_commit(
        &self,
        session_id: u32,
        generation: u32,
        token: u32,
        message: &str,
    ) -> Result<CreateCommitResponse> {
        validate_message(message)?;
        let start = Instant::now();
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let location = self.session_location(session_id, generation)?;
        let plan = self
            .commit_plan
            .lock()
            .map_err(|_| busy())?
            .take()
            .filter(|p| {
                p.token == token
                    && p.session == (session_id, generation)
                    && p.created.elapsed() < Duration::from_secs(60)
            })
            .ok_or_else(changed)?;
        if let Some(block) = plan.block {
            return Err(unsupported(&block));
        }
        let identity = trust::identity(&location)?;
        if !self.trust.is_trusted(&identity)? {
            return Err(AppError::new(
                ErrorCode::TrustRequired,
                "A confiança do repositório foi revogada.",
            ));
        }
        let hooks_allowed = self.trust.hooks_trusted(&identity)?;
        if !plan.policy.hooks.is_empty() && !hooks_allowed {
            return Err(AppError::new(
                ErrorCode::TrustRequired,
                "Hooks de commit precisam de autorização explícita separada.",
            ));
        }
        let git = Git::discover()?;
        if let Some(block) = git::operation_blocker(&location.git_dir) {
            return Err(unsupported(&block));
        }
        if git.commit_policy(&location).await? != plan.policy
            || hook_stamps(&plan.policy) != plan.hooks
            || repo_stamp(&location) != plan.repo
            || git.index_records(&location).await? != plan.index
        {
            return Err(changed());
        }
        if location.git_dir.join("index.lock").exists() {
            return Err(git::index_locked());
        }
        // Last preflight immediately before Git; hook guards check index entries again.
        if repo_stamp(&location) != plan.repo
            || git.head_oid(&location, &Cancellation::default()).await? != plan.head
        {
            return Err(changed());
        }
        let action = format!("poseidon:{}:{session_id}:{token}", std::process::id());
        let process_start = Instant::now();
        let result = git
            .create_commit(
                &location,
                &plan.policy,
                &plan.index,
                message,
                hooks_allowed,
                plan.head.as_deref(),
                &action,
            )
            .await;
        let total_git = elapsed_ms(process_start);
        let head = git.head_oid(&location, &Cancellation::default()).await;
        let recorded = match &head {
            Ok(Some(oid)) if Some(oid) != plan.head.as_ref() => {
                git.commit_recorded(&location, oid, &action).await
            }
            _ => Some(false),
        };
        let acknowledged = result.as_ref().is_ok_and(|(r, _, _)| r.success);
        let own_commit = recorded == Some(true) || (recorded.is_none() && acknowledged);
        let mut warning = false;
        let hooks_ms = result.as_ref().map(|(_, ms, _)| *ms).unwrap_or(0);
        let outcome = match &head {
            Ok(Some(oid)) if Some(oid) != plan.head.as_ref() => {
                let expected_parents: Vec<String> = plan.head.iter().cloned().collect();
                match git.committed_records(&location, oid).await {
                    Ok((records, parents))
                        if records == plan.index && parents == expected_parents && own_commit =>
                    {
                        warning = !result.as_ref().is_ok_and(|(r, _, w)| r.success && !w);
                        CommitOutcome::Created
                    }
                    _ => CommitOutcome::Unknown,
                }
            }
            Ok(_)
                if result
                    .as_ref()
                    .is_ok_and(|(r, _, _)| !r.success && r.code.is_some()) =>
            {
                CommitOutcome::Rejected
            }
            _ => CommitOutcome::Unknown,
        };
        let oid = head.ok().flatten();
        // No write is retried. Rebuild refs/history and invalidate caches for all outcomes.
        let invalidation_start = Instant::now();
        *self.diffs.working.lock().map_err(|_| busy())? = None;
        self.diffs.patches.lock().map_err(|_| busy())?.clear();
        self.diffs.lists.lock().map_err(|_| busy())?.clear();
        let invalidation_us = invalidation_start
            .elapsed()
            .as_micros()
            .min(u32::MAX as u128) as u32;
        let refresh_start = Instant::now();
        let snapshot = self.rebuild_snapshot(session_id).await.ok();
        let message = match outcome {
            CommitOutcome::Created if warning => "Commit criado e HEAD verificado, mas houve falha em hook ou na finalização. Confira os efeitos dos hooks; não repita o commit.",
            CommitOutcome::Created => "Commit criado. HEAD, histórico e index atualizados.",
            CommitOutcome::Rejected => "Commit não criado. O Git/hook recusou a operação ou um hook alterou o index. Confira as alterações antes de revisar novamente; o rascunho foi preservado.",
            CommitOutcome::Unknown => "Resultado desconhecido: confira o HEAD e o index antes de qualquer nova tentativa. Não houve retry nem rollback; rascunho preservado.",
        };
        let message = if snapshot.is_none() {
            format!("{message} A atualização do histórico falhou; atualize manualmente.")
        } else {
            message.into()
        };
        Ok(CreateCommitResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: CommitCreation {
                outcome,
                oid,
                message,
                git_ms: total_git.saturating_sub(hooks_ms),
                hooks_ms,
                refresh_ms: elapsed_ms(refresh_start),
                invalidation_us,
                elapsed_ms: elapsed_ms(start),
                snapshot,
            },
        })
    }
}

fn validate_message(message: &str) -> Result<()> {
    if message.len() > 65536
        || message.lines().next().is_none_or(|s| s.trim().is_empty())
        || message
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(AppError::new(ErrorCode::InvalidInput,"Informe um assunto não vazio e uma mensagem de até 64 KiB, sem caracteres de controle."));
    }
    Ok(())
}
