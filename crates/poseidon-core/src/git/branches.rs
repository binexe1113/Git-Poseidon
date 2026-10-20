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
            .process
            .run_configured(
                root,
                &[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=none",
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
                        // Bounded read, including symlink targets; hook edits invalidate plans.
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        if !meta.is_file() {
                            return Err(AppError::new(
                                ErrorCode::UnsupportedState,
                                "Hook executável não é um arquivo regular; inspeção recusada.",
                            ));
                        }
                        let mut options = std::fs::OpenOptions::new();
                        options.read(true);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::OpenOptionsExt;
                            options.custom_flags(libc::O_NONBLOCK);
                        }
                        let file = options.open(path).map_err(|_| local_failure())?;
                        // Validate the descriptor too: a concurrent replacement must not
                        // turn this read-only preview into a blocking FIFO/device read.
                        if !file.metadata().map_err(|_| local_failure())?.is_file() {
                            return Err(AppError::new(
                                ErrorCode::UnsupportedState,
                                "O tipo do hook mudou durante a inspeção.",
                            ));
                        }
                        file.take(1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|_| local_failure())?;
                        if bytes.len() > 1024 * 1024 {
                            return Err(AppError::new(
                                ErrorCode::OutputLimit,
                                "Hook excede o limite de inspeção (1 MiB).",
                            ));
                        }
                        hooks.push(((*name).into(), bytes));
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
        let drivers = self.repo_config(location, &cancel).await?.drivers;
        let mut config = inert_filters(&drivers)?;
        for driver in drivers {
            let mut key = b"filter.".to_vec();
            key.extend_from_slice(&driver);
            key.extend_from_slice(b".smudge");
            config.push((os_path(&key)?, OsString::new()));
        }
        let output = self
            .process
            .run_configured(
                &location.root,
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
    ) -> Result<crate::process::Output> {
        let args: Vec<&str> = match action {
            BranchAction::Create { name, start_oid } => {
                vec!["branch", "--no-track", "--", name, start_oid]
            }
            BranchAction::Switch { name, .. } => vec![
                "switch",
                "--no-guess",
                "--no-overwrite-ignore",
                "--no-recurse-submodules",
                "--",
                name,
            ],
            BranchAction::Rename { name, new_name, .. } => {
                vec!["branch", "-m", "--", name, new_name]
            }
            BranchAction::Delete { name, .. } => vec!["branch", "-d", "--", name],
        };
        let mut pinned: Vec<OsString> = vec![
            "--git-dir".into(),
            location.git_dir.as_os_str().to_owned(),
            "--work-tree".into(),
            location.root.as_os_str().to_owned(),
        ];
        pinned.extend(args.iter().map(OsString::from));
        self.process
            .run_with(
                &location.root,
                &pinned,
                Limits {
                    timeout: Duration::from_secs(120),
                    ..Limits::default()
                },
                &Cancellation::default(),
            )
            .await
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
