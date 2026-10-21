//! Public-service invariants with real Git and owned fixtures; no global environment edits.
use poseidon_core::{domain::*, RepositoryService};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;
async fn open(f: &Fixture) -> (RepositoryService, u32, u32) {
    let service = RepositoryService::default();
    let r = service.open(&f.root).await.unwrap();
    service
        .set_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    (service, r.session_id, r.generation)
}

fn racy_index_fixture() -> Fixture {
    let f = Fixture::new();
    f.git(&["config", "core.checkstat", "minimal"]);
    f.git(&["config", "core.trustctime", "false"]);
    f.write("partial", "base!\n");
    f.write("other", "base\n");
    f.commit("base");
    let timestamp = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    f.write("partial", "stage\n");
    std::fs::File::open(f.root.join("partial"))
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    f.git(&["add", "partial"]);
    f.write("partial", "dirty\n");
    std::fs::File::open(f.root.join("partial"))
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    // Deliberately exercise Git's racy timestamp rule, including filesystems
    // whose stat fields cannot distinguish equal-sized consecutive writes.
    std::fs::File::open(f.root.join(".git/index"))
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    f.write("other", "changed\n");
    assert_eq!(f.git(&["show", ":partial"]), "stage");
    assert!(f.git(&["diff-files", "--name-only"]).contains("partial"));
    f
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_commit_preserves_an_unchanged_indexs_racy_timestamp() {
    use std::os::unix::fs::PermissionsExt;
    let f = racy_index_fixture();
    f.write(".git/hooks/pre-commit", "#!/bin/sh\nexit 1\n");
    std::fs::set_permissions(
        f.root.join(".git/hooks/pre-commit"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let before = std::fs::metadata(f.root.join(".git/index"))
        .unwrap()
        .modified()
        .unwrap();
    let before_bytes = std::fs::read(f.root.join(".git/index")).unwrap();
    let (s, id, g) = open(&f).await;
    s.set_commit_hook_trust(id, g, true).await.unwrap();
    let w = s.working_changes(id, g).await.unwrap().data;
    let plan = s.prepare_commit(id, g, w.revision).await.unwrap().data;
    let result = s
        .create_commit(id, g, plan.token, "rejected")
        .await
        .unwrap()
        .data;
    assert_eq!(result.outcome, CommitOutcome::Rejected);
    // Git may legitimately refresh stat/cache entries before a hook rejects
    // the commit. An unchanged index must retain its timestamp; a refreshed
    // one must still expose the unstaged bytes to native Git.
    if std::fs::read(f.root.join(".git/index")).unwrap() == before_bytes {
        assert_eq!(
            std::fs::metadata(f.root.join(".git/index"))
                .unwrap()
                .modified()
                .unwrap(),
            before,
            "an unchanged rejected index was republished with a new timestamp"
        );
    }
    assert!(f.git(&["diff-files", "--name-only"]).contains("partial"));
    assert_eq!(f.git(&["show", ":partial"]), "stage");
}

#[tokio::test]
async fn staging_another_file_preserves_racy_unstaged_content_detection() {
    let f = racy_index_fixture();
    let (s, id, g) = open(&f).await;
    let w = s.working_changes(id, g).await.unwrap().data;
    assert!(w.unstaged.iter().any(|file| file.path == "partial"));
    let selected = w
        .unstaged
        .iter()
        .find(|file| file.path == "other")
        .unwrap()
        .index;
    let after = s
        .stage_file(id, g, w.revision, WorkingArea::Unstaged, selected)
        .await
        .unwrap()
        .data
        .working;
    assert_eq!(f.git(&["show", ":partial"]), "stage");
    assert_eq!(std::fs::read(f.root.join("partial")).unwrap(), b"dirty\n");
    assert!(
        f.git(&["diff-files", "--name-only"]).contains("partial"),
        "private index hid an unstaged file from native Git"
    );
    assert!(after.unstaged.iter().any(|file| file.path == "partial"));
}

#[tokio::test]
async fn lfs_is_refused_without_execution_only_for_paths_that_use_it() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::format(format);
        f.write("a", "base\n");
        f.commit("base");
        f.git(&["switch", "-c", "lfs-target"]);
        f.write(".gitattributes", "a filter=lfs\n");
        f.commit("lfs target");
        f.git(&["switch", "main"]);
        f.git(&["config", "filter.lfs.clean", ": > LFS_RAN; cat"]);
        f.git(&["config", "filter.lfs.smudge", ": > LFS_RAN; cat"]);
        f.git(&["config", "filter.lfs.required", "true"]);
        f.write("a", "working\n");
        let (s, id, g) = open(&f).await;
        let w = s.working_changes(id, g).await.unwrap().data;
        s.stage_file(id, g, w.revision, WorkingArea::Unstaged, 0)
            .await
            .unwrap();
        assert!(
            !f.root.join("LFS_RAN").exists(),
            "unused LFS driver executed"
        );
        f.write(".gitattributes", "a filter=lfs\n");
        f.write("a", "new working\n");
        let before = std::fs::read(f.root.join(".git/index")).unwrap();
        let w = s.working_changes(id, g).await.unwrap().data;
        let selected = w
            .unstaged
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .index;
        assert!(matches!(
            s.stage_file(id, g, w.revision, WorkingArea::Unstaged, selected)
                .await
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedState
        ));
        assert!(!f.root.join("LFS_RAN").exists());
        assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), before);
        // Clean source: LFS appears only in the destination's attribute tree.
        std::fs::remove_file(f.root.join(".gitattributes")).unwrap();
        f.write("a", "working\n");
        let before = std::fs::read(f.root.join(".git/index")).unwrap();
        let oid = f.git(&["rev-parse", "lfs-target"]);
        let error = s
            .prepare_branch(
                id,
                g,
                BranchAction::Switch {
                    name: "lfs-target".into(),
                    expected_oid: oid,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::UnsupportedState));
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), "refs/heads/main");
        assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), before);
        assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"working\n");
        assert!(!f.root.join("LFS_RAN").exists());
    }
}
#[tokio::test]
async fn unsupported_index_storage_fails_closed_but_reading_remains_available() {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.commit("base");
    f.git(&["update-index", "--split-index"]);
    f.write("a", "working\n");
    let (s, id, g) = open(&f).await;
    let w = s.working_changes(id, g).await.unwrap().data;
    let before = std::fs::read(f.root.join(".git/index")).unwrap();
    assert!(matches!(
        s.stage_file(id, g, w.revision, WorkingArea::Unstaged, 0)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedState
    ));
    assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), before);
    f.git(&["update-index", "--no-split-index"]);
    let w = s.working_changes(id, g).await.unwrap().data;
    s.stage_file(id, g, w.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap();
    // The product supports Git >=2.49, which includes refs migrate.
    f.git(&["refs", "migrate", "--ref-format=reftable"]);
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    s.working_changes(r.session_id, r.generation).await.unwrap();
    let error = s
        .prepare_commit(
            r.session_id,
            r.generation,
            s.working_changes(r.session_id, r.generation)
                .await
                .unwrap()
                .data
                .revision,
        )
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::UnsupportedState));
}
#[tokio::test]
async fn deletion_keeps_the_native_upstream_merge_requirement() {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.commit("base");
    f.git(&["branch", "upstream"]);
    f.write("a", "next\n");
    f.commit("next");
    f.git(&["branch", "victim"]);
    f.git(&["config", "branch.victim.remote", "."]);
    f.git(&["config", "branch.victim.merge", "refs/heads/upstream"]);
    let (s, id, g) = open(&f).await;
    let before = f.git(&["rev-parse", "victim"]);
    let plan = s
        .prepare_branch(
            id,
            g,
            BranchAction::Delete {
                name: "victim".into(),
                expected_oid: before.clone(),
            },
        )
        .await
        .unwrap()
        .data;
    assert!(plan.block.is_some());
    assert!(s.execute_branch(id, g, plan.token).await.is_err());
    assert_eq!(f.git(&["rev-parse", "victim"]), before);
    assert!(
        !f.output(&["branch", "-d", "victim"]).status.success(),
        "native positive control did not reject unmerged upstream"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn non_utf8_refs_cannot_alias_a_writable_unicode_ref() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    f.write("a", "base\n");
    f.commit("base");
    let oid = f.git(&["rev-parse", "HEAD"]);
    let raw = std::ffi::OsString::from_vec(b"refs/heads/collision-\xff".to_vec());
    assert!(f
        .command()
        .args([
            std::ffi::OsStr::new("update-ref"),
            &raw,
            std::ffi::OsStr::new(&oid)
        ])
        .status()
        .unwrap()
        .success());
    f.git(&["branch", "collision-�"]);
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    let names = r
        .data
        .branches
        .iter()
        .filter(|b| b.name.starts_with("collision-"))
        .map(|b| b.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    let escaped = names.iter().find(|n| n.contains("\\xff")).unwrap();
    assert!(s
        .prepare_branch(
            r.session_id,
            r.generation,
            BranchAction::Delete {
                name: escaped.clone(),
                expected_oid: oid.clone()
            }
        )
        .await
        .is_err());
    assert_eq!(f.git(&["rev-parse", "collision-�"]), oid);
    assert!(f
        .command()
        .args([
            std::ffi::OsStr::new("show-ref"),
            std::ffi::OsStr::new("--verify"),
            &raw
        ])
        .status()
        .unwrap()
        .success());
}

#[cfg(unix)]
#[tokio::test]
async fn private_post_commit_ref_changes_are_validated_before_publication() {
    use std::os::unix::fs::PermissionsExt;
    for mode in ["wrong-tree", "extra-tag"] {
        let f = Fixture::new();
        f.write("a", "base\n");
        let base = f.commit("base");
        let tree = f.git(&["rev-parse", "HEAD^{tree}"]);
        let other = f.git(&[
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "unexpected private candidate",
        ]);
        f.write("a", "reviewed\n");
        f.git(&["add", "a"]);
        let body = if mode == "wrong-tree" {
            format!("git update-ref HEAD {other}\n")
        } else {
            "git tag private-hook-tag\n".into()
        };
        let hook = f.root.join(".git/hooks/post-commit");
        std::fs::write(&hook, format!("#!/bin/sh\n: > AUTHORIZED_HOOK\n{body}")).unwrap();
        std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (s, id, g) = open(&f).await;
        s.set_commit_hook_trust(id, g, true).await.unwrap();
        let w = s.working_changes(id, g).await.unwrap().data;
        let plan = s.prepare_commit(id, g, w.revision).await.unwrap().data;
        let result = s
            .create_commit(id, g, plan.token, "reviewed commit")
            .await
            .unwrap()
            .data;
        assert!(
            f.root.join("AUTHORIZED_HOOK").exists(),
            "authorized hook was silently disabled"
        );
        if mode == "wrong-tree" {
            assert_eq!(result.outcome, CommitOutcome::Unknown);
            assert_eq!(f.git(&["rev-parse", "HEAD"]), base);
            assert_eq!(f.git(&["show", ":a"]), "reviewed");
        } else {
            assert_eq!(result.outcome, CommitOutcome::Created);
            assert_eq!(f.git(&["show", "HEAD:a"]), "reviewed");
            assert!(
                result.message.contains("metadados"),
                "unsupported hook metadata effects were silently dropped"
            );
            assert!(!f
                .output(&[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    "refs/tags/private-hook-tag"
                ])
                .status
                .success());
        }
    }
}
#[tokio::test]
async fn revoked_base_trust_invalidates_branch_confirmation_even_after_regrant() {
    let f = Fixture::new();
    f.write("a", "base\n");
    let oid = f.commit("base");
    let (s, id, g) = open(&f).await;
    let plan = s
        .prepare_branch(
            id,
            g,
            BranchAction::Create {
                name: "stale-confirmation".into(),
                start_oid: oid,
            },
        )
        .await
        .unwrap()
        .data;
    s.set_trust(id, g, false).await.unwrap();
    s.set_trust(id, g, true).await.unwrap();
    assert!(s.execute_branch(id, g, plan.token).await.is_err());
    assert!(!f
        .output(&[
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/stale-confirmation"
        ])
        .status
        .success());
}
#[tokio::test]
async fn rename_preserves_existing_history_and_respects_disabled_reflogs() {
    for existing_log in [false, true] {
        let f = Fixture::new();
        if !existing_log {
            f.git(&["config", "core.logallrefupdates", "false"]);
        }
        f.write("a", "base\n");
        let oid = f.commit("base");
        f.git(&["branch", "victim"]);
        let before = std::fs::read(f.root.join(".git/logs/refs/heads/victim")).ok();
        assert_eq!(before.is_some(), existing_log);
        f.git(&["config", "core.logallrefupdates", "false"]);
        let (s, id, g) = open(&f).await;
        let plan = s
            .prepare_branch(
                id,
                g,
                BranchAction::Rename {
                    name: "victim".into(),
                    expected_oid: oid,
                    new_name: "renamed".into(),
                },
            )
            .await
            .unwrap()
            .data;
        assert_eq!(
            s.execute_branch(id, g, plan.token)
                .await
                .unwrap()
                .data
                .outcome,
            BranchOutcome::Applied
        );
        let after = std::fs::read(f.root.join(".git/logs/refs/heads/renamed")).ok();
        if let Some(before) = before {
            assert!(after.unwrap().starts_with(&before));
        } else {
            assert!(after.is_none(), "disabled reflog policy was overridden");
        }
    }
}

#[tokio::test]
async fn current_rename_keeps_head_reflog_at_a_real_commit() {
    let f = Fixture::new();
    f.write("a", "base\n");
    let oid = f.commit("base");
    let (s, id, g) = open(&f).await;
    let plan = s
        .prepare_branch(
            id,
            g,
            BranchAction::Rename {
                name: "main".into(),
                expected_oid: oid.clone(),
                new_name: "renamed-main".into(),
            },
        )
        .await
        .unwrap()
        .data;
    assert_eq!(
        s.execute_branch(id, g, plan.token)
            .await
            .unwrap()
            .data
            .outcome,
        BranchOutcome::Applied
    );
    assert_eq!(f.git(&["reflog", "-1", "--format=%H", "HEAD"]), oid);
    assert_eq!(
        f.git(&["reflog", "-1", "--format=%H", "refs/heads/renamed-main"]),
        oid
    );
}
#[tokio::test]
async fn configured_index_hook_is_refused_before_index_writes() {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.commit("base");
    f.write("a", "working\n");
    f.git(&["config", "hook.test.command", ": > HOOK_RAN"]);
    f.git(&["config", "hook.test.event", "post-index-change"]);
    let (s, id, g) = open(&f).await;
    let w = s.working_changes(id, g).await.unwrap().data;
    let before = std::fs::read(f.root.join(".git/index")).unwrap();
    assert!(matches!(
        s.stage_file(id, g, w.revision, WorkingArea::Unstaged, 0)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedState
    ));
    assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), before);
    assert!(!f.root.join("HOOK_RAN").exists());
}
