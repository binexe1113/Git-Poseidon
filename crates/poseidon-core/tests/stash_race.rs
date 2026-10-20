//! A dedicated test executable keeps PATH instrumentation isolated from other tests.
#![cfg(unix)]
use poseidon_core::{domain::*, RepositoryService};
use std::{os::unix::fs::PermissionsExt, sync::Arc};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;
#[tokio::test]
async fn interrupted_failed_and_inflight_stashes_reconcile_without_retry_or_rollback() {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("git");
    std::fs::write(&wrapper,r#"#!/bin/sh
for arg in "$@"; do
 if [ "$arg" = stash ] && [ -f "$POSEIDON_STASH_RACE/trigger" ]; then
  mode=$(cat "$POSEIDON_STASH_RACE/trigger")
  rm -f "$POSEIDON_STASH_RACE/trigger"
  case "$mode" in
   kill-before) kill -KILL $$;;
   fail) exit 42;;
   kill-after) "$POSEIDON_STASH_REAL_GIT" "$@"; kill -KILL $$;;
   hold) : > "$POSEIDON_STASH_RACE/started"; while [ ! -f "$POSEIDON_STASH_RACE/resume" ]; do sleep 0.01; done;;
  esac
  break
 fi
done
exec "$POSEIDON_STASH_REAL_GIT" "$@"
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("POSEIDON_STASH_REAL_GIT", real);
    std::env::set_var("POSEIDON_STASH_RACE", dir.path());
    std::env::set_var(
        "PATH",
        std::env::join_paths(
            std::iter::once(dir.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap(),
    );
    for mode in ["kill-before", "fail", "kill-after", "hold"] {
        let f = Fixture::new();
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "dirty\n");
        let s = Arc::new(RepositoryService::default());
        let r = s.open(&f.root).await.unwrap();
        let (id, g) = (r.session_id, r.generation);
        s.set_trust(id, g, true).await.unwrap();
        let p = s
            .prepare_stash(
                id,
                g,
                StashAction::Create {
                    message: "race".into(),
                    include_untracked: false,
                },
            )
            .await
            .unwrap()
            .data;
        assert!(p.block.is_none());
        std::fs::write(dir.path().join("trigger"), mode).unwrap();
        let worker = s.clone();
        let task = tokio::spawn(async move { worker.execute_stash(id, g, p.token).await });
        if mode == "hold" {
            for _ in 0..300 {
                if dir.path().join("started").exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(dir.path().join("started").exists());
            let other = Fixture::new();
            for err in [
                s.refresh(id).await.unwrap_err(),
                s.open(&other.root).await.unwrap_err(),
                s.set_trust(id, g, false).await.unwrap_err(),
                s.execute_stash(id, g, p.token).await.unwrap_err(),
            ] {
                assert!(matches!(err.code, ErrorCode::Busy));
            }
            std::fs::write(dir.path().join("resume"), "").unwrap();
        }
        let c = task.await.unwrap().unwrap().data;
        assert_eq!(
            c.outcome,
            match mode {
                "hold" => BranchOutcome::Applied,
                "fail" => BranchOutcome::Rejected,
                _ => BranchOutcome::Unknown,
            }
        );
        let r = c
            .snapshot
            .expect("must reconcile after every process attempt");
        if mode == "kill-after" || mode == "hold" {
            assert_eq!(f.output(&["show", "stash:a"]).stdout, b"dirty\n");
            assert_eq!(std::fs::read_to_string(f.root.join("a")).unwrap(), "base\n");
            assert_eq!(
                s.stashes(id, r.generation)
                    .await
                    .unwrap()
                    .data
                    .entries
                    .len(),
                1
            );
        } else {
            assert_eq!(
                std::fs::read_to_string(f.root.join("a")).unwrap(),
                "dirty\n"
            );
            assert!(s
                .stashes(id, r.generation)
                .await
                .unwrap()
                .data
                .entries
                .is_empty());
        }
        assert!(s.execute_stash(id, g, p.token).await.is_err());
    }
    // Deterministic reproducer for the remaining external-writer gap, after the
    // final validation and before native Git runs. Passing means reconciliation
    // detects the unexpected result, NOT that positional drop is safe to merge.
    for mode in ["apply", "drop", "late-hook"] {
        let drop = mode == "drop";
        for marker in ["started", "resume"] {
            std::fs::remove_file(dir.path().join(marker)).unwrap();
        }
        let f = Fixture::new();
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "reviewed\n");
        f.git(&["add", "a"]);
        f.git(&["stash", "push", "-m", "reviewed"]);
        let reviewed = f.output(&["rev-parse", "stash"]).stdout;
        let s = Arc::new(RepositoryService::default());
        let r = s.open(&f.root).await.unwrap();
        let (id, g) = (r.session_id, r.generation);
        s.set_trust(id, g, true).await.unwrap();
        let selection = s.stashes(id, g).await.unwrap().data.entries[0]
            .selection
            .clone();
        let action = if drop {
            StashAction::Drop { selection }
        } else {
            StashAction::Apply {
                selection,
                restore_index: true,
            }
        };
        let plan = s.prepare_stash(id, g, action).await.unwrap().data;
        assert!(plan.block.is_none());
        std::fs::write(dir.path().join("trigger"), "hold").unwrap();
        let worker = s.clone();
        let task = tokio::spawn(async move { worker.execute_stash(id, g, plan.token).await });
        for _ in 0..300 {
            if dir.path().join("started").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(dir.path().join("started").exists());
        let external = if mode == "late-hook" {
            // SECURITY BLOCKER: a hook installed after inspection can execute.
            // This fixture-owned marker is the only effect of the test hook.
            f.write(
                ".git/hooks/post-index-change",
                "#!/bin/sh\n: > UNAUTHORIZED_HOOK\n",
            );
            std::fs::set_permissions(
                f.root.join(".git/hooks/post-index-change"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            reviewed.clone()
        } else {
            f.write("a", "external\n");
            f.git(&["stash", "push", "-m", "external"]);
            let external = f.output(&["rev-parse", "stash"]).stdout;
            assert_ne!(reviewed, external);
            external
        };
        std::fs::write(dir.path().join("resume"), "").unwrap();
        let change = task.await.unwrap().unwrap().data;
        assert_eq!(change.outcome, BranchOutcome::Unknown);
        let snapshot = change.snapshot.expect("actual state must be reconciled");
        let list = s.stashes(id, snapshot.generation).await.unwrap().data;
        if drop {
            // SECURITY BLOCKER: Git deleted the newly inserted entry, not the
            // reviewed entry. Detection cannot undo the wrong reflog removal.
            assert_eq!(list.entries.len(), 1);
            assert_eq!(f.output(&["rev-parse", "stash"]).stdout, reviewed);
            assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"base\n");
        } else {
            assert_eq!(list.entries.len(), if mode == "late-hook" { 1 } else { 2 });
            assert_eq!(f.output(&["rev-parse", "stash"]).stdout, external);
            assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"reviewed\n");
            assert_eq!(f.output(&["show", ":a"]).stdout, b"reviewed\n");
        }
        assert_eq!(
            f.root.join("UNAUTHORIZED_HOOK").exists(),
            mode == "late-hook"
        );
        assert!(s.execute_stash(id, g, plan.token).await.is_err());
    }
}
