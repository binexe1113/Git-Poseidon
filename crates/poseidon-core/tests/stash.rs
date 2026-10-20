use poseidon_core::{domain::*, RepositoryService};
#[path = "support/mod.rs"]
mod support;
use support::{Fixture, Scenario};
fn create(untracked: bool) -> StashAction {
    StashAction::Create {
        message: "rascunho 日本 ; $(literal)".into(),
        include_untracked: untracked,
    }
}
async fn open(f: &Fixture) -> (RepositoryService, SnapshotResponse) {
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    (s, r)
}
async fn execute(s: &RepositoryService, r: &SnapshotResponse, a: StashAction) -> BranchChange {
    let p = s
        .prepare_stash(r.session_id, r.generation, a)
        .await
        .unwrap()
        .data;
    assert!(p.block.is_none(), "{:?}", p.block);
    s.execute_stash(r.session_id, r.generation, p.token)
        .await
        .unwrap()
        .data
}
async fn selection(s: &RepositoryService, r: &SnapshotResponse) -> StashSelection {
    s.stashes(r.session_id, r.generation)
        .await
        .unwrap()
        .data
        .entries[0]
        .selection
        .clone()
}
fn oid(f: &Fixture, rev: &str) -> String {
    String::from_utf8(f.output(&["rev-parse", rev]).stdout)
        .unwrap()
        .trim()
        .into()
}
fn seed() -> Fixture {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.write(".gitignore", "ignored\n");
    f.commit("base");
    f
}
#[tokio::test]
async fn staged_unstaged_partial_untracked_ignored_and_three_layers_roundtrip() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::format(format);
        f.write("a", "base\n");
        f.write(".gitignore", "ignored\n");
        f.commit("base");
        f.write("a", "staged\n");
        f.git(&["add", "a"]);
        f.write("a", "working\n");
        f.write("日本 ; $(literal)", "new\n");
        f.write("ignored", "keep\n");
        let (s, r) = open(&f).await;
        let c = execute(&s, &r, create(true)).await;
        assert_eq!(c.outcome, BranchOutcome::Applied);
        let r = c.snapshot.unwrap();
        assert_eq!(std::fs::read_to_string(f.root.join("a")).unwrap(), "base\n");
        assert!(!f.root.join("日本 ; $(literal)").exists());
        assert!(f.root.join("ignored").exists());
        assert_eq!(f.output(&["status", "--porcelain"]).stdout, b"");
        let sel = selection(&s, &r).await;
        let log = s.stashes(r.session_id, r.generation).await.unwrap().data;
        assert!(log.entries[0]
            .message
            .contains("rascunho 日本 ; $(literal)"));
        assert_eq!(log.entries[0].branch.as_deref(), Some("main"));
        let d = s
            .stash_details(r.session_id, r.generation, sel.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].files[0].path, "a");
        assert_eq!(d[1].files[0].path, "a");
        assert_eq!(d[2].files[0].path, "日本 ; $(literal)");
        for (i, text) in [(0, "working"), (1, "staged"), (2, "new")] {
            let diff = s
                .file_diff(r.session_id, r.generation, &d[i].oid, 0, 0, false)
                .await
                .unwrap();
            assert!(format!("{:?}", diff.data.lines).contains(text));
        }
        let c = execute(
            &s,
            &r,
            StashAction::Apply {
                selection: sel,
                restore_index: true,
            },
        )
        .await;
        assert_eq!(c.outcome, BranchOutcome::Applied);
        let r = c.snapshot.unwrap();
        assert_eq!(
            std::fs::read_to_string(f.root.join("a")).unwrap(),
            "working\n"
        );
        assert_eq!(f.output(&["show", ":a"]).stdout, b"staged\n");
        assert_eq!(f.output(&["show", "stash:a"]).stdout, b"working\n");
        let sel = selection(&s, &r).await;
        let c = execute(&s, &r, StashAction::Drop { selection: sel }).await;
        assert_eq!(c.outcome, BranchOutcome::Applied);
        let r = c.snapshot.unwrap();
        assert!(s
            .stashes(r.session_id, r.generation)
            .await
            .unwrap()
            .data
            .entries
            .is_empty());
        assert!(f.root.join("ignored").exists());
    }
}
#[tokio::test]
async fn tracked_modes_and_optional_untracked() {
    for staged in [false, true] {
        let f = seed();
        f.write("a", "change\n");
        if staged {
            f.git(&["add", "a"]);
        }
        f.write("u", "untracked\n");
        let (s, r) = open(&f).await;
        let c = execute(&s, &r, create(false)).await;
        assert_eq!(c.outcome, BranchOutcome::Applied);
        assert!(f.root.join("u").exists());
        assert_eq!(f.output(&["show", "stash:a"]).stdout, b"change\n");
        assert_eq!(f.output(&["show", ":a"]).stdout, b"base\n");
    }
}
#[tokio::test]
async fn empty_and_unborn_are_blocked() {
    for initial in [false, true] {
        let f = if initial { seed() } else { Fixture::new() };
        let (s, r) = open(&f).await;
        let p = s
            .prepare_stash(r.session_id, r.generation, create(false))
            .await
            .unwrap()
            .data;
        assert!(p.block.is_some());
        assert!(s
            .execute_stash(r.session_id, r.generation, p.token)
            .await
            .is_err());
    }
}
#[tokio::test]
async fn detached_head_roundtrip() {
    let f = Fixture::scenario(Scenario::Detached);
    f.write("linear.txt", "dirty\n");
    let (s, r) = open(&f).await;
    let head = oid(&f, "HEAD");
    let c = execute(&s, &r, create(false)).await;
    assert_eq!(c.outcome, BranchOutcome::Applied);
    assert_eq!(oid(&f, "HEAD"), head);
    let r = c.snapshot.unwrap();
    let sel = selection(&s, &r).await;
    assert_eq!(
        execute(
            &s,
            &r,
            StashAction::Apply {
                selection: sel,
                restore_index: false
            }
        )
        .await
        .outcome,
        BranchOutcome::Applied
    );
    assert_eq!(oid(&f, "HEAD"), head);
}
#[tokio::test]
async fn apply_conflicts_preserves_stash_and_reports_actual_index() {
    let f = seed();
    f.write("a", "stash\n");
    let (s, r) = open(&f).await;
    let r = execute(&s, &r, create(false)).await.snapshot.unwrap();
    let stored = oid(&f, "stash");
    f.write("a", "other\n");
    f.commit("other");
    let r = s.refresh(r.session_id).await.unwrap();
    let sel = selection(&s, &r).await;
    let c = execute(
        &s,
        &r,
        StashAction::Apply {
            selection: sel,
            restore_index: false,
        },
    )
    .await;
    assert_eq!(c.outcome, BranchOutcome::Unknown);
    assert!(!f.output(&["ls-files", "-u"]).stdout.is_empty());
    assert!(c.snapshot.unwrap().data.status.conflicts > 0);
    assert_eq!(oid(&f, "stash"), stored);
    assert!(std::fs::read_to_string(f.root.join("a"))
        .unwrap()
        .contains("<<<<<<<"));
}
#[tokio::test]
async fn repeated_apply_is_not_automatically_retried() {
    let f = seed();
    f.write("a", "stash\n");
    let (s, r) = open(&f).await;
    let r = execute(&s, &r, create(false)).await.snapshot.unwrap();
    let sel = selection(&s, &r).await;
    let r = execute(
        &s,
        &r,
        StashAction::Apply {
            selection: sel,
            restore_index: false,
        },
    )
    .await
    .snapshot
    .unwrap();
    let sel = selection(&s, &r).await;
    let c = execute(
        &s,
        &r,
        StashAction::Apply {
            selection: sel,
            restore_index: false,
        },
    )
    .await;
    assert_eq!(c.outcome, BranchOutcome::Rejected);
    assert_eq!(f.output(&["show", "stash:a"]).stdout, b"stash\n");
    assert_eq!(
        std::fs::read_to_string(f.root.join("a")).unwrap(),
        "stash\n"
    );
}
#[tokio::test]
async fn reflog_position_and_duplicate_oid_invalidate_selection() {
    let f = seed();
    f.write("a", "one\n");
    f.git(&["stash", "push", "-m", "one"]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let stored = oid(&f, "stash");
    f.write("a", "two\n");
    f.git(&["stash", "push", "-m", "two"]);
    f.git(&["stash", "store", "-m", "duplicate", &stored]);
    assert!(s
        .prepare_stash(
            r.session_id,
            r.generation,
            StashAction::Drop { selection: sel }
        )
        .await
        .is_err());
    let sel = selection(&s, &r).await;
    let c = execute(&s, &r, StashAction::Drop { selection: sel }).await;
    assert_eq!(c.outcome, BranchOutcome::Applied);
    assert_ne!(oid(&f, "stash"), stored);
    assert!(String::from_utf8(f.output(&["stash", "list"]).stdout)
        .unwrap()
        .contains("one"));
}
#[tokio::test]
async fn preview_detects_external_worktree_index_head_reflog_locks_and_config() {
    for what in [
        "file", "index", "head", "reflog", "lock", "config", "new-file",
    ] {
        let f = seed();
        f.write("a", "change\n");
        let (s, r) = open(&f).await;
        let p = s
            .prepare_stash(r.session_id, r.generation, create(false))
            .await
            .unwrap()
            .data;
        assert!(p.block.is_none());
        match what {
            "file" => f.write("a", "external\n"),
            "index" => {
                f.git(&["add", "a"]);
            }
            "head" => {
                f.git(&["switch", "--detach"]);
            }
            "reflog" => {
                f.git(&["stash", "push"]);
            }
            "lock" => f.write(".git/index.lock", "owned by someone else"),
            "config" => {
                f.git(&["config", "merge.unsafe.driver", "touch BAD"]);
            }
            _ => f.write("new", "external\n"),
        };
        assert!(
            s.execute_stash(r.session_id, r.generation, p.token)
                .await
                .is_err(),
            "{what}"
        );
        assert!(s
            .execute_stash(r.session_id, r.generation, p.token)
            .await
            .is_err());
        if what == "lock" {
            assert!(f.root.join(".git/index.lock").exists());
        }
    }
}
#[tokio::test]
async fn revoked_trust_and_repository_switch_reject_pending_plan() {
    let f = seed();
    f.write("a", "change\n");
    let (s, r) = open(&f).await;
    let p = s
        .prepare_stash(r.session_id, r.generation, create(false))
        .await
        .unwrap()
        .data;
    s.set_trust(r.session_id, r.generation, false)
        .await
        .unwrap();
    assert!(s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .is_err());
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    let p = s
        .prepare_stash(r.session_id, r.generation, create(false))
        .await
        .unwrap()
        .data;
    let other = seed();
    s.open(&other.root).await.unwrap();
    assert!(s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .is_err());
    assert!(!f
        .output(&["show-ref", "--verify", "refs/stash"])
        .status
        .success());
}
#[tokio::test]
async fn filter_merge_driver_hook_sparse_submodule_and_operations_block_without_programs() {
    for what in [
        "filter",
        "merge",
        "hook",
        "sparse",
        "submodule",
        "merge-head",
    ] {
        let f = seed();
        f.write("a", "change\n");
        match what {
            "filter" => {
                f.git(&["config", "filter.evil.clean", "touch BAD"]);
                f.write(".gitattributes", "a filter=evil\n");
            }
            "merge" => {
                f.git(&["config", "merge.evil.driver", "touch BAD"]);
                f.write(".gitattributes", "a merge=evil\n");
            }
            "hook" => {
                f.write(".git/hooks/post-index-change", "#!/bin/sh\ntouch BAD\n");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        f.root.join(".git/hooks/post-index-change"),
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                }
            }
            "sparse" => {
                f.git(&["config", "core.sparseCheckout", "true"]);
            }
            "submodule" => {
                f.git(&[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{},child", oid(&f, "HEAD")),
                ]);
            }
            _ => f.write(".git/MERGE_HEAD", &oid(&f, "HEAD")),
        };
        let (s, r) = open(&f).await;
        let p = s
            .prepare_stash(r.session_id, r.generation, create(true))
            .await
            .unwrap()
            .data;
        assert!(p.block.is_some(), "{what}");
        assert!(s
            .execute_stash(r.session_id, r.generation, p.token)
            .await
            .is_err());
        assert!(!f.root.join("BAD").exists());
    }
}
#[cfg(unix)]
#[tokio::test]
async fn non_utf8_unicode_special_paths_and_symlinks_roundtrip() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = seed();
    let path = std::ffi::OsString::from_vec(b"non-utf8-\xff\n.txt".to_vec());
    std::fs::write(f.root.join(&path), b"initial\n").unwrap();
    symlink("a", f.root.join("link")).unwrap();
    f.commit("special");
    std::fs::write(f.root.join(&path), b"dirty\n").unwrap();
    std::fs::remove_file(f.root.join("link")).unwrap();
    symlink("elsewhere", f.root.join("link")).unwrap();
    let (s, r) = open(&f).await;
    let r = execute(&s, &r, create(true)).await.snapshot.unwrap();
    let sel = selection(&s, &r).await;
    let d = s
        .stash_details(r.session_id, r.generation, sel.clone())
        .await
        .unwrap();
    assert_eq!(d.data[0].total_files, 2);
    let c = execute(
        &s,
        &r,
        StashAction::Apply {
            selection: sel,
            restore_index: false,
        },
    )
    .await;
    assert_eq!(c.outcome, BranchOutcome::Applied);
    assert_eq!(std::fs::read(f.root.join(path)).unwrap(), b"dirty\n");
    assert_eq!(
        std::fs::read_link(f.root.join("link")).unwrap(),
        std::path::Path::new("elsewhere")
    );
}
#[tokio::test]
async fn apply_preserves_ignored_collision_and_token_is_single_use() {
    let f = seed();
    f.write("u", "stash\n");
    f.git(&["stash", "push", "-u"]);
    f.write(".gitignore", "ignored\nu\n");
    f.git(&["add", ".gitignore"]);
    f.git(&["commit", "-m", "ignore u"]);
    f.write("u", "precious\n");
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let p = s
        .prepare_stash(
            r.session_id,
            r.generation,
            StashAction::Apply {
                selection: sel,
                restore_index: false,
            },
        )
        .await
        .unwrap()
        .data;
    let c = s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .unwrap()
        .data;
    assert_ne!(c.outcome, BranchOutcome::Applied);
    assert_eq!(
        std::fs::read_to_string(f.root.join("u")).unwrap(),
        "precious\n"
    );
    assert!(s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .is_err());
    assert!(f.output(&["rev-parse", "stash"]).status.success());
}

#[tokio::test]
async fn changed_reflog_after_confirmation_and_regranted_trust_invalidate_plan() {
    let f = seed();
    f.write("a", "first\n");
    f.git(&["stash", "push", "-m", "first"]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let p = s
        .prepare_stash(
            r.session_id,
            r.generation,
            StashAction::Drop { selection: sel },
        )
        .await
        .unwrap()
        .data;
    f.write("a", "second\n");
    f.git(&["stash", "push", "-m", "second"]);
    assert!(s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .is_err());
    assert_eq!(
        String::from_utf8(f.output(&["stash", "list"]).stdout)
            .unwrap()
            .lines()
            .count(),
        2
    );
    f.write("a", "third\n");
    let p = s
        .prepare_stash(r.session_id, r.generation, create(false))
        .await
        .unwrap()
        .data;
    s.set_trust(r.session_id, r.generation, false)
        .await
        .unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    assert!(s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .is_err());
}
#[tokio::test]
async fn hidden_index_flags_and_nested_repositories_are_refused() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let f = seed();
        f.git(&["update-index", flag, "a"]);
        f.write("a", "precious hidden\n");
        f.write("u", "untracked\n");
        let (s, r) = open(&f).await;
        let p = s
            .prepare_stash(r.session_id, r.generation, create(true))
            .await
            .unwrap()
            .data;
        assert!(p.block.is_some());
        assert!(s
            .execute_stash(r.session_id, r.generation, p.token)
            .await
            .is_err());
        assert_eq!(
            std::fs::read_to_string(f.root.join("a")).unwrap(),
            "precious hidden\n"
        );
    }
    let f = seed();
    std::fs::create_dir(f.root.join("nested")).unwrap();
    f.git(&["init", "nested"]);
    let (s, r) = open(&f).await;
    assert!(s
        .prepare_stash(r.session_id, r.generation, create(true))
        .await
        .is_err());
}
#[tokio::test]
async fn malformed_stash_objects_are_refused() {
    let f = seed();
    f.git(&[
        "update-ref",
        "--create-reflog",
        "-m",
        "malformed",
        "refs/stash",
        "HEAD",
    ]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    assert!(s
        .stash_details(r.session_id, r.generation, sel.clone())
        .await
        .is_err());
    assert!(s
        .prepare_stash(
            r.session_id,
            r.generation,
            StashAction::Apply {
                selection: sel,
                restore_index: false
            }
        )
        .await
        .is_err());
}

#[tokio::test]
async fn untracked_only_stash_and_bounded_listing() {
    let f = seed();
    f.write("u", "only untracked\n");
    let (s, r) = open(&f).await;
    let r = execute(&s, &r, create(true)).await.snapshot.unwrap();
    let sel = selection(&s, &r).await;
    let d = s
        .stash_details(r.session_id, r.generation, sel)
        .await
        .unwrap()
        .data;
    assert_eq!(d[0].total_files, 0);
    assert_eq!(d[1].total_files, 0);
    assert_eq!(d[2].total_files, 1);
    let path = f.root.join(".git/logs/refs/stash");
    let line = std::fs::read_to_string(&path).unwrap();
    let (_, rest) = line.split_once(' ').unwrap();
    let id = oid(&f, "stash");
    let duplicate = format!("{id} {rest}");
    std::fs::write(path, format!("{line}{}", duplicate.repeat(1000))).unwrap();
    let list = s.stashes(r.session_id, r.generation).await.unwrap().data;
    assert_eq!(list.entries.len(), 1000);
    assert!(list.has_more);
    assert!(list.entries.iter().all(|e| e.selection.oid == id));
}

#[tokio::test]
async fn signing_configuration_does_not_launch_a_signer_for_native_stash() {
    let f = seed();
    f.write("a", "dirty\n");
    f.git(&["config", "commit.gpgsign", "true"]);
    f.git(&[
        "config",
        "gpg.program",
        "poseidon-signer-that-must-not-be-executed",
    ]);
    let (s, r) = open(&f).await;
    let c = execute(&s, &r, create(false)).await;
    assert_eq!(c.outcome, BranchOutcome::Applied);
    assert_eq!(f.output(&["show", "stash:a"]).stdout, b"dirty\n");
}

#[tokio::test]
async fn unchanged_gitlink_in_stored_layers_is_also_refused() {
    let f = seed();
    let base = oid(&f, "HEAD");
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{base},child"),
    ]);
    f.git(&["commit", "-m", "gitlink base"]);
    f.write("a", "stored\n");
    f.git(&["stash", "push", "-m", "gitlink unchanged"]);
    f.git(&["read-tree", &base]);
    f.git(&["commit", "-m", "remove child"]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let p = s
        .prepare_stash(
            r.session_id,
            r.generation,
            StashAction::Apply {
                selection: sel,
                restore_index: true,
            },
        )
        .await
        .unwrap()
        .data;
    assert!(p.block.is_some());
    assert!(s
        .execute_stash(r.session_id, r.generation, p.token)
        .await
        .is_err());
    assert_eq!(std::fs::read_to_string(f.root.join("a")).unwrap(), "base\n");
}

#[tokio::test]
async fn symbolic_stash_reference_cannot_redirect_writes_into_head() {
    let f = seed();
    let before = oid(&f, "HEAD");
    f.write("a", "precious\n");
    for target in ["refs/heads/main", "refs/heads/missing"] {
        f.git(&["symbolic-ref", "refs/stash", target]);
        let (s, r) = open(&f).await;
        assert!(s.stashes(r.session_id, r.generation).await.is_err());
        assert!(s
            .prepare_stash(r.session_id, r.generation, create(false))
            .await
            .is_err());
        assert_eq!(oid(&f, "HEAD"), before);
        assert_eq!(
            std::fs::read_to_string(f.root.join("a")).unwrap(),
            "precious\n"
        );
        assert!(!f
            .output(&["show-ref", "--verify", "refs/heads/missing"])
            .status
            .success());
    }
}
