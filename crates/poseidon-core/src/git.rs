pub(crate) mod branches;
pub(crate) mod commit;
use crate::{
    diff,
    domain::*,
    parse,
    process::{Cancellation, GitProcess, Limits},
};
pub(crate) use commit::CommitPolicy;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const COMMIT_FORMAT: &str =
    "--format=%H%x00%T%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s";

#[derive(Clone)]
pub struct Git {
    process: GitProcess,
}

#[derive(Clone)]
pub(crate) struct RepositoryLocation {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub bare: bool,
    pub shallow: bool,
    pub object_format: String,
}

impl Git {
    pub fn discover() -> Result<Self> {
        Ok(Self {
            process: GitProcess::discover()?,
        })
    }

    /// Explicit executable selection for harnesses/embedding; never accepted by IPC.
    pub fn with_executable(path: PathBuf) -> Result<Self> {
        let executable = path.canonicalize().map_err(|_| {
            AppError::new(ErrorCode::GitUnavailable, "Executável Git não encontrado.")
        })?;
        if !executable.is_file() {
            return Err(AppError::new(
                ErrorCode::GitUnavailable,
                "Selecione um arquivo executável Git.",
            ));
        }
        #[cfg(windows)]
        if executable.extension().and_then(|s| s.to_str()) != Some("exe") {
            return Err(AppError::new(
                ErrorCode::GitUnavailable,
                "Selecione um executável Git .exe.",
            ));
        }
        Ok(Self {
            process: GitProcess { executable },
        })
    }

    pub async fn installation(&self) -> Result<GitInstallation> {
        let output = self.checked(&std::env::temp_dir(), &["--version"]).await?;
        let version = parse::text(trim_lf(&output));
        let numbers: Vec<u32> = version
            .trim_start_matches("git version ")
            .split('.')
            .take(2)
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        if numbers.len() < 2 || (numbers[0], numbers[1]) < (2, 49) {
            return Err(AppError::new(
                ErrorCode::UnsupportedGit,
                "Esta fase requer Git 2.49 ou mais recente para inspeção sem lazy fetch.",
            ));
        }
        Ok(GitInstallation {
            version,
            executable: self.process.executable.to_string_lossy().into_owned(),
        })
    }

    pub(crate) async fn checked(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.process.run(cwd, args).await?;
        if !output.success {
            // stderr may contain credentials or hostile terminal escapes; do not expose it.
            return Err(AppError::new(ErrorCode::CommandFailed, "O Git não conseguiu concluir a leitura. Verifique permissões, integridade e configuração safe.directory no terminal."));
        }
        Ok(output.stdout)
    }

    pub(crate) async fn locate(&self, path: &Path) -> Result<RepositoryLocation> {
        let selected = path.canonicalize().map_err(|_| {
            AppError::new(
                ErrorCode::InvalidRepository,
                "A pasta selecionada não existe ou não está acessível.",
            )
        })?;
        if !selected.is_dir() {
            return Err(AppError::new(
                ErrorCode::InvalidRepository,
                "Selecione uma pasta de repositório Git.",
            ));
        }
        let check = self
            .process
            .run(&selected, &["rev-parse", "--is-bare-repository"])
            .await?;
        if !check.success {
            return Err(AppError::new(ErrorCode::InvalidRepository, "A pasta não é um repositório Git acessível. Confira também permissões e safe.directory."));
        }
        let bare = trim_lf(&check.stdout) == b"true";
        let root = if bare {
            selected.clone()
        } else {
            path_from_output(
                &self
                    .checked(
                        &selected,
                        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
                    )
                    .await?,
            )?
        };
        let git_dir = path_from_output(
            &self
                .checked(&selected, &["rev-parse", "--absolute-git-dir"])
                .await?,
        )?;
        let common_dir = path_from_output(
            &self
                .checked(
                    &selected,
                    &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                )
                .await?,
        )?;
        let shallow = trim_lf(
            &self
                .checked(&root, &["rev-parse", "--is-shallow-repository"])
                .await?,
        ) == b"true";
        let object_format = parse::text(trim_lf(
            &self
                .checked(&root, &["rev-parse", "--show-object-format"])
                .await?,
        ));
        Ok(RepositoryLocation {
            root,
            git_dir,
            common_dir,
            bare,
            shallow,
            object_format,
        })
    }

    pub(crate) async fn head(&self, root: &Path) -> Result<Head> {
        let branch = self
            .process
            .run(root, &["symbolic-ref", "--quiet", "HEAD"])
            .await?;
        let target = self
            .process
            .run(root, &["rev-parse", "--verify", "--quiet", "HEAD"])
            .await?;
        let name = parse::text(trim_lf(&branch.stdout))
            .trim_start_matches("refs/heads/")
            .to_owned();
        match (branch.success, target.success) {
            (true, true) => Ok(Head::Attached {
                name,
                oid: oid_output(&target.stdout)?,
            }),
            (false, true) if branch.code == Some(1) => Ok(Head::Detached {
                oid: oid_output(&target.stdout)?,
            }),
            (true, false) if target.code == Some(1) => Ok(Head::Unborn { name }),
            _ => Err(AppError::new(
                ErrorCode::InvalidRepository,
                "Não foi possível determinar o HEAD do repositório.",
            )),
        }
    }

    async fn branch_output(&self, root: &Path) -> Result<Vec<u8>> {
        self.checked(
            root,
            &[
                "for-each-ref",
                "--sort=refname",
                "--count=5001",
                "--format=%(refname)%00%(objectname)%00%(upstream)%00%(HEAD)%00%(symref)%00",
                "refs/heads/",
                "refs/remotes/",
            ],
        )
        .await
    }

    pub(crate) async fn snapshot(
        &self,
        location: &RepositoryLocation,
        trusted: bool,
    ) -> Result<RepositorySnapshot> {
        let start = Instant::now();
        let head = self.head(&location.root).await?;
        let (branch_output, status) = tokio::try_join!(
            self.branch_output(&location.root),
            self.working_status(location, trusted),
        )?;
        let mut branches = parse::branches(&branch_output)?;
        let new_head = self.head(&location.root).await?;
        if head_key(&head) != head_key(&new_head)
            || branch_output != self.branch_output(&location.root).await?
        {
            return Err(AppError::new(
                ErrorCode::StaleSession,
                "As referências mudaram durante a leitura. Atualize para obter uma nova leitura.",
            ));
        }

        let has_more_branches = branches.len() > BRANCH_LIMIT;
        branches.truncate(BRANCH_LIMIT);
        let operation = operation_state(&location.git_dir);
        Ok(RepositorySnapshot {
            name: location
                .root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Repositório".into()),
            path: location.root.to_string_lossy().into_owned(),
            git_dir: location.git_dir.to_string_lossy().into_owned(),
            common_dir: location.common_dir.to_string_lossy().into_owned(),
            bare: location.bare,
            shallow: location.shallow,
            object_format: location.object_format.clone(),
            head,
            operation,
            status,
            commits: Vec::new(),
            graph: Vec::new(),
            branches,
            has_more_commits: false,
            has_more_branches,
            read_elapsed_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
        })
    }

    pub(crate) fn stream(
        &self,
        root: &Path,
        roots: &[String],
    ) -> Result<crate::process::GitStream> {
        let input = roots.join("\n") + "\n";
        self.process.history_stream(
            root,
            &[
                "log",
                "--topo-order",
                "--no-show-signature",
                "--no-decorate",
                "-z",
                COMMIT_FORMAT,
                "--stdin",
                "--",
            ],
            input.into_bytes(),
        )
    }

    async fn reference_output(&self, root: &Path) -> Result<Vec<u8>> {
        self.checked(root, &["for-each-ref", "--sort=refname", "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)%00%(symref)%00"]).await
    }

    pub(crate) async fn fingerprint(&self, location: &RepositoryLocation) -> Result<Vec<u8>> {
        let head = self.head(&location.root).await?;
        Ok(seal_fingerprint(
            self.reference_output(&location.root).await?,
            &head,
            location,
        ))
    }

    pub(crate) async fn ref_state(
        &self,
        location: &RepositoryLocation,
    ) -> Result<(
        Vec<u8>,
        Vec<String>,
        std::collections::BTreeMap<String, Vec<CommitRef>>,
    )> {
        use std::collections::{BTreeMap, BTreeSet};
        let head = self.head(&location.root).await?;
        let fingerprint = self.reference_output(&location.root).await?;
        let mut labels: BTreeMap<String, Vec<CommitRef>> = BTreeMap::new();
        let mut roots = BTreeSet::new();
        for record in fingerprint.split(|b| *b == b'\n').filter(|r| !r.is_empty()) {
            let f: Vec<_> = record.split(|b| *b == 0).collect();
            if f.len() != 7 {
                return Err(AppError::protocol());
            }
            let name = parse::text(f[0]);
            let oid = if f[2] == b"commit" {
                Some(parse::text(f[1]))
            } else if f[4] == b"commit" {
                Some(parse::text(f[3]))
            } else if f[2] == b"tag" && f[4] == b"tag" {
                // Nested annotated tags are rare; peel with Git rather than guessing.
                let value = self
                    .process
                    .run(
                        &location.root,
                        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
                    )
                    .await?;
                if value.success {
                    Some(oid_output(&value.stdout)?)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(oid) = oid {
                if !parse::valid_oid(&oid) {
                    return Err(AppError::protocol());
                }
                roots.insert(oid.clone());
                if !f[5].is_empty() {
                    continue;
                }
                let (short, kind) = if let Some(n) = name.strip_prefix("refs/heads/") {
                    (n, "branch")
                } else if let Some(n) = name.strip_prefix("refs/remotes/") {
                    (n, "remote")
                } else if let Some(n) = name.strip_prefix("refs/tags/") {
                    (n, "tag")
                } else {
                    (name.as_str(), "ref")
                };
                let current = matches!(&head, Head::Attached { name: n, .. } if name == format!("refs/heads/{n}"));
                labels.entry(oid).or_default().push(CommitRef {
                    name: short.to_owned(),
                    kind: kind.into(),
                    current,
                });
            }
        }
        let fingerprint = seal_fingerprint(fingerprint, &head, location);
        let mut ordered = Vec::with_capacity(roots.len() + 1);
        if let Head::Attached { oid, .. } | Head::Detached { oid } = &head {
            roots.remove(oid);
            ordered.push(oid.clone());
            labels.entry(oid.clone()).or_default().insert(
                0,
                CommitRef {
                    name: "HEAD".into(),
                    kind: "head".into(),
                    current: true,
                },
            );
        }
        for refs in labels.values_mut() {
            refs.sort_by_key(|r| !r.current);
        }
        ordered.extend(roots);
        Ok((fingerprint, ordered, labels))
    }

    async fn working_status(
        &self,
        location: &RepositoryLocation,
        trusted: bool,
    ) -> Result<WorkingStatus> {
        if location.bare {
            return Ok(WorkingStatus {
                available: false,
                total_files: 0,
                has_more_changes: false,
                staged: 0,
                modified: 0,
                untracked: 0,
                conflicts: 0,
                changes: Vec::new(),
            });
        }
        let cancel = Cancellation::default();
        let config = self.read_config(location, trusted, &cancel).await?;
        let output = self
            .process
            .run_configured(
                &location.root,
                &[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=dirty",
                ],
                &config,
                Limits::default(),
                &cancel,
            )
            .await?;
        if !output.success {
            return Err(AppError::new(ErrorCode::CommandFailed, "O Git não conseguiu concluir a leitura. Verifique permissões, integridade e configuração safe.directory no terminal."));
        }
        parse::status(&output.stdout)
    }

    /// Filter drivers and sparse checkout from the effective configuration, in one
    /// `git config` read. Nothing configured is run to find them.
    pub(crate) async fn repo_config(
        &self,
        location: &RepositoryLocation,
        cancel: &Cancellation,
    ) -> Result<RepoConfig> {
        let output = self
            .process
            .run_with(
                &location.root,
                &[
                    "config",
                    "-z",
                    "--get-regexp",
                    r"^(filter\..*|core\.sparsecheckout|index\.sparse)$",
                ],
                Limits::default(),
                cancel,
            )
            .await?;
        // Exit code 1: nothing matched.
        if !output.success && output.code != Some(1) {
            return Err(AppError::new(
                ErrorCode::CommandFailed,
                "O Git não conseguiu ler a configuração do repositório.",
            ));
        }
        let mut config = RepoConfig::default();
        for record in output.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            // "<key>\n<value>"; a key without value has no newline (boolean true).
            let (key, value) = match record.iter().position(|b| *b == b'\n') {
                Some(i) => (&record[..i], Some(&record[i + 1..])),
                None => (record, None),
            };
            if let Some(rest) = key.strip_prefix(b"filter.") {
                if let Some(dot) = rest.iter().rposition(|b| *b == b'.') {
                    config.drivers.push(rest[..dot].to_vec());
                }
            } else if value.is_none_or(|v| {
                matches!(
                    v.to_ascii_lowercase().as_slice(),
                    b"true" | b"yes" | b"on" | b"1"
                )
            }) {
                config.sparse = true;
            }
        }
        // Index writes may invoke post-index-change. T09 does not support arbitrary
        // hooks: reject them, including core.hooksPath, rather than bypassing them.
        let hook = self
            .checked(
                &location.root,
                &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
            )
            .await?;
        let hook = path_from_output(&hook)?.join("post-index-change");
        config.index_hook = match std::fs::metadata(hook) {
            Ok(meta) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode() & 0o111 != 0
                }
                #[cfg(not(unix))]
                {
                    let _ = meta;
                    true
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                false
            }
            Err(_) => return Err(local_failure()),
        };
        config.drivers.sort_unstable();
        config.drivers.dedup();
        Ok(config)
    }

    /// Configuration for reading the worktree. Untrusted: every configured clean/process
    /// filter becomes a no-op, because `status`/`diff-files` would otherwise run them and
    /// inspection must not execute repository-configured programs (docs/09); files are
    /// compared as raw bytes. Trusted: Git's own configuration, as `git status` sees it.
    pub(crate) async fn read_config(
        &self,
        location: &RepositoryLocation,
        trusted: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<(OsString, OsString)>> {
        if trusted {
            return Ok(Vec::new());
        }
        inert_filters(&self.repo_config(location, cancel).await?.drivers)
    }

    pub(crate) async fn detail(
        &self,
        location: &RepositoryLocation,
        commit: Commit,
    ) -> Result<CommitDetail> {
        if !parse::valid_oid(&commit.oid) {
            return Err(AppError::new(ErrorCode::InvalidInput, "OID inválido."));
        }
        let bytes = self
            .checked(
                &location.root,
                &[
                    "show",
                    "--no-patch",
                    "--no-show-signature",
                    "--format=%B",
                    &commit.oid,
                    "--",
                ],
            )
            .await?;
        Ok(CommitDetail {
            commit,
            message: parse::text(&bytes).trim_end_matches('\n').to_owned(),
        })
    }
}

/// Options shared by every diff query: no external programs, no colour, explicit rename
/// detection and algorithm so user configuration cannot change the parsed format.
const DIFF_OPTIONS: [&str; 9] = [
    "-r",
    "--no-commit-id",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--find-renames",
    "--diff-algorithm=myers",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

impl Git {
    /// First parent of a commit, or `None` for a root/shallow-boundary commit.
    pub(crate) async fn diff_base(
        &self,
        location: &RepositoryLocation,
        oid: &str,
        cancel: &Cancellation,
    ) -> Result<DiffBase> {
        let output = self
            .bounded(
                &location.root,
                &[
                    "log",
                    "--no-walk",
                    "--max-count=1",
                    "--no-show-signature",
                    "--format=%P",
                    &format!("{oid}^{{commit}}"),
                    "--",
                ],
                Limits::default(),
                cancel,
            )
            .await?;
        let parents: Vec<String> = parse::text(&output)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if parents.iter().any(|p| !parse::valid_oid(p)) {
            return Err(AppError::protocol());
        }
        Ok(match parents.first() {
            None => DiffBase::EmptyTree,
            Some(first) => DiffBase::FirstParent {
                oid: first.clone(),
                parent_count: parents.len() as u32,
            },
        })
    }

    pub(crate) async fn changed_files(
        &self,
        location: &RepositoryLocation,
        oid: &str,
        base: &DiffBase,
        cancel: &Cancellation,
    ) -> Result<diff::ParsedChanges> {
        let mut args: Vec<OsString> = vec!["diff-tree".into(), "-z".into()];
        args.extend(DIFF_OPTIONS.iter().map(OsString::from));
        args.extend(["--raw".into(), "--numstat".into()]);
        push_range(&mut args, oid, base);
        let output = self
            .bounded(
                &location.root,
                &args,
                Limits {
                    stdout_bytes: 16 * 1024 * 1024,
                    ..Limits::default()
                },
                cancel,
            )
            .await?;
        diff::changes(&output, DIFF_FILE_LIMIT)
    }

    pub(crate) async fn blob_size(
        &self,
        location: &RepositoryLocation,
        oid: &str,
        cancel: &Cancellation,
    ) -> Result<u64> {
        let output = self
            .bounded(
                &location.root,
                &["cat-file", "-s", oid],
                Limits::default(),
                cancel,
            )
            .await?;
        parse::text(trim_lf(&output))
            .parse()
            .map_err(|_| AppError::protocol())
    }

    /// Patch of one listed change, selected by the literal pathspec of both sides.
    pub(crate) async fn file_patch(
        &self,
        location: &RepositoryLocation,
        oid: &str,
        base: &DiffBase,
        change: &diff::RawChange,
        line_limit: usize,
        cancel: &Cancellation,
    ) -> Result<diff::ParsedPatch> {
        let mut args: Vec<OsString> = vec!["--literal-pathspecs".into(), "diff-tree".into()];
        args.extend(DIFF_OPTIONS.iter().map(OsString::from));
        args.extend(["-p".into(), "-U3".into()]);
        push_range(&mut args, oid, base);
        args.push("--".into());
        for path in change.pathspec() {
            args.push(os_path(path)?);
        }
        let output = self
            .bounded(
                &location.root,
                &args,
                Limits {
                    stdout_bytes: DIFF_OUTPUT_BYTES,
                    ..Limits::default()
                },
                cancel,
            )
            .await
            .map_err(|error| match error.code {
                ErrorCode::OutputLimit => diff::too_large(),
                _ => error,
            })?;
        diff::patch(&output, line_limit)
    }

    /// HEAD commit, or `None` on an unborn branch.
    pub(crate) async fn head_oid(
        &self,
        location: &RepositoryLocation,
        cancel: &Cancellation,
    ) -> Result<Option<String>> {
        let output = self
            .process
            .run_with(
                &location.root,
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
                Limits::default(),
                cancel,
            )
            .await?;
        match (output.success, output.code) {
            (true, _) => oid_output(&output.stdout).map(Some),
            (false, Some(1)) => Ok(None),
            _ => Err(local_failure()),
        }
    }

    /// Files of one local group. `base` is the HEAD commit or the empty tree. Non-empty
    /// `paths` restrict the read to those exact (literal) paths.
    pub(crate) async fn local_changes(
        &self,
        location: &RepositoryLocation,
        area: WorkingArea,
        base: &str,
        config: &[(OsString, OsString)],
        paths: &[Vec<u8>],
        cancel: &Cancellation,
    ) -> Result<diff::ParsedChanges> {
        let mut args: Vec<OsString> = vec!["--literal-pathspecs".into()];
        if area == WorkingArea::Untracked {
            args.extend(
                ["ls-files", "-z", "--others", "--exclude-standard", "--"].map(OsString::from),
            );
            for path in paths {
                args.push(os_path(path)?);
            }
            let output = self.local(location, &args, config, cancel).await?;
            return Ok(diff::untracked(&output, DIFF_FILE_LIMIT));
        }
        args.push(match area {
            WorkingArea::Staged => "diff-index".into(),
            _ => "diff-files".into(),
        });
        args.push("-z".into());
        args.extend(DIFF_OPTIONS.iter().map(OsString::from));
        args.extend(["--raw".into(), "--numstat".into()]);
        push_local(&mut args, area, base);
        for path in paths {
            args.push(os_path(path)?);
        }
        let output = self.local(location, &args, config, cancel).await?;
        match area {
            WorkingArea::Unstaged => diff::worktree_changes(&output, DIFF_FILE_LIMIT),
            _ => diff::changes(&output, DIFF_FILE_LIMIT),
        }
    }

    /// Patch of one staged (`HEAD → index`) or unstaged (`index → working tree`) change.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn local_patch(
        &self,
        location: &RepositoryLocation,
        area: WorkingArea,
        base: &str,
        change: &diff::RawChange,
        config: &[(OsString, OsString)],
        line_limit: usize,
        cancel: &Cancellation,
    ) -> Result<diff::ParsedPatch> {
        let command = match area {
            WorkingArea::Staged => "diff-index",
            _ => "diff-files",
        };
        let mut args: Vec<OsString> = vec!["--literal-pathspecs".into(), command.into()];
        args.extend(DIFF_OPTIONS.iter().map(OsString::from));
        args.extend(["-p".into(), "-U3".into()]);
        push_local(&mut args, area, base);
        for path in change.pathspec() {
            args.push(os_path(path)?);
        }
        let output =
            self.local(location, &args, config, cancel)
                .await
                .map_err(|error| match error.code {
                    ErrorCode::OutputLimit => diff::too_large(),
                    _ => error,
                })?;
        diff::patch(&output, line_limit)
    }

    async fn local<S: AsRef<std::ffi::OsStr>>(
        &self,
        location: &RepositoryLocation,
        args: &[S],
        config: &[(OsString, OsString)],
        cancel: &Cancellation,
    ) -> Result<Vec<u8>> {
        let limits = Limits {
            stdout_bytes: DIFF_OUTPUT_BYTES,
            ..Limits::default()
        };
        let output = self
            .process
            .run_configured(&location.root, args, config, limits, cancel)
            .await?;
        if !output.success {
            return Err(local_failure());
        }
        Ok(output.stdout)
    }

    /// Stages whole files: `git add -A` of exact paths, read NUL-delimited from stdin as
    /// literal pathspecs (no path in argv, no pathspec magic). Additions, modifications,
    /// deletions and mode changes alike. The repository is trusted: configured filters
    /// run as Git runs them. Active post-index-change hooks are blocked in preflight.
    pub(crate) async fn stage(
        &self,
        location: &RepositoryLocation,
        paths: &[Vec<u8>],
    ) -> Result<Duration> {
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path);
            input.push(0);
        }
        let args = [
            "--literal-pathspecs",
            "add",
            "-A",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ];
        self.write(location, &args, &input).await
    }

    /// Replaces index entries with `update-index -z --index-info` records (mode, object,
    /// path; mode 0 removes). Only the index changes: the working tree is neither read
    /// nor written, so no filter can run and no file content can be lost.
    pub(crate) async fn set_index_entries(
        &self,
        location: &RepositoryLocation,
        records: &[u8],
    ) -> Result<Duration> {
        self.write(location, &["update-index", "-z", "--index-info"], records)
            .await
    }

    async fn write(
        &self,
        location: &RepositoryLocation,
        args: &[&str],
        input: &[u8],
    ) -> Result<Duration> {
        let limits = Limits {
            timeout: WRITE_TIMEOUT,
            ..Limits::default()
        };
        // Never cancelled by a newer selection: a write runs to its end or its timeout.
        let output = match self
            .process
            .run_input(
                &location.root,
                args,
                &[],
                Some(input),
                limits,
                &Cancellation::default(),
            )
            .await
        {
            Ok(output) => output,
            Err(error) if matches!(error.code, ErrorCode::Timeout) => return Err(outcome_unknown()),
            Err(error) => return Err(error),
        };
        if output.success {
            return Ok(output.duration);
        }
        if output.code.is_none() {
            return Err(outcome_unknown()); // ended by a signal
        }
        if output.stderr.windows(10).any(|w| w == b"index.lock") {
            return Err(index_locked());
        }
        // stderr may contain hostile escapes or private paths; it is not shown.
        Err(AppError::new(
            ErrorCode::CommandFailed,
            "O Git recusou a alteração do index. O estado foi relido; confira o arquivo no terminal se o erro persistir.",
        ))
    }

    async fn bounded<S: AsRef<std::ffi::OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        limits: Limits,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>> {
        let output = self.process.run_with(cwd, args, limits, cancel).await?;
        if !output.success {
            // stderr may contain credentials or hostile terminal escapes; do not expose it.
            return Err(AppError::new(
                ErrorCode::CommandFailed,
                "O Git não conseguiu ler este commit. Ele pode não existir neste repositório.",
            ));
        }
        Ok(output.stdout)
    }
}

fn push_range(args: &mut Vec<OsString>, oid: &str, base: &DiffBase) {
    match base {
        DiffBase::EmptyTree => args.extend(["--root".into(), oid.into()]),
        DiffBase::FirstParent { oid: parent, .. } => {
            args.extend([OsString::from(parent), OsString::from(oid)])
        }
    }
}

/// Staged: `--cached <base> --`; unstaged: submodules count only when their commit moved,
/// because a dirty check would run `git status` inside them with their own configuration.
fn push_local(args: &mut Vec<OsString>, area: WorkingArea, base: &str) {
    match area {
        WorkingArea::Staged => args.extend(["--cached".into(), base.into()]),
        _ => args.push("--ignore-submodules=dirty".into()),
    }
    args.push("--".into());
}

/// Empty tree of the repository's object format: the base of an unborn branch.
pub(crate) fn empty_tree(location: &RepositoryLocation) -> &'static str {
    if location.object_format == "sha256" {
        "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    }
}

/// Writes may run trusted filters (LFS on large files): longer than a read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn index_locked() -> AppError {
    AppError::new(
        ErrorCode::IndexLocked,
        "Outro processo Git está usando o index (index.lock existe). Aguarde ele terminar; se nenhum Git estiver rodando, remova o arquivo manualmente.",
    )
}

fn outcome_unknown() -> AppError {
    AppError::new(
        ErrorCode::OutcomeUnknown,
        "O Git foi interrompido durante a escrita. O resultado não é conhecido; o estado atual foi relido.",
    )
}

fn local_failure() -> AppError {
    // stderr may contain credentials or hostile terminal escapes; do not expose it.
    AppError::new(
        ErrorCode::CommandFailed,
        "O Git não conseguiu ler as alterações locais. Verifique permissões e integridade do repositório.",
    )
}

pub(crate) fn os_path(bytes: &[u8]) -> Result<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Ok(OsString::from(
            std::str::from_utf8(bytes).map_err(|_| AppError::protocol())?,
        ))
    }
}

fn trim_lf(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

fn oid_output(bytes: &[u8]) -> Result<String> {
    let value = parse::text(trim_lf(bytes));
    if !parse::valid_oid(&value) {
        return Err(AppError::protocol());
    }
    Ok(value)
}

fn path_from_output(bytes: &[u8]) -> Result<PathBuf> {
    let bytes = trim_lf(bytes);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
    }
    #[cfg(not(unix))]
    {
        let path = std::str::from_utf8(bytes).map_err(|_| AppError::protocol())?;
        Ok(PathBuf::from(OsString::from(path)))
    }
}

#[derive(Default)]
pub(crate) struct RepoConfig {
    pub drivers: Vec<Vec<u8>>,
    pub sparse: bool,
    pub index_hook: bool,
}

pub(crate) fn inert_filters(drivers: &[Vec<u8>]) -> Result<Vec<(OsString, OsString)>> {
    let mut config = Vec::with_capacity(drivers.len() * 3);
    for driver in drivers {
        let key = |field: &str| -> Result<OsString> {
            let mut key = b"filter.".to_vec();
            key.extend_from_slice(driver);
            key.push(b'.');
            key.extend_from_slice(field.as_bytes());
            os_path(&key)
        };
        config.push((key("clean")?, OsString::new()));
        config.push((key("process")?, OsString::new()));
        config.push((key("required")?, OsString::from("false")));
    }
    Ok(config)
}

/// Why no write may start, judged from the Git directory alone.
pub(crate) fn operation_blocker(git_dir: &Path) -> Option<String> {
    if !std::fs::read(git_dir.join("HEAD")).is_ok_and(|h| h.starts_with(b"ref: refs/heads/")) {
        return Some("HEAD destacado ou indisponível: preparar e retirar do index ficam bloqueados nesta fase.".into());
    }
    let state = operation_state(git_dir);
    (state != NO_OPERATION)
        .then(|| format!("{state}: preparar e retirar do index ficam bloqueados."))
}

pub(crate) const NO_OPERATION: &str = "Nenhuma operação em andamento";

fn head_key(head: &Head) -> String {
    match head {
        Head::Attached { name, oid } => format!("attached:{name}:{oid}"),
        Head::Unborn { name } => format!("unborn:{name}"),
        Head::Detached { oid } => format!("detached:{oid}"),
    }
}

pub(crate) fn operation_state(git_dir: &Path) -> String {
    for (file, state) in [
        ("rebase-merge", "Rebase em andamento"),
        ("rebase-apply", "Rebase/apply em andamento"),
        ("MERGE_HEAD", "Merge em andamento"),
        ("CHERRY_PICK_HEAD", "Cherry-pick em andamento"),
        ("REVERT_HEAD", "Revert em andamento"),
        ("BISECT_LOG", "Bisect em andamento"),
        ("sequencer", "Sequência Git em andamento"),
    ] {
        if git_dir.join(file).exists() {
            return state.into();
        }
    }
    NO_OPERATION.into()
}

fn seal_fingerprint(mut bytes: Vec<u8>, head: &Head, location: &RepositoryLocation) -> Vec<u8> {
    bytes.push(0);
    bytes.extend_from_slice(head_key(head).as_bytes());
    for path in ["shallow", "info/grafts"] {
        bytes.push(0);
        bytes.extend_from_slice(&std::fs::read(location.common_dir.join(path)).unwrap_or_default());
    }
    bytes
}
