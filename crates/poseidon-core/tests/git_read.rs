use poseidon_core::{
    domain::{ErrorCode, Head},
    Git, RepositoryService,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[path = "support/mod.rs"]
mod support;
use support::Fixture;

fn all_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &entry.path(), files);
            } else if entry.file_type().unwrap().is_file() {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

#[tokio::test]
async fn detects_git_and_rejects_missing_executable() {
    let installation = Git::discover().unwrap().installation().await.unwrap();
    assert!(installation.version.starts_with("git version "));
    assert!(Path::new(&installation.executable).is_absolute());
    let temp = tempfile::tempdir().unwrap();
    assert!(Git::with_executable(temp.path().join("missing-git")).is_err());
}

#[tokio::test]
async fn empty_repository_is_a_valid_unborn_head() {
    let f = Fixture::new();
    let before = all_files(&f.root);
    let opened = RepositoryService::default().open(&f.root).await.unwrap();
    assert!(matches!(opened.data.head, Head::Unborn { ref name } if name == "main"));
    assert!(opened.data.commits.is_empty());
    assert!(opened.data.branches.is_empty());
    assert!(opened.data.status.changes.is_empty());
    assert_eq!(before, all_files(&f.root));
}

#[tokio::test]
async fn large_status_limits_payload_but_preserves_total_counts() {
    let f = Fixture::new();
    for i in 0..510 {
        f.write(&format!("file-{i:04}.txt"), "content\n");
    }
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert_eq!(data.status.total_files, 510);
    assert_eq!(data.status.untracked, 510);
    assert_eq!(data.status.changes.len(), 500);
    assert!(data.status.has_more_changes);
}

#[tokio::test]
async fn reads_real_metadata_message_branches_and_preserves_every_file() {
    let f = Fixture::new();
    f.write("hello.txt", "first\n");
    let first = f.commit("Primeiro commit\n\nDescrição multilinha.\nSegunda linha.");
    f.write("hello.txt", "second\n");
    let second = f.commit("Atualiza conteúdo");
    f.git(&["branch", "feature/test"]);
    f.git(&["update-ref", "refs/remotes/origin/main", &second]);
    f.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    f.git(&["config", "branch.main.remote", "origin"]);
    f.git(&["config", "branch.main.merge", "refs/heads/main"]);
    let before = all_files(&f.root);
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    assert_eq!(opened.schema_version, 2);
    assert_eq!(opened.data.commits.len(), 2);
    assert_eq!(opened.data.commits[0].oid, second);
    assert_eq!(opened.data.commits[0].parents, vec![first.clone()]);
    assert_eq!(opened.data.commits[0].author_email, "test@example.invalid");
    assert!(["2025-01-01T12:00:00+00:00", "2025-01-01T12:00:00Z"]
        .contains(&opened.data.commits[0].authored_at.as_str()));
    assert!(opened
        .data
        .branches
        .iter()
        .any(|b| b.remote && b.name == "origin/main"));
    assert!(opened
        .data
        .branches
        .iter()
        .any(|b| b.current && b.name == "main"));
    assert!(opened
        .data
        .branches
        .iter()
        .any(|b| b.symbolic_target.as_deref() == Some("refs/remotes/origin/main")));
    let detail = service
        .commit(opened.session_id, opened.generation, &first)
        .await
        .unwrap();
    assert!(detail
        .data
        .message
        .contains("Descrição multilinha.\nSegunda linha."));
    assert_eq!(before, all_files(&f.root));
}

#[tokio::test]
async fn partial_staging_untracked_and_nested_ignore_are_distinct() {
    let f = Fixture::new();
    f.write("file.txt", "base\n");
    f.commit("base");
    f.write("file.txt", "staged\n");
    f.git(&["add", "file.txt"]);
    f.write("file.txt", "unstaged\n");
    f.write("novo arquivo.txt", "new\n");
    std::fs::create_dir(f.root.join("nested")).unwrap();
    f.write("nested/.gitignore", "ignored.txt\n");
    f.write("nested/ignored.txt", "ignored\n");
    let before = all_files(&f.root);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert_eq!(data.status.staged, 1);
    assert_eq!(data.status.modified, 1);
    assert!(data
        .status
        .changes
        .iter()
        .any(|c| c.path == "file.txt" && c.index_status == "M" && c.worktree_status == "M"));
    assert!(data
        .status
        .changes
        .iter()
        .any(|c| c.path == "novo arquivo.txt" && c.untracked));
    assert!(!data
        .status
        .changes
        .iter()
        .any(|c| c.path.contains("ignored.txt")));
    assert_eq!(before, all_files(&f.root));
}

#[tokio::test]
async fn renames_keep_both_paths() {
    let f = Fixture::new();
    f.write("old name.txt", "content\n");
    f.commit("base");
    f.git(&["mv", "old name.txt", "new name.txt"]);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert!(data
        .status
        .changes
        .iter()
        .any(|c| c.path == "new name.txt" && c.original_path.as_deref() == Some("old name.txt")));
}

#[tokio::test]
async fn supports_detached_head_and_sha256() {
    let f = Fixture::format("sha256");
    f.write("file", "data");
    let oid = f.commit("sha256");
    f.git(&["checkout", "--detach", &oid]);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert_eq!(data.object_format, "sha256");
    assert_eq!(data.commits[0].oid.len(), 64);
    assert!(matches!(data.head, Head::Detached { oid: head_oid } if head_oid == oid));
}

#[tokio::test]
async fn linked_worktree_and_bare_are_readable() {
    let f = Fixture::new();
    f.write("file", "data");
    f.commit("base");
    let linked = f.temp.path().join("linked");
    f.git(&[
        "worktree",
        "add",
        "-b",
        "linked-branch",
        linked.to_str().unwrap(),
    ]);
    let service = RepositoryService::default();
    let data = service.open(&linked).await.unwrap().data;
    assert_ne!(data.git_dir, data.common_dir);
    assert!(!data.bare);
    assert!(matches!(data.head, Head::Attached { name, .. } if name == "linked-branch"));
    let bare = f.temp.path().join("bare.git");
    f.git(&[
        "clone",
        "--bare",
        "--no-local",
        f.root.to_str().unwrap(),
        bare.to_str().unwrap(),
    ]);
    let data = service.open(&bare).await.unwrap().data;
    assert!(data.bare);
    assert!(!data.status.available);
    assert_eq!(data.commits.len(), 1);
}

#[tokio::test]
async fn invalid_folder_never_replaces_the_active_session() {
    let f = Fixture::new();
    f.write("file", "data");
    f.commit("base");
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    let invalid = f.temp.path().join("not a repo");
    std::fs::create_dir(&invalid).unwrap();
    let error = service.open(&invalid).await.unwrap_err();
    assert!(matches!(error.code, ErrorCode::InvalidRepository));
    assert_eq!(
        service
            .refresh(opened.session_id)
            .await
            .unwrap()
            .data
            .commits
            .len(),
        1
    );
    let error = service.open(&invalid.join("missing")).await.unwrap_err();
    assert!(matches!(error.code, ErrorCode::InvalidRepository));
}

#[tokio::test]
async fn stale_sessions_generations_and_oid_injection_are_rejected() {
    let f = Fixture::new();
    f.write("file", "data");
    let oid = f.commit("base");
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    assert!(matches!(
        service
            .commit(opened.session_id, 1, "--exec=anything")
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    ));
    service.refresh(opened.session_id).await.unwrap();
    assert!(matches!(
        service
            .commit(opened.session_id, 1, &oid)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    ));
    service.open(&f.root).await.unwrap();
    assert!(matches!(
        service.refresh(opened.session_id).await.unwrap_err().code,
        ErrorCode::StaleSession
    ));
}

#[tokio::test]
async fn conflict_state_is_read_without_resolving_it() {
    let f = Fixture::new();
    f.write("file", "base\n");
    f.commit("base");
    f.git(&["checkout", "-b", "other"]);
    f.write("file", "other\n");
    f.commit("other");
    f.git(&["checkout", "main"]);
    f.write("file", "main\n");
    f.commit("main");
    assert!(!f.output(&["merge", "other"]).status.success());
    let before = all_files(&f.root);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert_eq!(data.status.conflicts, 1);
    assert_eq!(data.operation, "Merge em andamento");
    assert_eq!(before, all_files(&f.root));
}

#[cfg(unix)]
#[tokio::test]
async fn hostile_paths_and_executable_config_are_safe_to_inspect() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
    let f = Fixture::new();
    f.write("file", "base\n");
    f.commit("base");
    let marker = f.temp.path().join("EXECUTED");
    let hook = f.temp.path().join("hook");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.git(&["config", "core.fsmonitor", hook.to_str().unwrap()]);
    f.git(&["config", "core.pager", hook.to_str().unwrap()]);
    f.git(&["config", "gpg.program", hook.to_str().unwrap()]);
    f.git(&["config", "log.showSignature", "true"]);
    for name in [
        "a\nname",
        "a\tname",
        "-flag",
        ":(glob)*",
        "$(touch EXECUTED)",
    ] {
        f.write(name, "text");
    }
    std::fs::write(
        f.root
            .join(std::ffi::OsString::from_vec(b"bad-\xff".to_vec())),
        "text",
    )
    .unwrap();
    let before = all_files(&f.root);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert!(data.status.changes.iter().any(|c| c.path == "a\\nname"));
    assert!(data.status.changes.iter().any(|c| c.path == "bad-\\xff"));
    assert!(!marker.exists());
    assert_eq!(before, all_files(&f.root));
}

#[tokio::test]
async fn history_is_bounded_without_claiming_completeness() {
    let f = Fixture::new();
    // Fast deterministic fixture using plumbing; does not touch a real checkout.
    let tree = f.git(&["mktree"]);
    let mut parent: Option<String> = None;
    for i in 0..205 {
        let message = format!("Commit {i}");
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message.as_str()];
        if let Some(ref oid) = parent {
            args.extend(["-p", oid.as_str()]);
        }
        parent = Some(f.git(&args));
    }
    f.git(&["update-ref", "refs/heads/main", parent.as_deref().unwrap()]);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert_eq!(data.commits.len(), 200);
    assert!(data.has_more_commits);
    assert_eq!(data.commits[0].summary, "Commit 204");
    assert_eq!(data.commits[199].summary, "Commit 5");
}

#[tokio::test]
async fn submodule_changes_are_reported_without_recursive_writes() {
    let child = Fixture::new();
    child.write("file", "child\n");
    child.commit("child");
    let f = Fixture::new();
    f.write("file", "parent\n");
    f.commit("parent");
    f.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        child.root.to_str().unwrap(),
        "sub",
    ]);
    f.commit("submodule");
    // Move the child HEAD without recursively inspecting its dirty contents.
    let child_commit = f
        .command()
        .args([
            "-C",
            "sub",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "commit",
            "--allow-empty",
            "-m",
            "child moved",
        ])
        .output()
        .unwrap();
    assert!(child_commit.status.success());
    f.write("sub/file", "dirty child\n");
    let before = all_files(&f.root);
    let data = RepositoryService::default()
        .open(&f.root)
        .await
        .unwrap()
        .data;
    assert!(data
        .status
        .changes
        .iter()
        .any(|c| c.path == "sub" && c.submodule));
    assert_eq!(before, all_files(&f.root));
}

#[cfg(unix)]
#[tokio::test]
async fn missing_promisor_objects_do_not_start_a_remote_helper() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("file", "base\n");
    let oid = f.commit("base");
    let marker = f.temp.path().join("REMOTE_EXECUTED");
    let helper = f.temp.path().join("remote-helper");
    std::fs::write(
        &helper,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.git(&[
        "config",
        "remote.origin.url",
        &format!("ext::{}", helper.display()),
    ]);
    f.git(&["config", "remote.origin.promisor", "true"]);
    f.git(&["config", "extensions.partialClone", "origin"]);
    f.git(&["config", "protocol.ext.allow", "always"]);
    let object = f.root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    std::fs::remove_file(object).unwrap(); // Owned temporary fixture only.
    let before = all_files(&f.root);
    assert!(RepositoryService::default().open(&f.root).await.is_err());
    assert!(!marker.exists());
    assert_eq!(before, all_files(&f.root));
}
