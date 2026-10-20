//! Stash adapter. Writes use native Git semantics and pinned repository paths.
use super::*;
impl Git {
    pub(crate) async fn stash_log(&self, l: &RepositoryLocation) -> Result<Vec<u8>> {
        // A symbolic refs/stash can redirect a native push/drop into another ref,
        // including HEAD's branch. It is not a supported stash store.
        let symbolic = self
            .process
            .run(&l.root, &["symbolic-ref", "--quiet", "refs/stash"])
            .await?;
        if symbolic.success {
            return Err(AppError::new(ErrorCode::UnsupportedState, "refs/stash é simbólica; use uma referência de stash direta. Nenhuma escrita foi executada."));
        }
        if symbolic.code != Some(1) {
            return Err(local_failure());
        }
        // Missing refs/stash is normal, but other failures must not look like an empty list.
        let exists = self
            .process
            .run(&l.root, &["show-ref", "--verify", "--quiet", "refs/stash"])
            .await?;
        if exists.code == Some(1) {
            return Ok(Vec::new());
        }
        if !exists.success {
            return Err(local_failure());
        }
        if trim_lf(
            &self
                .checked(&l.root, &["cat-file", "-t", "refs/stash"])
                .await?,
        ) != b"commit"
        {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "refs/stash não aponta diretamente a um commit de stash.",
            ));
        }
        self.checked(
            &l.root,
            &[
                "reflog",
                "show",
                "-z",
                "--date=iso-strict",
                "--format=%H%x00%gs%x00%gD%x00%gn%x00%ge",
                "refs/stash",
            ],
        )
        .await
    }
    pub(crate) async fn stash_parents(
        &self,
        l: &RepositoryLocation,
        oid: &str,
    ) -> Result<Vec<String>> {
        let raw = self
            .checked(&l.root, &["rev-list", "--parents", "-1", oid])
            .await?;
        let values: Vec<String> = parse::text(trim_lf(&raw))
            .split_whitespace()
            .skip(1)
            .map(str::to_owned)
            .collect();
        if !(2..=3).contains(&values.len()) || !values.iter().all(|v| parse::valid_oid(v)) {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "Objeto não tem a estrutura de stash suportada.",
            ));
        }
        let index = self
            .checked(&l.root, &["rev-list", "--parents", "-1", &values[1]])
            .await?;
        let expected = format!("{} {}", values[1], values[0]);
        if trim_lf(&index) != expected.as_bytes() {
            return Err(AppError::new(
                ErrorCode::UnsupportedState,
                "Parent do index não corresponde à base do stash.",
            ));
        }
        if let Some(untracked) = values.get(2) {
            let raw = self
                .checked(&l.root, &["rev-list", "--parents", "-1", untracked])
                .await?;
            if trim_lf(&raw) != untracked.as_bytes() {
                return Err(AppError::new(
                    ErrorCode::UnsupportedState,
                    "Camada untracked não é uma árvore independente.",
                ));
            }
        }
        Ok(values)
    }
    pub(crate) async fn stash_write(
        &self,
        l: &RepositoryLocation,
        action: &StashAction,
    ) -> Result<crate::process::Output> {
        let reference;
        let mut args = vec!["stash"];
        match action {
            StashAction::Create {
                message,
                include_untracked,
            } => {
                args.push("push");
                if *include_untracked {
                    args.push("--include-untracked");
                }
                args.extend(["--message", message.as_str()]);
            }
            StashAction::Apply {
                selection,
                restore_index,
            } => {
                args.push("apply");
                if *restore_index {
                    args.push("--index");
                }
                args.push(&selection.oid);
            }
            StashAction::Drop { selection } => {
                reference = format!("stash@{{{}}}", selection.index);
                args.extend(["drop", &reference]);
            }
        }
        let mut pinned: Vec<OsString> = vec![
            "--git-dir".into(),
            l.git_dir.as_os_str().to_owned(),
            "--work-tree".into(),
            l.root.as_os_str().to_owned(),
        ];
        pinned.extend(args.iter().map(OsString::from));
        self.process
            .run_with(
                &l.root,
                &pinned,
                Limits {
                    timeout: Duration::from_secs(120),
                    ..Limits::default()
                },
                &Cancellation::default(),
            )
            .await
    }
}
