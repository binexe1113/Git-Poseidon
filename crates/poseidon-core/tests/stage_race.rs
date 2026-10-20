//! Write failures caused from outside: a wrapper first in PATH (own test binary: PATH is
//! process-wide) acts once when it sees a trigger argument, then runs the real Git.
#![cfg(unix)]

use poseidon_core::{
    domain::{ErrorCode, WorkingArea},
    RepositoryService,
};
use std::os::unix::fs::PermissionsExt;

#[path = "support/mod.rs"]
mod support;
use support::Fixture;

#[tokio::test]
async fn interrupted_and_locked_writes_are_reported_without_rollback() {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|p| p.is_file())
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
# trigger file: "<kill|lock> <argument>"
if [ -e "$POSEIDON_RACE_DIR/trigger" ]; then
  read -r what trigger < "$POSEIDON_RACE_DIR/trigger"
  for arg in "$@"; do
    if [ "$arg" = "$trigger" ]; then
      rm -f "$POSEIDON_RACE_DIR/trigger"
      if [ "$what" = kill ]; then kill -KILL $$; fi
      if [ "$what" = hold ]; then
        : > "$POSEIDON_RACE_DIR/started"
        while [ ! -e "$POSEIDON_RACE_DIR/resume" ]; do sleep 0.01; done
      fi
      # Another Git takes the lock between the app's check and its write.
      if [ "$what" = lock ]; then : > "$POSEIDON_RACE_GITDIR/index.lock"; fi
      break
    fi
  done
fi
exec "$POSEIDON_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let f = Fixture::new();
    let path = std::env::join_paths(
        std::iter::once(dir.path().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    std::env::set_var("POSEIDON_REAL_GIT", real);
    std::env::set_var("POSEIDON_RACE_DIR", dir.path());
    std::env::set_var("POSEIDON_RACE_GITDIR", f.root.join(".git"));
    std::env::set_var("PATH", path);

    f.write("a.txt", "a\n");
    f.commit("base");
    f.write("a.txt", "a2\n");
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    let (id, generation) = (opened.session_id, opened.generation);
    service.set_trust(id, generation, true).await.unwrap();
    let index = || std::fs::read(f.root.join(".git/index")).unwrap();

    // `git add` dies by a signal: the outcome is unknown, nothing is retried or undone.
    let changes = service.working_changes(id, generation).await.unwrap().data;
    let before = index();
    std::fs::write(dir.path().join("trigger"), "kill add").unwrap();
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::OutcomeUnknown), "{error:?}");
    assert_eq!(index(), before);
    // The state was read again: the same file is still unstaged and can be staged.
    let changes = service.working_changes(id, generation).await.unwrap().data;
    assert_eq!(changes.unstaged[0].path, "a.txt");

    // The lock appears after the app's check: Git refuses, the lock is left in place.
    std::fs::write(dir.path().join("trigger"), "lock add").unwrap();
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::IndexLocked), "{error:?}");
    let lock = f.root.join(".git/index.lock");
    assert!(lock.exists(), "the app removed another process's lock");
    assert_eq!(index(), before);
    std::fs::remove_file(lock).unwrap();

    // Same for an unstage (`update-index`) once the file is staged.
    let changes = service.working_changes(id, generation).await.unwrap().data;
    let staged = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap()
        .data;
    let after_stage = index();
    std::fs::write(dir.path().join("trigger"), "kill update-index").unwrap();
    let error = service
        .unstage_file(id, generation, staged.working.revision, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::OutcomeUnknown));
    assert_eq!(index(), after_stage);
    assert_eq!(std::fs::read(f.root.join("a.txt")).unwrap(), b"a2\n");
    // Switching or refreshing a session cannot overtake an in-flight index write.
    f.write("a.txt", "a3\n");
    let service = std::sync::Arc::new(service);
    let changes = service.working_changes(id, generation).await.unwrap().data;
    std::fs::write(dir.path().join("trigger"), "hold add").unwrap();
    let task = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
                .await
        })
    };
    for _ in 0..300 {
        if dir.path().join("started").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(dir.path().join("started").exists());
    assert!(matches!(
        service.refresh(id).await.unwrap_err().code,
        ErrorCode::Busy
    ));
    assert!(matches!(
        service.open(&f.root).await.unwrap_err().code,
        ErrorCode::Busy
    ));
    assert!(matches!(
        service
            .set_trust(id, generation, false)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    ));
    std::fs::write(dir.path().join("resume"), "").unwrap();
    task.await.unwrap().unwrap();
}
