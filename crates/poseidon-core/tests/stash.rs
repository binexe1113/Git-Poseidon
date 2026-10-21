//! Read-only stash regressions. Fixtures create native stashes; the service must
//! inspect their immutable layers and refuse every mutation without side effects.
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
fn stored() -> Fixture {
    let f = seed();
    f.write("a", "stored\n");
    f.git(&["stash", "push", "-m", "stored"]);
    f
}
fn state(f: &Fixture) -> Vec<Option<Vec<u8>>> {
    [
        "a",
        "u",
        "ignored",
        ".git/index",
        ".git/HEAD",
        ".git/refs/heads/main",
        ".git/refs/stash",
        ".git/logs/refs/stash",
        ".git/index.lock",
    ]
    .map(|p| std::fs::read(f.root.join(p)).ok())
    .into()
}
async fn refused(s: &RepositoryService, r: &SnapshotResponse, selection: StashSelection) {
    for a in [
        create(false),
        create(true),
        StashAction::Apply {
            selection: selection.clone(),
            restore_index: false,
        },
        StashAction::Apply {
            selection: selection.clone(),
            restore_index: true,
        },
        StashAction::Drop { selection },
    ] {
        let err = s
            .prepare_stash(r.session_id, r.generation, a)
            .await
            .unwrap_err();
        assert!(matches!(err.code, ErrorCode::UnsupportedState));
        assert!(err.message.contains("somente para leitura"));
    }
    for token in [0, 1, u32::MAX, 1] {
        assert!(matches!(
            s.execute_stash(r.session_id, r.generation, token)
                .await
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedState
        ));
    }
}
fn fabricated() -> StashSelection {
    StashSelection {
        revision: 0,
        index: 0,
        oid: "0".repeat(40),
    }
}

#[tokio::test]
async fn staged_unstaged_partial_untracked_ignored_and_three_layers_are_read_only() {
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
        f.git(&["stash", "push", "-u", "-m", "rascunho 日本 ; $(literal)"]);
        let (s, r) = open(&f).await;
        let before = state(&f);
        let sel = selection(&s, &r).await;
        let log = s.stashes(r.session_id, r.generation).await.unwrap().data;
        assert!(log.entries[0]
            .message
            .contains("rascunho 日本 ; $(literal)"));
        assert_eq!(log.entries[0].branch.as_deref(), Some("main"));
        let layers = s
            .stash_details(r.session_id, r.generation, sel.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(layers.len(), 3);
        for (i, text, path) in [
            (0, "working", "a"),
            (1, "staged", "a"),
            (2, "new", "日本 ; $(literal)"),
        ] {
            assert_eq!(layers[i].files[0].path, path);
            let diff = s
                .file_diff(r.session_id, r.generation, &layers[i].oid, 0, 0, false)
                .await
                .unwrap();
            assert!(format!("{:?}", diff.data.lines).contains(text));
        }
        refused(&s, &r, sel).await;
        assert_eq!(state(&f), before);
        assert!(f.root.join("ignored").exists());
        assert!(!f.root.join("日本 ; $(literal)").exists());
        assert_eq!(f.output(&["show", "stash^2:a"]).stdout, b"staged\n");
    }
}
#[tokio::test]
async fn tracked_modes_and_optional_untracked_are_inspected_without_writes() {
    for staged in [false, true] {
        let f = seed();
        f.write("a", "change\n");
        if staged {
            f.git(&["add", "a"]);
        }
        f.write("u", "untracked\n");
        f.git(&["stash", "push"]);
        let (s, r) = open(&f).await;
        let before = state(&f);
        let sel = selection(&s, &r).await;
        let layers = s
            .stash_details(r.session_id, r.generation, sel.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].total_files, u32::from(staged));
        refused(&s, &r, sel).await;
        assert_eq!(state(&f), before);
        assert_eq!(std::fs::read(f.root.join("u")).unwrap(), b"untracked\n");
    }
}
#[tokio::test]
async fn empty_and_unborn_remain_readable_and_all_mutations_are_unavailable() {
    for initial in [false, true] {
        let f = if initial { seed() } else { Fixture::new() };
        let (s, r) = open(&f).await;
        let before = state(&f);
        assert!(s
            .stashes(r.session_id, r.generation)
            .await
            .unwrap()
            .data
            .entries
            .is_empty());
        refused(&s, &r, fabricated()).await;
        assert_eq!(state(&f), before);
    }
}
#[tokio::test]
async fn detached_head_inspection_keeps_head_and_index() {
    let f = Fixture::scenario(Scenario::Detached);
    f.write("linear.txt", "dirty\n");
    f.git(&["stash", "push"]);
    let (s, r) = open(&f).await;
    let head = oid(&f, "HEAD");
    let before = state(&f);
    let sel = selection(&s, &r).await;
    assert_eq!(
        s.stash_details(r.session_id, r.generation, sel.clone())
            .await
            .unwrap()
            .data[0]
            .files[0]
            .path,
        "linear.txt"
    );
    refused(&s, &r, sel).await;
    assert_eq!(oid(&f, "HEAD"), head);
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn existing_apply_conflicts_are_inspected_without_resolving_or_retrying() {
    let f = stored();
    let stored = oid(&f, "stash");
    f.write("a", "other\n");
    f.commit("other");
    assert!(!f.output(&["stash", "apply"]).status.success());
    let (s, r) = open(&f).await;
    assert!(r.data.status.conflicts > 0);
    let before = state(&f);
    let sel = selection(&s, &r).await;
    s.stash_details(r.session_id, r.generation, sel.clone())
        .await
        .unwrap();
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
    assert_eq!(oid(&f, "stash"), stored);
    assert!(!f.output(&["ls-files", "-u"]).stdout.is_empty());
}
#[tokio::test]
async fn repeated_requests_cannot_apply_or_consume_a_stash() {
    let f = stored();
    let (s, r) = open(&f).await;
    let before = state(&f);
    let sel = selection(&s, &r).await;
    for _ in 0..3 {
        refused(&s, &r, sel.clone()).await;
    }
    assert_eq!(state(&f), before);
    assert_eq!(f.output(&["show", "stash:a"]).stdout, b"stored\n");
}
#[tokio::test]
async fn reflog_position_and_duplicate_oid_invalidate_inspection_selection() {
    let f = stored();
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let stored = oid(&f, "stash");
    f.write("a", "two\n");
    f.git(&["stash", "push", "-m", "two"]);
    f.git(&["stash", "store", "-m", "duplicate", &stored]);
    let before = state(&f);
    assert!(s
        .stash_details(r.session_id, r.generation, sel.clone())
        .await
        .is_err());
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
    let fresh = selection(&s, &r).await;
    s.stash_details(r.session_id, r.generation, fresh)
        .await
        .unwrap();
    assert_eq!(
        f.output(&["stash", "list"])
            .stdout
            .split(|b| *b == b'\n')
            .filter(|s| !s.is_empty())
            .count(),
        3
    );
}
#[tokio::test]
async fn external_worktree_index_head_reflog_locks_and_config_are_preserved_on_refusal() {
    for what in [
        "file", "index", "head", "reflog", "lock", "config", "new-file",
    ] {
        let f = stored();
        let (s, r) = open(&f).await;
        let sel = selection(&s, &r).await;
        match what {
            "file" => f.write("a", "external\n"),
            "index" => {
                f.write("a", "staged\n");
                f.git(&["add", "a"]);
            }
            "head" => {
                f.git(&["switch", "--detach"]);
            }
            "reflog" => {
                f.write("a", "second\n");
                f.git(&["stash", "push"]);
            }
            "lock" => f.write(".git/index.lock", "third-party lock"),
            "config" => {
                f.git(&["config", "merge.evil.driver", ": > BAD"]);
            }
            _ => f.write("u", "external\n"),
        }
        let before = state(&f);
        refused(&s, &r, sel).await;
        assert_eq!(state(&f), before, "{what}");
        assert!(!f.root.join("BAD").exists());
    }
}
#[tokio::test]
async fn revoked_trust_and_repository_switch_do_not_enable_writes() {
    let f = stored();
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let before = state(&f);
    for trusted in [false, true, false] {
        s.set_trust(r.session_id, r.generation, trusted)
            .await
            .unwrap();
        refused(&s, &r, sel.clone()).await;
    }
    let other = seed();
    s.open(&other.root).await.unwrap();
    assert!(s
        .stash_details(r.session_id, r.generation, sel)
        .await
        .is_err());
    assert!(s
        .execute_stash(r.session_id, r.generation, 1)
        .await
        .is_err());
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn filters_merge_hooks_sparse_submodules_and_operations_never_enable_mutations() {
    for what in [
        "filter",
        "merge",
        "hook",
        "sparse",
        "submodule",
        "merge-head",
    ] {
        let f = stored();
        let (s, r) = open(&f).await;
        let sel = selection(&s, &r).await;
        match what {
            "filter" => {
                f.git(&["config", "filter.evil.clean", ": > BAD; cat"]);
                f.write(".gitattributes", "a filter=evil\n");
            }
            "merge" => {
                f.git(&["config", "merge.evil.driver", ": > BAD; exit 1"]);
                f.write(".gitattributes", "a merge=evil\n");
            }
            "hook" => {
                f.write(".git/hooks/post-index-change", "#!/bin/sh\n: > BAD\n");
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
        }
        let before = state(&f);
        refused(&s, &r, sel).await;
        assert_eq!(state(&f), before, "{what}");
        assert!(!f.root.join("BAD").exists());
    }
}
#[cfg(unix)]
#[tokio::test]
async fn non_utf8_unicode_special_paths_and_symlinks_are_inspected_without_writes() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = seed();
    let path = std::ffi::OsString::from_vec(b"non-utf8-\xff\n.txt".to_vec());
    std::fs::write(f.root.join(&path), b"initial\n").unwrap();
    symlink("a", f.root.join("link")).unwrap();
    f.commit("special");
    std::fs::write(f.root.join(&path), b"dirty\n").unwrap();
    std::fs::remove_file(f.root.join("link")).unwrap();
    symlink("elsewhere", f.root.join("link")).unwrap();
    f.git(&["stash", "push"]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let before = state(&f);
    let d = s
        .stash_details(r.session_id, r.generation, sel.clone())
        .await
        .unwrap();
    assert_eq!(d.data[0].total_files, 2);
    for file in &d.data[0].files {
        s.file_diff(
            r.session_id,
            r.generation,
            &d.data[0].oid,
            file.index,
            0,
            false,
        )
        .await
        .unwrap();
    }
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
    assert_eq!(std::fs::read(f.root.join(path)).unwrap(), b"initial\n");
    assert_eq!(
        std::fs::read_link(f.root.join("link")).unwrap(),
        std::path::Path::new("a")
    );
}
#[tokio::test]
async fn ignored_collision_is_preserved_and_no_apply_token_is_issued() {
    let f = seed();
    f.write("u", "stored\n");
    f.git(&["stash", "push", "-u"]);
    f.write(".gitignore", "ignored\nu\n");
    f.git(&["add", ".gitignore"]);
    f.git(&["commit", "-m", "ignore u"]);
    f.write("u", "precious\n");
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let before = state(&f);
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
    assert_eq!(std::fs::read(f.root.join("u")).unwrap(), b"precious\n");
}
#[tokio::test]
async fn changed_reflog_and_regranted_trust_never_issue_a_plan() {
    let f = stored();
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    refused(&s, &r, sel.clone()).await;
    f.write("a", "second\n");
    f.git(&["stash", "push"]);
    s.set_trust(r.session_id, r.generation, false)
        .await
        .unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    let before = state(&f);
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn hidden_index_flags_and_nested_repositories_are_preserved() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let f = seed();
        f.git(&["update-index", flag, "a"]);
        f.write("a", "precious\n");
        let (s, r) = open(&f).await;
        let before = state(&f);
        refused(&s, &r, fabricated()).await;
        assert_eq!(state(&f), before);
    }
    let f = stored();
    let (s, r) = open(&f).await;
    f.git(&["init", "nested"]);
    let before = state(&f);
    refused(&s, &r, selection(&s, &r).await).await;
    assert_eq!(state(&f), before);
    assert!(f.root.join("nested/.git").exists());
}
#[tokio::test]
async fn malformed_stash_objects_are_refused_for_inspection_and_all_writes() {
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
    let before = state(&f);
    assert!(s
        .stash_details(r.session_id, r.generation, sel.clone())
        .await
        .is_err());
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn untracked_only_stash_and_bounded_listing() {
    let f = seed();
    f.write("u", "only untracked\n");
    f.git(&["stash", "push", "-u"]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let d = s
        .stash_details(r.session_id, r.generation, sel)
        .await
        .unwrap()
        .data;
    assert_eq!(d[0].total_files, 0);
    assert_eq!(d[1].total_files, 0);
    assert_eq!(d[2].total_files, 1);
    // Fixture-only external reflog growth; production never edits internal logs.
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
    let before = state(&f);
    refused(&s, &r, list.entries[0].selection.clone()).await;
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn signing_configuration_does_not_launch_a_signer_for_refused_mutations() {
    let f = stored();
    f.git(&["config", "commit.gpgsign", "true"]);
    f.git(&[
        "config",
        "gpg.program",
        "poseidon-signer-that-must-not-be-executed",
    ]);
    let (s, r) = open(&f).await;
    let before = state(&f);
    refused(&s, &r, selection(&s, &r).await).await;
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn unchanged_gitlinks_in_stored_layers_can_be_inspected_but_never_applied() {
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
    f.git(&["stash", "push", "-m", "unchanged gitlink"]);
    f.git(&["read-tree", &base]);
    f.git(&["commit", "-m", "remove child"]);
    let (s, r) = open(&f).await;
    let sel = selection(&s, &r).await;
    let before = state(&f);
    let layers = s
        .stash_details(r.session_id, r.generation, sel.clone())
        .await
        .unwrap()
        .data;
    assert_eq!(layers[0].files[0].path, "a");
    refused(&s, &r, sel).await;
    assert_eq!(state(&f), before);
}
#[tokio::test]
async fn symbolic_stash_reference_cannot_redirect_any_mutation() {
    let f = seed();
    let head = oid(&f, "HEAD");
    f.write("a", "precious\n");
    for target in ["refs/heads/main", "refs/heads/missing"] {
        f.git(&["symbolic-ref", "refs/stash", target]);
        let (s, r) = open(&f).await;
        let before = state(&f);
        assert!(s.stashes(r.session_id, r.generation).await.is_err());
        refused(&s, &r, fabricated()).await;
        assert_eq!(state(&f), before);
        assert_eq!(oid(&f, "HEAD"), head);
        assert!(!f
            .output(&["show-ref", "--verify", "refs/heads/missing"])
            .status
            .success());
    }
}
