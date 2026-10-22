//! Native local operations only. No force, stash, merge, guessing or remote transport.
use super::*;

pub(crate) const HOOKS: &[&str] = &[
    "post-checkout",
    "post-index-change",
    "reference-transaction",
];

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BranchState {
    pub refs: Vec<u8>,
    pub head: String,
    pub worktrees: Vec<u8>,
    pub config: Vec<u8>,
    pub index: Vec<u8>,
    pub status: Vec<u8>,
    pub hooks: Vec<(String, Vec<u8>)>,
}
impl Git {
    pub(crate) async fn branch_state(&self, location: &RepositoryLocation) -> Result<BranchState> {
        let root = &location.root;
        let config = self
            .read_config(location, false, &Cancellation::default())
            .await?;
        let status = self
            .inspect(
                location,
                &[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=dirty",
                ],
                &config,
                Limits::default(),
                &Cancellation::default(),
            )
            .await?;
        if !status.success {
            return Err(local_failure());
        }
        let hook_dir = path_from_output(
            &self
                .checked(
                    root,
                    &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
                )
                .await?,
        )?;
        let mut hooks = Vec::new();
        for name in HOOKS {
            let path = hook_dir.join(name);
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    #[cfg(unix)]
                    let executable = {
                        use std::os::unix::fs::PermissionsExt;
                        meta.permissions().mode() & 0o111 != 0
                    };
                    #[cfg(not(unix))]
                    let executable = true;
                    if executable {
                        hooks.push(((*name).into(), execution::read_hook(&path)?));
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(_) => return Err(local_failure()),
            }
        }
        Ok(BranchState {
            refs: self.branch_output(root).await?,
            head: head_key(&self.head(root).await?),
            worktrees: self
                .checked(root, &["worktree", "list", "--porcelain", "-z"])
                .await?,
            config: self
                .checked(root, &["config", "--null", "--list", "--includes"])
                .await?,
            index: self.index_records(location).await?,
            status: status.stdout,
            hooks,
        })
    }
    pub(crate) async fn branch_name(&self, root: &Path, name: &str) -> Result<()> {
        // Reject revisions, options, HEAD and lossy display names before Git validation.
        if name.is_empty()
            || name.len() > 1024
            || name.starts_with('-')
            || name == "HEAD"
            || name == "@"
            || name.contains(['\0', '\n', '\r'])
        {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Nome de branch inválido.",
            ));
        }
        let output = self
            .process
            .run(root, &["check-ref-format", "--branch", name])
            .await?;
        if !output.success || trim_lf(&output.stdout) != name.as_bytes() {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Nome de branch inválido. Use um nome de referência literal.",
            ));
        }
        Ok(())
    }
    pub(crate) async fn branch_switch_check(
        &self,
        location: &RepositoryLocation,
        target: &str,
    ) -> Result<bool> {
        let others = self
            .checked(&location.root, &["ls-files", "--others", "-z"])
            .await?;
        let entries = self
            .checked(&location.root, &["ls-tree", "-r", "-z", target])
            .await?;
        if entries
            .split(|b| *b == 0)
            .any(|r| r.starts_with(b"160000 "))
        {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "A branch de destino contém submódulos; troca bloqueada nesta fase.",
            ));
        }
        let tree: Vec<u8> = entries
            .split(|b| *b == 0)
            .filter_map(|r| r.iter().position(|b| *b == b'\t').map(|i| &r[i + 1..]))
            .flat_map(|p| p.iter().copied().chain(std::iter::once(0)))
            .collect();
        // Include ignored files: switch defaults to overwriting those unless explicitly disabled.
        // Sort once: ignored/untracked trees can be large. Avoid O(untracked * target).
        let mut targets: Vec<&[u8]> = tree.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
        targets.sort_unstable();
        for path in others.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let ancestor = path
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'/')
                .any(|(i, _)| targets.binary_search(&&path[..i]).is_ok());
            let mut prefix = path.to_vec();
            prefix.push(b'/');
            let next = targets.partition_point(|target| *target < prefix.as_slice());
            if targets.binary_search(&path).is_ok()
                || ancestor
                || targets
                    .get(next)
                    .is_some_and(|target| target.starts_with(&prefix))
            {
                return Ok(false);
            }
        }
        let cancel = Cancellation::default();
        let snapshot = self.execution_config(location, &cancel).await?;
        let mut paths = targets.iter().map(|p| p.to_vec()).collect::<Vec<_>>();
        paths.extend(
            self.checked(&location.root, &["ls-files", "-z"])
                .await?
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| p.to_vec()),
        );
        self.reject_unsupported_filters(location, &snapshot, &paths, None)
            .await?;
        self.reject_unsupported_filters(location, &snapshot, &paths, Some(target))
            .await?;
        let drivers = self.repo_config(location, &cancel).await?.drivers;
        let mut config = inert_filters(&drivers)?;
        for driver in drivers {
            let mut key = b"filter.".to_vec();
            key.extend_from_slice(&driver);
            key.extend_from_slice(b".smudge");
            config.push((os_path(&key)?, OsString::new()));
        }
        let output = self
            .inspect(
                location,
                &["read-tree", "--dry-run", "-m", "-u", "HEAD", target],
                &config,
                Limits::default(),
                &cancel,
            )
            .await?;
        Ok(output.success)
    }
    pub(crate) async fn branch_write(
        &self,
        location: &RepositoryLocation,
        action: &BranchAction,
        state: &BranchState,
    ) -> Result<crate::process::Output> {
        self.ensure_write_format(location, matches!(action, BranchAction::Switch { .. }))
            .await?;
        let config = execution::config_records(&state.config)?;
        if config
            .iter()
            .any(|(k, v)| k == "extensions.refstorage" && v == "reftable")
        {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "Escritas de branches com reftable ainda não têm guard compartilhado validado.",
            ));
        }
        let mut context = self.execution_context(
            location,
            &config,
            execution::GitExecutionPolicy::BranchWrite,
        )?;
        context.hooks(&state.hooks)?;
        let limits = Limits {
            timeout: Duration::from_secs(120),
            ..Limits::default()
        };
        let cancel = Cancellation::default();
        if let BranchAction::Create { name, start_oid } = action {
            return self
                .process
                .run_input_env(
                    &location.root,
                    &["branch", "--no-track", "--", name, start_oid],
                    &context.config,
                    None,
                    limits,
                    &cancel,
                    &context.env,
                )
                .await;
        }
        let mut guard_state = execution::HeadGuard::acquire(location)?;
        let _other_heads = execution::OtherHeadGuards::acquire(location)?;
        if self
            .checked(&location.root, &["worktree", "list", "--porcelain", "-z"])
            .await?
            != state.worktrees
        {
            return Err(AppError::new(
                ErrorCode::WorkingTreeChanged,
                "O registro de worktrees mudou desde a revisão.",
            ));
        }
        if head_key(&self.head(&location.root).await?) != state.head {
            return Err(AppError::new(
                ErrorCode::WorkingTreeChanged,
                "HEAD mudou desde a revisão.",
            ));
        }
        if let BranchAction::Switch { name, expected_oid } = action {
            let reference = format!("refs/heads/{name}");
            let path = execution::ref_path(location, reference.as_bytes())?;
            let _target = execution::SharedLock::acquire(execution::lock_path(&path))?;
            let mut index = execution::IndexTransaction::acquire(location)?;
            if self
                .checked(
                    &location.root,
                    &["show-ref", "--hash", "--verify", &reference],
                )
                .await?
                .trim_ascii_end()
                != expected_oid.as_bytes()
                || self.index_records(location).await? != state.index
            {
                return Err(AppError::new(
                    ErrorCode::WorkingTreeChanged,
                    "Branch/index mudou desde a revisão.",
                ));
            }
            context.index(&index.index);
            let result = self
                .process
                .run_input_env(
                    &location.root,
                    &[
                        "switch",
                        "--no-guess",
                        "--no-overwrite-ignore",
                        "--no-recurse-submodules",
                        "--",
                        name,
                    ],
                    &context.config,
                    None,
                    limits,
                    &cancel,
                    &context.env,
                )
                .await;
            index.publish()?;
            let head = context.head()?;
            if head == format!("ref: {reference}\n").as_bytes() {
                guard_state
                    .head_lock
                    .publish(&location.git_dir.join("HEAD"), &head)?;
            } else if head
                != std::fs::read(location.git_dir.join("HEAD")).map_err(|_| local_failure())?
            {
                return Err(outcome_unknown());
            }
            return result;
        }
        // Native branch -m/-d accept a mutable name, not the reviewed OID.
        // Use Git's expected-old transactions for both refs of a rename/delete.
        let (name, expected, new_name) = match action {
            BranchAction::Rename {
                name,
                expected_oid,
                new_name,
            } => (name, expected_oid, Some(new_name)),
            BranchAction::Delete { name, expected_oid } => (name, expected_oid, None),
            _ => return Err(AppError::protocol()),
        };
        let mut config_lock =
            execution::SharedLock::acquire(location.common_dir.join("config.lock"))?;
        let original_config = execution::optional_log(&location.common_dir.join("config"))?
            .ok_or_else(local_failure)?;
        if self.execution_config(location, &cancel).await? != config {
            return Err(AppError::new(
                ErrorCode::WorkingTreeChanged,
                "Configuração mudou desde a revisão.",
            ));
        }
        let local_config = context.local_config(&original_config)?;
        let section = format!("branch.{name}");
        let mut config_args = vec![
            OsString::from("config"),
            "--file".into(),
            local_config.as_os_str().to_owned(),
        ];
        if let Some(new_name) = new_name {
            config_args.extend([
                "--rename-section".into(),
                section.into(),
                format!("branch.{new_name}").into(),
            ]);
        } else {
            config_args.extend(["--remove-section".into(), section.into()]);
        }
        let changed = self
            .process
            .run_with(&location.root, &config_args, limits, &cancel)
            .await?;
        if !changed.success
            && changed.code != Some(5)
            && !(changed.code == Some(128)
                && changed.stderr.starts_with(b"fatal: no such section:"))
        {
            return Err(local_failure());
        }
        let source = format!("refs/heads/{name}");
        if guard_state.reference.as_deref() == Some(source.as_bytes()) {
            guard_state.ref_lock.take();
        }
        context.guard_reference(
            &self.process.executable,
            &source,
            location,
            &state.worktrees,
        )?;
        let old_log = execution::optional_log(&location.common_dir.join("logs").join(&source))?;
        context.guard_reflog(
            location,
            &source,
            new_name.map(String::as_str),
            old_log.as_deref(),
        )?;
        let mut input = String::from("start\n");
        // HeadGuard already holds the reviewed current branch lock. Ref
        // transactions below never mutate it except a current-branch rename.

        if matches!(action, BranchAction::Delete { .. }) {
            let branches = parse::branches(&state.refs)?;
            if let Some(upstream) = branches
                .iter()
                .find(|b| b.full_ref == source)
                .and_then(|b| b.upstream.as_ref())
            {
                if upstream != &source
                    && guard_state.reference.as_deref() != Some(upstream.as_bytes())
                {
                    let branch = branches
                        .iter()
                        .find(|b| b.full_ref == *upstream)
                        .ok_or_else(local_failure)?;
                    input.push_str(&format!("verify {} {}\n", branch.full_ref, branch.oid));
                }
            }
        }
        if let Some(new_name) = new_name {
            input.push_str(&format!("create refs/heads/{new_name} {expected}\n"));
        }
        input.push_str(&format!("delete {source} {expected}\nprepare\ncommit\n"));
        let result = self
            .process
            .run_input_env(
                &location.root,
                &[
                    "update-ref",
                    "--no-deref",
                    "-m",
                    "branch: Poseidon reviewed local branch operation",
                    "--stdin",
                ],
                &context.config,
                Some(input.as_bytes()),
                limits,
                &cancel,
                &context.env,
            )
            .await;
        let acknowledged = result.as_ref().is_ok_and(|output| output.success);
        let observed = if !acknowledged {
            let source_present = self
                .process
                .run(
                    &location.root,
                    &["show-ref", "--verify", "--quiet", &source],
                )
                .await?;
            let destination_ok = if let Some(new_name) = new_name {
                let output = self
                    .process
                    .run(
                        &location.root,
                        &[
                            "show-ref",
                            "--hash",
                            "--verify",
                            &format!("refs/heads/{new_name}"),
                        ],
                    )
                    .await?;
                output.success && output.stdout.trim_ascii_end() == expected.as_bytes()
            } else {
                true
            };
            source_present.code == Some(1) && destination_ok
        } else {
            false
        };
        if acknowledged || observed {
            // Ref CAS succeeded. Metadata publication may still fail: the caller
            // then reports Unknown, never retries or rolls back these refs.
            if let Some(new_name) = new_name {
                if state.head == format!("attached:{name}:{expected}") {
                    guard_state.head_lock.publish(
                        &location.git_dir.join("HEAD"),
                        format!("ref: refs/heads/{new_name}\n").as_bytes(),
                    )?;
                }
            }
            config_lock.publish(
                &location.common_dir.join("config"),
                &std::fs::read(local_config).map_err(|_| local_failure())?,
            )?;
        }
        result
    }
    pub(crate) async fn ancestor(&self, root: &Path, oid: &str, head: &str) -> Result<bool> {
        let output = self
            .process
            .run(root, &["merge-base", "--is-ancestor", oid, head])
            .await?;
        match output.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(local_failure()),
        }
    }
}
