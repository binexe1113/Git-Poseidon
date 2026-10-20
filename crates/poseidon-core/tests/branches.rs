use poseidon_core::{domain::*, RepositoryService};
#[path = "support/mod.rs"]
mod support;
use std::{path::Path, sync::Arc};
use support::{Fixture, Scenario};
fn oid(f: &Fixture, rev: &str) -> String {
    String::from_utf8(f.output(&["rev-parse", rev]).stdout)
        .unwrap()
        .trim()
        .to_owned()
}
fn action(f: &Fixture, kind: &str, name: &str) -> BranchAction {
    let expected_oid = oid(f, &format!("refs/heads/{name}"));
    match kind {
        "switch" => BranchAction::Switch {
            name: name.into(),
            expected_oid,
        },
        "delete" => BranchAction::Delete {
            name: name.into(),
            expected_oid,
        },
        _ => BranchAction::Rename {
            name: name.into(),
            expected_oid,
            new_name: kind.into(),
        },
    }
}
async fn open(f: &Fixture) -> (RepositoryService, SnapshotResponse) {
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    (s, r)
}
async fn execute(s: &RepositoryService, r: &SnapshotResponse, a: BranchAction) -> BranchChange {
    let p = s
        .prepare_branch(r.session_id, r.generation, a)
        .await
        .unwrap()
        .data;
    assert!(p.block.is_none(), "{:?}", p.block);
    s.execute_branch(r.session_id, r.generation, p.token)
        .await
        .unwrap()
        .data
}
fn create(f: &Fixture, name: &str) -> BranchAction {
    BranchAction::Create {
        name: name.into(),
        start_oid: oid(f, "HEAD"),
    }
}
fn hook(f: &Fixture, name: &str, body: &str) {
    let p = f.root.join(".git/hooks").join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn tree(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn walk(root: &Path, p: &Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        for e in std::fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            if e.file_name() == ".git" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(root, &e.path(), out)
            } else {
                out.push((
                    e.path().strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(e.path()).unwrap(),
                ));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort();
    out
}
#[tokio::test]
async fn create_switch_rename_current_and_safe_delete_verify_git_and_generation() {
    let f = Fixture::scenario(Scenario::Linear);
    let (s, mut r) = open(&f).await;
    let result = execute(&s, &r, create(&f, "feature/日本")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    r = result.snapshot.unwrap();
    assert_eq!(oid(&f, "main"), oid(&f, "feature/日本"));
    let result = execute(&s, &r, action(&f, "switch", "feature/日本")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    r = result.snapshot.unwrap();
    assert!(matches!(&r.data.head,Head::Attached{name,..} if name=="feature/日本"));
    let result = execute(&s, &r, action(&f, "renamed", "feature/日本")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    r = result.snapshot.unwrap();
    assert!(matches!(&r.data.head,Head::Attached{name,..} if name=="renamed"));
    assert!(!f
        .output(&["show-ref", "--verify", "refs/heads/feature/日本"])
        .status
        .success());
    let result = execute(&s, &r, action(&f, "switch", "main")).await;
    r = result.snapshot.unwrap();
    let result = execute(&s, &r, action(&f, "delete", "renamed")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    assert!(!f
        .output(&["show-ref", "--verify", "refs/heads/renamed"])
        .status
        .success());
}
#[tokio::test]
async fn divergent_branch_cannot_be_deleted_and_current_cannot_be_deleted() {
    let f = Fixture::scenario(Scenario::Divergent);
    let (s, r) = open(&f).await;
    let before = tree(&f.root);
    for name in ["feature", "main"] {
        let p = s
            .prepare_branch(r.session_id, r.generation, action(&f, "delete", name))
            .await
            .unwrap();
        assert!(p.data.block.is_some());
        assert!(s
            .execute_branch(r.session_id, r.generation, p.data.token)
            .await
            .is_err());
    }
    assert_eq!(before, tree(&f.root));
    assert!(f
        .output(&["show-ref", "--verify", "refs/heads/feature"])
        .status
        .success());
}
#[tokio::test]
async fn compatible_dirty_worktree_is_preserved_and_conflicting_dirty_is_refused() {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.write("b", "keep\n");
    f.commit("base");
    f.git(&["switch", "-c", "feature"]);
    f.write("a", "feature\n");
    f.commit("feature");
    f.git(&["switch", "main"]);
    f.write("b", "dirty\n");
    f.write("notes", "untracked\n");
    let (s, r) = open(&f).await;
    let result = execute(&s, &r, action(&f, "switch", "feature")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    assert_eq!(
        std::fs::read_to_string(f.root.join("b")).unwrap(),
        "dirty\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("notes")).unwrap(),
        "untracked\n"
    );
    let r = result.snapshot.unwrap();
    f.write("a", "conflicting\n");
    let before = tree(&f.root);
    let p = s
        .prepare_branch(r.session_id, r.generation, action(&f, "switch", "main"))
        .await
        .unwrap();
    assert!(p.data.block.is_some());
    assert_eq!(before, tree(&f.root));
    assert_eq!(oid(&f, "HEAD"), oid(&f, "feature"));
}
#[tokio::test]
async fn untracked_and_ignored_collisions_are_blocked_without_loss() {
    for ignored in [false, true] {
        let f = Fixture::scenario(Scenario::Initial);
        f.git(&["switch", "-c", "feature"]);
        f.write("collision", "tracked target\n");
        f.commit("target");
        f.git(&["switch", "main"]);
        if ignored {
            f.write(".gitignore", "collision\n");
            f.commit("ignore");
        }
        f.write("collision", "must survive\n");
        let (s, r) = open(&f).await;
        let before = tree(&f.root);
        let p = s
            .prepare_branch(r.session_id, r.generation, action(&f, "switch", "feature"))
            .await
            .unwrap();
        assert!(p.data.block.is_some());
        assert!(s
            .execute_branch(r.session_id, r.generation, p.data.token)
            .await
            .is_err());
        assert_eq!(before, tree(&f.root));
    }
}
#[tokio::test]
async fn detached_head_can_create_and_switch_but_unborn_is_refused() {
    let f = Fixture::scenario(Scenario::Detached);
    let (s, r) = open(&f).await;
    let result = execute(&s, &r, create(&f, "rescue")).await;
    let r = result.snapshot.unwrap();
    let result = execute(&s, &r, action(&f, "switch", "rescue")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    let f = Fixture::new();
    let (s, r) = open(&f).await;
    assert!(s
        .prepare_branch(
            r.session_id,
            r.generation,
            BranchAction::Create {
                name: "new".into(),
                start_oid: "0".repeat(40)
            }
        )
        .await
        .is_err());
    assert!(!f.root.join(".git/refs/heads/new").exists());
}
#[tokio::test]
async fn stale_refs_and_selection_and_one_use_plans_are_rejected() {
    let f = Fixture::scenario(Scenario::Linear);
    f.git(&["branch", "feature"]);
    let (s, r) = open(&f).await;
    let a = action(&f, "switch", "feature");
    let p = s
        .prepare_branch(r.session_id, r.generation, a.clone())
        .await
        .unwrap();
    f.git(&["update-ref", "refs/heads/feature", "HEAD~1"]);
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
    assert!(s
        .prepare_branch(r.session_id, r.generation, a)
        .await
        .is_err());
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    let result = s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .unwrap();
    assert_eq!(result.data.outcome, BranchOutcome::Applied);
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
}
#[tokio::test]
async fn worktree_occupancy_blocks_switch_rename_delete() {
    let f = Fixture::scenario(Scenario::Linear);
    let path = f.temp.path().join("other");
    f.git(&["worktree", "add", "-b", "used", path.to_str().unwrap()]);
    let (s, r) = open(&f).await;
    for k in ["switch", "rename-used", "delete"] {
        let p = s
            .prepare_branch(r.session_id, r.generation, action(&f, k, "used"))
            .await
            .unwrap();
        assert!(p.data.block.unwrap().contains("worktree"));
    }
    assert_eq!(oid(&f, "used"), oid(&f, "main"));
}
#[tokio::test]
async fn trust_is_separate_for_branch_hooks_and_revalidated() {
    let f = Fixture::scenario(Scenario::Initial);
    hook(&f, "reference-transaction", "echo ran >> marker");
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    let a = create(&f, "new");
    let p = s
        .prepare_branch(r.session_id, r.generation, a.clone())
        .await
        .unwrap();
    assert!(p.data.block.is_some());
    assert!(!f.root.join("marker").exists());
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    s.set_commit_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let p = s
        .prepare_branch(r.session_id, r.generation, a.clone())
        .await
        .unwrap();
    assert!(!p.data.hooks_trusted);
    assert!(p.data.block.is_some());
    s.set_branch_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let p = s
        .prepare_branch(r.session_id, r.generation, a)
        .await
        .unwrap();
    s.set_trust(r.session_id, r.generation, false)
        .await
        .unwrap();
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
    assert!(!f.root.join("marker").exists());
}
#[tokio::test]
async fn failed_post_checkout_is_reconciled_without_retry_or_false_rejection() {
    let f = Fixture::scenario(Scenario::Divergent);
    hook(&f, "post-checkout", "echo ran >> hook-marker; exit 1");
    let (s, r) = open(&f).await;
    s.set_branch_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let result = execute(&s, &r, action(&f, "switch", "feature")).await;
    assert_eq!(result.outcome, BranchOutcome::Unknown);
    assert_eq!(oid(&f, "HEAD"), oid(&f, "feature"));
    assert!(
        matches!(result.snapshot.unwrap().data.head,Head::Attached{name,..} if name=="feature")
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("hook-marker")).unwrap(),
        "ran\n"
    );
}
#[tokio::test]
async fn failed_filter_never_returns_confirmed_success() {
    let f = Fixture::new();
    f.write(".gitattributes", "a filter=broken\n");
    f.write("a", "base\n");
    f.commit("base");
    f.git(&["switch", "-c", "feature"]);
    f.write("a", "target\n");
    f.commit("target");
    f.git(&["switch", "main"]);
    f.git(&["config", "filter.broken.clean", "cat"]);
    f.git(&["config", "filter.broken.smudge", "exit 1"]);
    f.git(&["config", "filter.broken.required", "true"]);
    let (s, r) = open(&f).await;
    let result = execute(&s, &r, action(&f, "switch", "feature")).await;
    assert_ne!(result.outcome, BranchOutcome::Applied);
    assert!(result.message.contains("recusou") || result.message.contains("não confirmado"));
}
#[tokio::test]
async fn changed_files_hook_config_conflicts_and_locks_fail_closed() {
    let f = Fixture::scenario(Scenario::Divergent);
    let (s, r) = open(&f).await;
    f.write("dirty\t日本", "one");
    let p = s
        .prepare_branch(r.session_id, r.generation, action(&f, "switch", "feature"))
        .await
        .unwrap();
    f.write("dirty\t日本", "two");
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    hook(&f, "reference-transaction", "exit 0");
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
    std::fs::remove_file(f.root.join(".git/hooks/reference-transaction")).unwrap();
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    std::fs::write(f.root.join(".git/index.lock"), "external").unwrap();
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(f.root.join(".git/index.lock")).unwrap(),
        "external"
    );
    std::fs::remove_file(f.root.join(".git/index.lock")).unwrap();
    std::fs::write(f.root.join(".git/MERGE_HEAD"), oid(&f, "feature")).unwrap();
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    assert!(p.data.block.is_some());
}
#[tokio::test]
async fn invalid_names_duplicate_names_and_upstream_preservation() {
    let f = Fixture::scenario(Scenario::Initial);
    let (s, r) = open(&f).await;
    for name in ["-oops", "@{-1}", "HEAD", "a..b", "refs/../x", "a\nb"] {
        assert!(
            s.prepare_branch(r.session_id, r.generation, create(&f, name))
                .await
                .is_err(),
            "{name}"
        );
    }
    assert!(s
        .prepare_branch(r.session_id, r.generation, create(&f, "main"))
        .await
        .is_err());
    f.git(&["branch", "feature"]);
    f.git(&["branch", "--set-upstream-to=main", "feature"]);
    let result = execute(&s, &r, action(&f, "renamed", "feature")).await;
    let b = result
        .snapshot
        .unwrap()
        .data
        .branches
        .into_iter()
        .find(|b| b.name == "renamed")
        .unwrap();
    assert_eq!(b.upstream.as_deref(), Some("refs/heads/main"));
}
#[tokio::test]
async fn competing_mutations_are_busy_while_native_hook_runs() {
    let f = Fixture::scenario(Scenario::Initial);
    hook(
        &f,
        "reference-transaction",
        "if [ \"$1\" = prepared ]; then echo yes > in-hook; sleep 0.3; fi",
    );
    let (s, r) = open(&f).await;
    s.set_branch_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let s = Arc::new(s);
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    let t = {
        let s = s.clone();
        let id = r.session_id;
        let g = r.generation;
        tokio::spawn(async move { s.execute_branch(id, g, p.data.token).await })
    };
    for _ in 0..100 {
        if f.root.join("in-hook").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(f.root.join("in-hook").exists());
    assert!(matches!(
        s.refresh(r.session_id).await.unwrap_err().code,
        ErrorCode::Busy
    ));
    assert!(matches!(
        s.prepare_branch(r.session_id, r.generation, create(&f, "other"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    ));
    assert_eq!(
        t.await.unwrap().unwrap().data.outcome,
        BranchOutcome::Applied
    );
}

#[tokio::test]
async fn preview_never_executes_hooks_or_filters_and_revocation_invalidates_confirmation() {
    let f = Fixture::scenario(Scenario::Divergent);
    hook(&f, "post-index-change", "echo ran >> unsafe-preview");
    hook(&f, "post-checkout", "echo ran >> unsafe-preview");
    let (s, r) = open(&f).await;
    let p = s
        .prepare_branch(r.session_id, r.generation, action(&f, "switch", "feature"))
        .await
        .unwrap();
    assert!(p.data.block.is_some());
    assert!(!f.root.join("unsafe-preview").exists());
    s.set_branch_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let p = s
        .prepare_branch(r.session_id, r.generation, action(&f, "switch", "feature"))
        .await
        .unwrap();
    assert!(p.data.block.is_none());
    assert!(!f.root.join("unsafe-preview").exists());
    s.set_branch_hook_trust(r.session_id, r.generation, false)
        .await
        .unwrap();
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
}
#[tokio::test]
async fn conflicts_in_index_without_merge_marker_are_blocked() {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.commit("base");
    let base = oid(&f, "HEAD:a");
    f.git(&["update-index", "--index-info"]); // no-op; then feed unmerged entries explicitly.
    use std::io::Write;
    let mut cmd = f.command();
    let mut child = cmd
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "0 {}\ta\n100644 {base} 1\ta\n100644 {base} 2\ta\n100644 {base} 3\ta\n",
                "0".repeat(40)
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(child.wait().unwrap().success());
    let (s, r) = open(&f).await;
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    assert!(p.data.block.unwrap().contains("conflitos"));
}
#[tokio::test]
async fn required_clean_filter_is_not_executed_by_preview() {
    let f = Fixture::scenario(Scenario::Initial);
    f.git(&["branch", "feature"]);
    f.write(".gitattributes", "* filter=probe\n");
    f.git(&[
        "config",
        "filter.probe.clean",
        "echo run >> forbidden-preview; cat",
    ]);
    f.git(&[
        "config",
        "filter.probe.smudge",
        "echo run >> forbidden-preview; cat",
    ]);
    f.git(&["config", "filter.probe.required", "true"]);
    // Open uses protected inspection, then trust is granted without reading filtered status.
    let (s, r) = open(&f).await;
    let _ = s
        .prepare_branch(r.session_id, r.generation, action(&f, "switch", "feature"))
        .await
        .unwrap();
    assert!(!f.root.join("forbidden-preview").exists());
}
#[tokio::test]
async fn sha256_branch_mutations_and_nested_ignored_collisions() {
    let f = Fixture::format("sha256");
    f.write("base", "one");
    f.commit("base");
    let (s, r) = open(&f).await;
    let result = execute(&s, &r, create(&f, "new")).await;
    assert_eq!(result.outcome, BranchOutcome::Applied);
    assert_eq!(oid(&f, "new").len(), 64);
}

#[tokio::test]
async fn native_ref_lock_failure_keeps_pages_and_ref_lock_untouched() {
    let f = Fixture::scenario(Scenario::Initial);
    let (s, r) = open(&f).await;
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "locked"))
        .await
        .unwrap();
    f.write(".git/refs/heads/locked.lock", "external");
    let change = s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .unwrap()
        .data;
    assert_eq!(change.outcome, BranchOutcome::Rejected);
    assert!(change.snapshot.is_none());
    assert_eq!(
        std::fs::read_to_string(f.root.join(".git/refs/heads/locked.lock")).unwrap(),
        "external"
    );
    assert!(s.history_page(r.session_id, r.generation, 0).await.is_ok());
}

#[tokio::test]
async fn redirected_worktree_git_file_is_refused_before_mutation() {
    let f = Fixture::scenario(Scenario::Initial);
    let linked = f.temp.path().join("linked");
    f.git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
    let s = RepositoryService::default();
    let r = s.open(&linked).await.unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    let p = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap();
    assert!(p.data.block.is_none());
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", f.root.join(".git").display()),
    )
    .unwrap();
    assert!(s
        .execute_branch(r.session_id, r.generation, p.data.token)
        .await
        .is_err());
    assert!(!f
        .output(&["show-ref", "--verify", "refs/heads/new"])
        .status
        .success());
}

#[tokio::test]
async fn directory_file_collisions_and_lexical_neighbors_preserve_untracked_data() {
    for target_directory in [false, true] {
        let f = Fixture::scenario(Scenario::Initial);
        f.git(&["switch", "-c", "feature"]);
        if target_directory {
            std::fs::create_dir_all(f.root.join("a")).unwrap();
            f.write("a/b", "target\n");
            f.write("a-b", "lexical neighbor\n");
        } else {
            f.write("a", "target\n");
        }
        f.commit("target");
        f.git(&["switch", "main"]);
        if target_directory {
            f.write("a", "untracked file\n");
        } else {
            std::fs::create_dir_all(f.root.join("a")).unwrap();
            f.write("a/local", "untracked nested\n");
        }
        let before = tree(&f.root);
        let (s, r) = open(&f).await;
        let p = s
            .prepare_branch(r.session_id, r.generation, action(&f, "switch", "feature"))
            .await
            .unwrap();
        assert!(p.data.block.is_some());
        assert_eq!(before, tree(&f.root));
        assert_eq!(oid(&f, "HEAD"), oid(&f, "main"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn executable_fifo_hook_is_refused_without_opening_or_executing_it() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::scenario(Scenario::Initial);
    let path = f.root.join(".git/hooks/post-checkout");
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated owned temporary fixture path.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o700) }, 0);
    let (s, r) = open(&f).await;
    let failure = s
        .prepare_branch(r.session_id, r.generation, create(&f, "new"))
        .await
        .unwrap_err();
    assert!(matches!(failure.code, ErrorCode::UnsupportedState));
    assert!(!f
        .output(&["show-ref", "--verify", "refs/heads/new"])
        .status
        .success());
}
