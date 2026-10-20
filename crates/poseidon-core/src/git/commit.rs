use super::*;
use std::io::Read;

pub(crate) const HOOKS: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-index-change",
    "reference-transaction",
];

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CommitPolicy {
    pub config: Vec<u8>,
    pub author: Option<String>,
    pub committer: Option<String>,
    pub hook_dir: PathBuf,
    pub hooks: Vec<String>,
    pub signing: Option<String>,
    pub unsupported_hooks: bool,
}

impl Git {
    pub(crate) async fn commit_policy(
        &self,
        location: &RepositoryLocation,
    ) -> Result<CommitPolicy> {
        let config = self
            .checked(
                &location.root,
                &["config", "--null", "--list", "--includes"],
            )
            .await?;
        let value = |name: &[u8]| -> Option<&[u8]> {
            config
                .split(|b| *b == 0)
                .filter_map(|r| {
                    r.iter()
                        .position(|b| *b == b'\n')
                        .map(|i| (&r[..i], &r[i + 1..]))
                })
                .filter(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
                .next_back()
        };
        let signing_output = self
            .process
            .run(
                &location.root,
                &["config", "--type=bool", "--get", "commit.gpgsign"],
            )
            .await?;
        let signing = match (signing_output.success, signing_output.code, trim_lf(&signing_output.stdout)) {
            (true, _, b"true") => Some(format!("Assinatura {} configurada: ainda não suportada. Nenhum commit sem assinatura será criado.", value(b"gpg.format").map(parse::text).unwrap_or_else(|| "GPG".into()))),
            (false, Some(1), _) | (true, _, b"false") => None,
            _ => return Err(local_failure()),
        };
        let identity = |var: &'static str| async move {
            let output = self
                .process
                .run_configured(
                    &location.root,
                    &["var", var],
                    &[("user.useConfigOnly".into(), "true".into())],
                    Limits::default(),
                    &Cancellation::default(),
                )
                .await?;
            Ok::<_, AppError>(
                output
                    .success
                    .then(|| {
                        let text = parse::text(trim_lf(&output.stdout));
                        text.rfind('>')
                            .map(|end| text[..=end].to_owned())
                            .unwrap_or(text)
                    })
                    .filter(|id| {
                        id.split_once('<').is_some_and(|(name, email)| {
                            !name.trim().is_empty()
                                && !email.trim_end_matches('>').trim().is_empty()
                        })
                    }),
            )
        };
        let (author, committer) = tokio::try_join!(
            identity("GIT_AUTHOR_IDENT"),
            identity("GIT_COMMITTER_IDENT")
        )?;
        let path = self
            .checked(
                &location.root,
                &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
            )
            .await?;
        let hook_dir = path_from_output(&path)?;
        let mut hooks = Vec::new();
        for name in HOOKS {
            match std::fs::metadata(hook_dir.join(name)) {
                Ok(meta) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if meta.permissions().mode() & 0o111 != 0 {
                            hooks.push((*name).into());
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = meta;
                        hooks.push((*name).into());
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
        let unsupported_hooks = config.split(|b| *b == 0).any(|r| r.starts_with(b"hook."));
        Ok(CommitPolicy {
            config,
            author,
            committer,
            hook_dir,
            hooks,
            signing,
            unsupported_hooks,
        })
    }

    pub(crate) async fn index_records(&self, location: &RepositoryLocation) -> Result<Vec<u8>> {
        self.checked(&location.root, &["ls-files", "--stage", "-z"])
            .await
    }

    /// Read-only oracle: represent the committed tree in the same framing as ls-files -s.
    pub(crate) async fn committed_records(
        &self,
        location: &RepositoryLocation,
        oid: &str,
    ) -> Result<(Vec<u8>, Vec<String>)> {
        let raw = self
            .checked(&location.root, &["ls-tree", "-r", "-z", "--full-tree", oid])
            .await?;
        let mut records = Vec::new();
        for row in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let tab = row
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(AppError::protocol)?;
            let fields: Vec<_> = row[..tab].split(|b| *b == b' ').collect();
            if fields.len() != 3 {
                return Err(AppError::protocol());
            }
            records.extend_from_slice(fields[0]);
            records.push(b' ');
            records.extend_from_slice(fields[2]);
            records.extend_from_slice(b" 0\t");
            records.extend_from_slice(&row[tab + 1..]);
            records.push(0);
        }
        let parents = self
            .checked(&location.root, &["rev-list", "--parents", "-1", oid])
            .await?;
        Ok((
            records,
            parse::text(trim_lf(&parents))
                .split_whitespace()
                .skip(1)
                .map(str::to_owned)
                .collect(),
        ))
    }

    /// Correlates the observed HEAD with this operation's reflog action. A failed
    /// command must never claim another process's identical tree as its own commit.
    pub(crate) async fn commit_recorded(
        &self,
        location: &RepositoryLocation,
        oid: &str,
        action: &str,
    ) -> Option<bool> {
        let raw = self
            .checked(
                &location.root,
                &["reflog", "-1", "--format=%H%x00%gs", "HEAD"],
            )
            .await
            .ok()?;
        if raw.is_empty() {
            return None;
        }
        let mut fields = raw.splitn(2, |b| *b == 0);
        Some(
            fields.next()? == oid.as_bytes()
                && fields.next()?.starts_with(format!("{action}:").as_bytes()),
        )
    }

    /// Private guard hooks delegate actual hooks via Git, then refuse pre-commit index
    /// changes. No user data is interpolated into a script; paths travel in environment
    /// values/argv. This is a Git hook script, never a shell command built from a message.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_commit(
        &self,
        location: &RepositoryLocation,
        policy: &CommitPolicy,
        expected: &[u8],
        message: &str,
        hooks_allowed: bool,
        expected_head: Option<&str>,
        action: &str,
    ) -> Result<(crate::process::Output, u32, bool)> {
        #[cfg(not(unix))]
        {
            let _ = (
                location,
                policy,
                expected,
                message,
                hooks_allowed,
                expected_head,
                action,
            );
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "Criação de commits ainda requer o guard Unix nesta fase.",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let guard = tempfile::Builder::new()
                .prefix("poseidon-commit-")
                .tempdir()
                .map_err(|_| local_failure())?;
            std::fs::write(guard.path().join("expected"), expected).map_err(|_| local_failure())?;
            for name in HOOKS {
                let script = format!("#!/bin/sh\nhook='{name}'\n{GUARD}");
                let path = guard.path().join(name);
                std::fs::write(&path, script).map_err(|_| local_failure())?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| local_failure())?;
            }
            let trace = guard.path().join("trace.jsonl");
            let env = [
                ("GIT_REFLOG_ACTION".into(), action.into()),
                (
                    "POSEIDON_COMMIT_HEAD".into(),
                    expected_head.unwrap_or("").into(),
                ),
                (
                    "POSEIDON_COMMIT_ALLOW_HOOKS".into(),
                    if hooks_allowed { "1" } else { "0" }.into(),
                ),
                (
                    "POSEIDON_COMMIT_GIT".into(),
                    self.process.executable.as_os_str().to_owned(),
                ),
                (
                    "POSEIDON_COMMIT_ORIGINAL".into(),
                    policy.hook_dir.as_os_str().to_owned(),
                ),
                (
                    "POSEIDON_COMMIT_GUARD".into(),
                    guard.path().as_os_str().to_owned(),
                ),
                ("GIT_TRACE2_EVENT".into(), trace.as_os_str().to_owned()),
            ];
            let config = [
                ("core.hooksPath".into(), guard.path().as_os_str().to_owned()),
                ("user.useConfigOnly".into(), "true".into()),
            ];
            let result = self
                .process
                .run_input_env(
                    &location.root,
                    &["commit", "--file=-", "--cleanup=whitespace", "--no-edit"],
                    &config,
                    Some(message.as_bytes()),
                    Limits {
                        timeout: WRITE_TIMEOUT,
                        ..Limits::default()
                    },
                    &Cancellation::default(),
                    &env,
                )
                .await;
            let hook_ms = hook_time(&trace);
            let warning = guard.path().join("hook-failed").exists()
                || guard.path().join("index-changed").exists();
            // Errors still go through HEAD reconciliation in the service.
            result.map(|output| (output, hook_ms, warning))
        }
    }
}

// Guard all pre-commit entry points, even when the original hook is absent. Hooks
// modifying the real index are preserved but cause rejection before a commit exists.
#[cfg(unix)]
const GUARD: &str = r#"
check_index() {
  head=$("$POSEIDON_COMMIT_GIT" rev-parse --verify --quiet HEAD)
  if [ "$head" != "$POSEIDON_COMMIT_HEAD" ]; then : > "$POSEIDON_COMMIT_GUARD/index-changed"; return 1; fi
  current="$POSEIDON_COMMIT_GUARD/current-$$"
  "$POSEIDON_COMMIT_GIT" --no-optional-locks ls-files --stage -z > "$current" || return 1
  /usr/bin/cmp -s "$POSEIDON_COMMIT_GUARD/expected" "$current"
  same=$?
  rm -f "$current"
  if [ "$same" != 0 ]; then : > "$POSEIDON_COMMIT_GUARD/index-changed"; return 1; fi
}
case "$hook" in pre-commit|prepare-commit-msg|commit-msg) check_index || exit 1;; esac
result=0
if [ "$POSEIDON_COMMIT_ALLOW_HOOKS" = 1 ]; then
  "$POSEIDON_COMMIT_GIT" -c "core.hooksPath=$POSEIDON_COMMIT_ORIGINAL" hook run --ignore-missing "$hook" -- "$@"
  result=$?
elif [ -x "$POSEIDON_COMMIT_ORIGINAL/$hook" ]; then
  result=1
fi
if [ "$result" != 0 ]; then : > "$POSEIDON_COMMIT_GUARD/hook-failed"; fi
case "$hook" in pre-commit|prepare-commit-msg|commit-msg) check_index || exit 1;; esac
exit "$result"
"#;

fn hook_time(path: &Path) -> u32 {
    let mut bytes = Vec::new();
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    if file.take(8 * 1024 * 1024).read_to_end(&mut bytes).is_err() {
        return 0;
    }
    let mut root = None;
    let mut hooks = Vec::new();
    let mut seconds = 0.0;
    for row in bytes.split(|b| *b == b'\n') {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(row) else {
            continue;
        };
        if root.is_none() {
            root = v["sid"].as_str().map(str::to_owned);
        }
        if v["sid"].as_str() != root.as_deref() {
            continue;
        }
        if v["event"] == "child_start" && v["child_class"] == "hook" {
            hooks.push(v["child_id"].clone());
        }
        if v["event"] == "child_exit" && hooks.contains(&v["child_id"]) {
            seconds += v["t_rel"].as_f64().unwrap_or(0.0);
        }
    }
    (seconds * 1000.0).min(u32::MAX as f64) as u32
}
