//! Writes that land while Git is running. A wrapper placed first in PATH modifies a file
//! once, when it sees a trigger argument, and then runs the real Git. Own test binary:
//! PATH is process-wide.
#![cfg(unix)]

use poseidon_core::{
    domain::{DiffLineKind, ErrorCode, WorkingArea},
    RepositoryService,
};
use std::{os::unix::fs::PermissionsExt, path::Path};

#[path = "support/mod.rs"]
mod support;
use support::Fixture;

fn real_git() -> std::path::PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|p| p.is_file())
        .unwrap()
}

fn arm(dir: &Path, trigger: &str) {
    std::fs::write(dir.join("trigger"), trigger).unwrap();
}

#[tokio::test]
async fn writes_during_listing_or_patch_are_never_served_as_current() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
# trigger file: "<before|after> <argument>"
if [ -e "$POSEIDON_RACE_DIR/trigger" ]; then
  read -r when trigger < "$POSEIDON_RACE_DIR/trigger"
  for arg in "$@"; do
    if [ "$arg" = "$trigger" ]; then
      rm -f "$POSEIDON_RACE_DIR/trigger"
      echo "$trigger" >> "$POSEIDON_RACE_DIR/fired"
      if [ "$when" = before ]; then
        printf 'written during %s\n' "$trigger" >> "$POSEIDON_RACE_FILE"
        break
      fi
      # After Git has read the worktree, before Poseidon sees the output.
      "$POSEIDON_REAL_GIT" "$@" > "$POSEIDON_RACE_DIR/out"
      status=$?
      printf 'written during %s\n' "$trigger" >> "$POSEIDON_RACE_FILE"
      cat "$POSEIDON_RACE_DIR/out"
      exit $status
    fi
  done
fi
exec "$POSEIDON_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(dir.path().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    std::env::set_var("POSEIDON_REAL_GIT", real_git());
    std::env::set_var("POSEIDON_RACE_DIR", dir.path());
    std::env::set_var("POSEIDON_RACE_FILE", f.root.join("tracked.txt"));
    std::env::set_var("PATH", path);

    f.write("tracked.txt", "base\n");
    f.commit("base");
    f.write("tracked.txt", "base\nedited\n");
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    let (id, generation) = (opened.session_id, opened.generation);
    let listed = service.working_changes(id, generation).await.unwrap().data;

    // The file is appended to while `diff-files -p` runs: the result is refused.
    arm(dir.path(), "before -p");
    let error = service
        .working_diff(
            id,
            generation,
            listed.revision,
            WorkingArea::Unstaged,
            0,
            0,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fired")).unwrap(),
        "-p\n"
    );
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    // Nothing from that request was cached under the old listing either.
    let again = service
        .working_diff(
            id,
            generation,
            listed.revision,
            WorkingArea::Unstaged,
            0,
            0,
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(again.code, ErrorCode::WorkingTreeChanged));

    // The file is appended to after `diff-files` read it, before its output is
    // parsed: the listing is racy, retries, and describes the file as it is now.
    arm(dir.path(), "after diff-files");
    let relisted = service.working_changes(id, generation).await.unwrap().data;
    assert!(std::fs::read_to_string(dir.path().join("fired"))
        .unwrap()
        .ends_with("diff-files\n"));
    assert_eq!(relisted.unstaged[0].additions, Some(3));
    let page = service
        .working_diff(
            id,
            generation,
            relisted.revision,
            WorkingArea::Unstaged,
            0,
            0,
            false,
        )
        .await
        .unwrap()
        .data;
    let added: Vec<&str> = page
        .lines
        .iter()
        .filter(|l| l.kind == DiffLineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        added,
        ["edited", "written during -p", "written during diff-files"]
    );
}
