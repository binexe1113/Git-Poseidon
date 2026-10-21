use super::*;
use git::branches::BranchState;

pub(super) struct PreparedBranch {
    token: u32,
    session: (u32, u32),
    created: Instant,
    action: BranchAction,
    state: BranchState,
    repo: RepoStamp,
    files: Vec<(PathBuf, Option<FileStamp>)>,
    identity: trust::RepoIdentity,
    block: Option<String>,
}
fn action_name(action: &BranchAction) -> &str {
    match action {
        BranchAction::Create { name, .. }
        | BranchAction::Switch { name, .. }
        | BranchAction::Rename { name, .. }
        | BranchAction::Delete { name, .. } => name,
    }
}
impl RepositoryService {
    pub async fn set_branch_hook_trust(
        &self,
        session_id: u32,
        generation: u32,
        trusted: bool,
    ) -> Result<()> {
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let location = self.session_location(session_id, generation)?;
        self.trust
            .set_branch_hooks(&trust::identity(&location)?, trusted)?;
        *self.branch_plan.lock().map_err(|_| busy())? = None;
        Ok(())
    }
    pub async fn prepare_branch(
        &self,
        session_id: u32,
        generation: u32,
        action: BranchAction,
    ) -> Result<BranchPlanResponse> {
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let location = self.session_location(session_id, generation)?;
        if location.bare {
            return Err(unsupported(
                "Gerenciamento de branches requer working tree nesta fase.",
            ));
        }
        let git = Git::discover()?;
        ensure_location(&git, &location).await?;
        git.branch_name(&location.root, action_name(&action))
            .await?;
        if let BranchAction::Rename { new_name, .. } = &action {
            git.branch_name(&location.root, new_name).await?;
        }
        let identity = trust::identity(&location)?;
        let trusted = self.trust.is_trusted(&identity)?;
        let hooks_trusted = self.trust.branch_hooks_trusted(&identity)?;
        let repo = repo_stamp(&location);
        let state = git.branch_state(&location).await?;
        let branches = parse::branches(&state.refs)?;
        let selected = branches.iter().find(|b| {
            b.full_ref == format!("refs/heads/{}", action_name(&action))
                && b.symbolic_target.is_none()
        });
        match &action {
            BranchAction::Create { start_oid, .. } => {
                if !parse::valid_oid(start_oid) {
                    return Err(AppError::new(
                        ErrorCode::InvalidInput,
                        "Selecione um commit completo para criar a branch.",
                    ));
                }
                git.checked(
                    &location.root,
                    &["cat-file", "-e", &format!("{start_oid}^{{commit}}")],
                )
                .await?;
                if selected.is_some() {
                    return Err(unsupported("Já existe uma branch com esse nome."));
                }
            }
            BranchAction::Switch { expected_oid, .. }
            | BranchAction::Rename { expected_oid, .. }
            | BranchAction::Delete { expected_oid, .. } => {
                if !parse::valid_oid(expected_oid)
                    || selected.is_none_or(|b| b.oid != *expected_oid)
                {
                    return Err(stale());
                }
            }
        }
        let status = parse::status(&state.status)?;
        let config = git.repo_config(&location, &Cancellation::default()).await?;
        let mut block = (git::operation_state(&location.git_dir) != git::NO_OPERATION)
            .then(|| "Há uma operação Git em andamento. Conclua-a no terminal.".to_owned());
        if status.conflicts > 0 {
            block = Some("Index com conflitos: operações de branch bloqueadas.".into());
        }
        if config.sparse
            || status.changes.iter().any(|f| f.submodule)
            || state
                .index
                .split(|b| *b == 0)
                .any(|r| r.starts_with(b"160000 "))
        {
            block = Some(
                "Sparse checkout/index e submódulos não são suportados nestas operações.".into(),
            );
        }
        if state
            .config
            .split(|b| *b == 0)
            .any(|r| r.starts_with(b"hook."))
        {
            block =
                Some("Configuração hook.* ainda não suportada. Nenhum hook será ignorado.".into());
        }
        if status.has_more_changes {
            block = Some(
                "Mais de 500 alterações: revise no terminal antes de gerenciar branches.".into(),
            );
        }
        let head = git.head(&location.root).await?;
        if matches!(head, Head::Unborn { .. }) {
            block = Some("Crie o commit inicial antes de gerenciar branches.".into());
        }
        let full_ref = format!("refs/heads/{}", action_name(&action));
        let mut occupied = false;
        let mut used_elsewhere = false;
        let mut worktree: &[u8] = b"";
        for record in state.worktrees.split(|b| *b == 0) {
            if let Some(path) = record.strip_prefix(b"worktree ") {
                worktree = path;
            }
            if record == format!("branch {full_ref}").as_bytes() {
                occupied = true;
                if worktree != location.root.as_os_str().as_encoded_bytes() {
                    used_elsewhere = true;
                }
            }
        }
        match &action {
            BranchAction::Switch { .. } if selected.is_some_and(|b| b.current) => {
                block = Some("Esta já é a branch atual.".into());
            }
            BranchAction::Switch { .. } | BranchAction::Delete { .. } if occupied => {
                block = Some(
                    "Esta branch está em uso em um worktree. Nenhum worktree será alterado.".into(),
                );
            }
            BranchAction::Rename { .. } if used_elsewhere => {
                block = Some("A branch está em uso por outro worktree.".into());
            }
            _ => {}
        }
        if let BranchAction::Rename { new_name, .. } = &action {
            if branches
                .iter()
                .any(|b| b.full_ref == format!("refs/heads/{new_name}"))
            {
                block = Some("O novo nome já existe.".into());
            }
        }
        if let BranchAction::Delete { expected_oid, .. } = &action {
            let head_oid = match &head {
                Head::Attached { oid, .. } | Head::Detached { oid } => Some(oid),
                _ => None,
            };
            if let Some(oid) = head_oid {
                if !git.ancestor(&location.root, expected_oid, oid).await? {
                    block = Some("Branch divergente: seus commits não estão integrados ao HEAD atual. Exclusão forçada não é permitida.".into());
                }
            }
        }
        // Native dry-run checks tracked modifications without touching index/worktree.
        // Filters are neutralised in this preview; execution uses the authorized native policy.
        if block.is_none() && trusted {
            if let BranchAction::Switch { expected_oid, .. } = &action {
                let check = git.branch_switch_check(&location, expected_oid).await?;
                if !check {
                    block = Some("Arquivos locais impedem a troca. Revise as alterações/untracked; nada será descartado.".into());
                }
            }
        }
        if !trusted {
            block = Some(
                "Conceda confiança para executar esta operação e os filtros Git configurados."
                    .into(),
            );
        }
        if !state.hooks.is_empty() && !hooks_trusted {
            block = Some("Autorize separadamente os hooks de branches antes de executar.".into());
        }
        let files = status_paths(&state.status)
            .into_iter()
            .map(|path| {
                let path = location.root.join(git::os_path(&path)?);
                Ok((path.clone(), file_stamp(&path)))
            })
            .collect::<Result<Vec<_>>>()?;
        if repo != repo_stamp(&location) || state != git.branch_state(&location).await? {
            return Err(changed());
        }
        let token = self.next_plan.fetch_add(1, Ordering::Relaxed);
        let data = BranchPlan {
            token,
            action: action.clone(),
            hooks: state.hooks.iter().map(|h| h.0.clone()).collect(),
            filters: config.drivers.iter().map(|v| parse::text(v)).collect(),
            trusted,
            hooks_trusted,
            dirty_paths: status.changes.iter().map(|f| f.path.clone()).collect(),
            block: block.clone(),
        };
        *self.branch_plan.lock().map_err(|_| busy())? = Some(PreparedBranch {
            token,
            session: (session_id, generation),
            created: Instant::now(),
            action,
            state,
            repo,
            files,
            identity,
            block,
        });
        Ok(BranchPlanResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data,
        })
    }
    pub async fn execute_branch(
        &self,
        session_id: u32,
        generation: u32,
        token: u32,
    ) -> Result<BranchChangeResponse> {
        let start = Instant::now();
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let location = self.session_location(session_id, generation)?;
        let plan = self
            .branch_plan
            .lock()
            .map_err(|_| busy())?
            .take()
            .filter(|p| {
                p.token == token
                    && p.session == (session_id, generation)
                    && p.created.elapsed() < Duration::from_secs(120)
            })
            .ok_or_else(stale)?;
        if let Some(reason) = plan.block {
            return Err(unsupported(&reason));
        }
        let git = Git::discover()?;
        if trust::identity(&location)? != plan.identity
            || !self.trust.is_trusted(&plan.identity)?
            || (!plan.state.hooks.is_empty() && !self.trust.branch_hooks_trusted(&plan.identity)?)
        {
            return Err(AppError::new(
                ErrorCode::TrustRequired,
                "Confiança revogada. Revise novamente a operação.",
            ));
        }
        if plan.repo != repo_stamp(&location)
            || plan.files.iter().any(|(p, s)| file_stamp(p) != *s)
            || plan.state != git.branch_state(&location).await?
        {
            return Err(changed());
        }
        if git::operation_state(&location.git_dir) != git::NO_OPERATION {
            return Err(unsupported("Operação Git em andamento."));
        }
        if location.git_dir.join("index.lock").exists() {
            return Err(git::index_locked());
        }
        for slot in [&self.diffs.current, &self.diffs.listing] {
            if let Some(token) = slot.lock().map_err(|_| busy())?.take() {
                token.cancel();
            }
        }
        ensure_location(&git, &location).await?;
        let git_start = Instant::now();
        let result = git.branch_write(&location, &plan.action).await;
        let git_ms = elapsed_ms(git_start);
        let after = git.branch_state(&location).await;
        let acknowledged = result.as_ref().is_ok_and(|o| o.success);
        let expected = after
            .as_ref()
            .is_ok_and(|s| branch_applied(&plan.action, &plan.state, s));
        let unchanged = after.as_ref().is_ok_and(|s| *s == plan.state)
            && plan.files.iter().all(|(p, s)| file_stamp(p) == *s);
        let outcome = if acknowledged && expected {
            BranchOutcome::Applied
        } else if !acknowledged && unchanged && result.as_ref().is_ok_and(|o| o.code.is_some()) {
            BranchOutcome::Rejected
        } else {
            BranchOutcome::Unknown
        };
        let refresh = Instant::now();
        // A refusal with verified identical state leaves existing graph/pages intact.
        let snapshot = if unchanged {
            None
        } else {
            *self.diffs.working.lock().map_err(|_| busy())? = None;
            self.diffs.patches.lock().map_err(|_| busy())?.clear();
            self.diffs.lists.lock().map_err(|_| busy())?.clear();
            *self.commit_plan.lock().map_err(|_| busy())? = None;
            self.rebuild_snapshot(session_id).await.ok()
        };
        let mut message = match outcome {
            BranchOutcome::Applied => "Operação concluída; refs e HEAD finais conferidos pelo Git.",
            BranchOutcome::Rejected => "Git recusou a operação; refs, HEAD e index conferidos sem alteração. Arquivos locais, upstream ou locks podem impedir a ação.",
            BranchOutcome::Unknown => "Resultado não confirmado ou hook/filtro falhou após efeitos parciais. Confira HEAD, branches e arquivos; não repita automaticamente. Não houve rollback.",
        }.to_owned();
        if !unchanged && snapshot.is_none() {
            message.push_str(
                " Atualização da interface falhou; atualize manualmente antes de continuar.",
            );
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
fn branch_applied(action: &BranchAction, before: &BranchState, state: &BranchState) -> bool {
    let Ok(branches) = parse::branches(&state.refs) else {
        return false;
    };
    let find = |name: &str, oid: &str| {
        branches
            .iter()
            .any(|b| b.full_ref == format!("refs/heads/{name}") && b.oid == oid)
    };
    match action {
        BranchAction::Create { name, start_oid } => {
            state.head == before.head && find(name, start_oid)
        }
        BranchAction::Switch { name, expected_oid } => {
            state.head == format!("attached:{name}:{expected_oid}")
        }
        BranchAction::Rename {
            name,
            expected_oid,
            new_name,
        } => {
            let expected_head = if before.head == format!("attached:{name}:{expected_oid}") {
                format!("attached:{new_name}:{expected_oid}")
            } else {
                before.head.clone()
            };
            state.head == expected_head
                && !branches
                    .iter()
                    .any(|b| b.full_ref == format!("refs/heads/{name}"))
                && find(new_name, expected_oid)
        }
        BranchAction::Delete { name, .. } => {
            state.head == before.head
                && !branches
                    .iter()
                    .any(|b| b.full_ref == format!("refs/heads/{name}"))
        }
    }
}

fn status_paths(status: &[u8]) -> Vec<Vec<u8>> {
    let mut paths = Vec::new();
    let mut records = status.split(|b| *b == 0);
    while let Some(record) = records.next() {
        let path = match record.first() {
            Some(b'1') => record.splitn(9, |b| *b == b' ').nth(8),
            Some(b'2') => {
                let path = record.splitn(10, |b| *b == b' ').nth(9);
                if let Some(old) = records.next() {
                    paths.push(old.to_vec());
                }
                path
            }
            Some(b'?') => record.get(2..),
            _ => None,
        };
        if let Some(path) = path {
            paths.push(path.to_vec());
        }
    }
    paths
}

pub(super) async fn ensure_location(git: &Git, location: &RepositoryLocation) -> Result<()> {
    let actual = git.locate(&location.root).await?;
    if actual.root != location.root
        || actual.git_dir != location.git_dir
        || actual.common_dir != location.common_dir
        || actual.object_format != location.object_format
    {
        return Err(stale());
    }
    Ok(())
}
