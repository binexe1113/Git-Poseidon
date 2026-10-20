#![cfg(unix)]
use poseidon_core::{
    domain::{CommitOutcome, ErrorCode},
    RepositoryService,
};
use std::{os::unix::fs::PermissionsExt, sync::Arc};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;
#[tokio::test]
async fn interrupted_commits_reconcile_head_and_inflight_operations_are_serialized() {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("git");
    std::fs::write(&wrapper,r#"#!/bin/sh
for arg in "$@"; do
 if [ "$arg" = commit ] && [ -f "$POSEIDON_COMMIT_RACE/trigger" ]; then
  mode=$(cat "$POSEIDON_COMMIT_RACE/trigger")
  rm -f "$POSEIDON_COMMIT_RACE/trigger"
  case "$mode" in
   kill-before) kill -KILL $$;;
   fail) exit 42;;
   kill-after) "$POSEIDON_REAL_GIT" "$@"; kill -KILL $$;;
   hold|hold-identical) : > "$POSEIDON_COMMIT_RACE/started"; while [ ! -f "$POSEIDON_COMMIT_RACE/resume" ]; do sleep 0.01; done;;
  esac
  break
 fi
done
exec "$POSEIDON_REAL_GIT" "$@"
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("POSEIDON_REAL_GIT", real);
    std::env::set_var("POSEIDON_COMMIT_RACE", dir.path());
    let path = std::env::join_paths(
        std::iter::once(dir.path().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    std::env::set_var("PATH", path);
    for mode in [
        "kill-before",
        "fail",
        "kill-after",
        "hold",
        "hold-identical",
    ] {
        for file in ["started", "resume"] {
            let _ = std::fs::remove_file(dir.path().join(file));
        }
        let f = Fixture::new();
        f.write("a", "prepared");
        f.git(&["add", "a"]);
        f.write("a", "working");
        if mode == "hold-identical" {
            f.git(&["commit", "-m", "base before race"]);
            f.write("a", "prepared again");
            f.git(&["add", "a"]);
        }
        let service = Arc::new(RepositoryService::default());
        let opened = service.open(&f.root).await.unwrap();
        let (id, g) = (opened.session_id, opened.generation);
        service.set_trust(id, g, true).await.unwrap();
        let w = service.working_changes(id, g).await.unwrap().data;
        let plan = service
            .prepare_commit(id, g, w.revision)
            .await
            .unwrap()
            .data;
        std::fs::write(dir.path().join("trigger"), mode).unwrap();
        let worker = service.clone();
        let token = plan.token;
        let task = tokio::spawn(async move { worker.create_commit(id, g, token, "race").await });
        if mode == "hold" || mode == "hold-identical" {
            for _ in 0..300 {
                if dir.path().join("started").exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(dir.path().join("started").exists());
            for error in [
                service
                    .create_commit(id, g, token, "duplicate")
                    .await
                    .unwrap_err(),
                service.refresh(id).await.unwrap_err(),
                service
                    .set_commit_hook_trust(id, g, true)
                    .await
                    .unwrap_err(),
            ] {
                assert!(matches!(error.code, ErrorCode::Busy));
            }
            // A Git writer wins between application preflight and subprocess startup.
            if mode == "hold-identical" {
                f.git(&["commit", "-m", "external writer, different message"]);
            } else {
                f.write("b", "external");
                f.git(&["add", "b"]);
            }
            std::fs::write(dir.path().join("resume"), "").unwrap();
        }
        let r = task.await.unwrap().unwrap().data;
        let expected = match mode {
            "kill-before" | "hold-identical" => CommitOutcome::Unknown,
            "kill-after" => CommitOutcome::Created,
            _ => CommitOutcome::Rejected,
        };
        assert_eq!(r.outcome, expected, "{mode}: {}", r.message);
        if mode != "hold-identical" {
            assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"working");
        }
        if mode == "kill-after" {
            assert_eq!(f.git(&["show", "HEAD:a"]), "prepared");
            assert!(r.message.contains("falha"));
        } else if mode == "hold-identical" {
            assert_eq!(
                f.git(&["log", "-1", "--format=%s"]),
                "external writer, different message"
            );
        } else {
            assert!(!f.root.join(".git/refs/heads/main").exists());
        }
    }
}
