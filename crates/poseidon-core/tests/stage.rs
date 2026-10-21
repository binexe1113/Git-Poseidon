//! Whole-file stage/unstage against real Git. Each write is repeated with the Git CLI on
//! an identical twin fixture; index (`ls-files -s`), status and working tree must match.
use poseidon_core::{
    domain::{AppError, ChangeKind, ErrorCode, IndexChange, WorkingArea, WorkingChanges},
    RepositoryService, TrustStore,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[path = "support/mod.rs"]
mod support;
use support::Fixture;

/// Every byte under `root`. With `git` false, `.git` is skipped (working tree only).
fn tree(root: &Path, git: bool) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, git: bool, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let relative = entry.path().strip_prefix(root).unwrap().to_owned();
            if !git && relative == Path::new(".git") {
                continue;
            }
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk(root, &entry.path(), git, files);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(entry.path()).unwrap();
                files.insert(relative, target.as_os_str().as_encoded_bytes().to_vec());
            } else {
                files.insert(relative, std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, git, &mut files);
    files
}

/// Index entries (mode, object, stage, path) and porcelain status, as Git reports them.
fn state(f: &Fixture) -> (Vec<u8>, Vec<u8>) {
    let index = f.output(&["ls-files", "-s", "-z"]).stdout;
    let status = f
        // Without optional locks: the oracle itself must not refresh (rewrite) the index.
        .output(&[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
        ])
        .stdout;
    (index, status)
}

async fn trusted(f: &Fixture) -> (RepositoryService, u32, u32) {
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    service
        .set_trust(opened.session_id, opened.generation, true)
        .await
        .unwrap();
    (service, opened.session_id, opened.generation)
}

async fn list(service: &RepositoryService, id: u32, generation: u32) -> WorkingChanges {
    service.working_changes(id, generation).await.unwrap().data
}

fn find(changes: &WorkingChanges, area: WorkingArea, path: &str) -> u32 {
    let group = match area {
        WorkingArea::Staged => &changes.staged,
        WorkingArea::Unstaged => &changes.unstaged,
        WorkingArea::Untracked => &changes.untracked,
    };
    group
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} not in {area:?}"))
        .index
}

async fn stage(
    service: &RepositoryService,
    id: u32,
    generation: u32,
    area: WorkingArea,
    path: &str,
) -> Result<IndexChange, AppError> {
    let changes = list(service, id, generation).await;
    let index = find(&changes, area, path);
    service
        .stage_file(id, generation, changes.revision, area, index)
        .await
        .map(|r| r.data)
}

async fn unstage(
    service: &RepositoryService,
    id: u32,
    generation: u32,
    path: &str,
) -> Result<IndexChange, AppError> {
    let changes = list(service, id, generation).await;
    let index = find(&changes, WorkingArea::Staged, path);
    service
        .unstage_file(id, generation, changes.revision, index)
        .await
        .map(|r| r.data)
}

/// The listing returned by a write (incremental) equals a complete listing read now.
async fn assert_listing_current(
    service: &RepositoryService,
    id: u32,
    generation: u32,
    change: &IndexChange,
) {
    let full = list(service, id, generation).await;
    assert_eq!(change.working.staged, full.staged, "staged after write");
    assert_eq!(
        change.working.unstaged, full.unstaged,
        "unstaged after write"
    );
    assert_eq!(
        change.working.untracked, full.untracked,
        "untracked after write"
    );
    assert_eq!(change.working.revision, full.revision);
}

/// Local changes of every kind, built identically in any fixture.
fn local_changes(f: &Fixture) {
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    f.write("modified.txt", "a\nb\n");
    f.write("deleted.txt", "bye\n");
    f.write("old-name.txt", &body);
    f.write("mode.sh", "echo hi\n");
    f.write("partial.txt", "one\ntwo\nthree\n");
    f.write("-dash.txt", "dash\n");
    f.commit("base");
    f.write("modified.txt", "a\nB\n");
    std::fs::remove_file(f.root.join("deleted.txt")).unwrap();
    std::fs::rename(f.root.join("old-name.txt"), f.root.join("new-name.txt")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.root.join("mode.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("modified.txt", f.root.join("link")).unwrap();
    }
    f.write("partial.txt", "ONE\ntwo\nthree\n");
    f.git(&["add", "partial.txt"]);
    f.write("partial.txt", "ONE\ntwo\nTHREE\n");
    f.write("-dash.txt", "dash 2\n");
    f.write(":(magic) * ?.txt", "magic\n");
    f.write("tab\tand \"quotes\" 日本.txt", "special\n");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"invalid-\xff.txt");
        std::fs::write(f.root.join(name), "bytes\n").unwrap();
    }
}

#[tokio::test]
async fn staging_every_kind_matches_git_add() {
    let (app, twin) = (Fixture::new(), Fixture::new());
    local_changes(&app);
    local_changes(&twin);
    let (service, id, generation) = trusted(&app).await;
    let mut cases = vec![
        (WorkingArea::Unstaged, "modified.txt", "modified.txt"),
        (WorkingArea::Unstaged, "deleted.txt", "deleted.txt"),
        (WorkingArea::Unstaged, "old-name.txt", "old-name.txt"),
        (WorkingArea::Untracked, "new-name.txt", "new-name.txt"),
        (WorkingArea::Unstaged, "partial.txt", "partial.txt"),
        (WorkingArea::Unstaged, "-dash.txt", "-dash.txt"),
        (
            WorkingArea::Untracked,
            ":(magic) * ?.txt",
            ":(magic) * ?.txt",
        ),
        (
            WorkingArea::Untracked,
            "tab\\tand \"quotes\" 日本.txt",
            "tab\tand \"quotes\" 日本.txt",
        ),
    ];
    if cfg!(unix) {
        cases.push((WorkingArea::Unstaged, "mode.sh", "mode.sh"));
        cases.push((WorkingArea::Untracked, "link", "link"));
    }
    let worktree = tree(&app.root, false);
    for (area, listed, real) in cases {
        let change = stage(&service, id, generation, area, listed).await.unwrap();
        twin.git(&["--literal-pathspecs", "add", "-A", "--", real]);
        assert_eq!(state(&app), state(&twin), "after staging {real}");
        assert_listing_current(&service, id, generation, &change).await;
        assert!(change.incremental);
    }
    #[cfg(unix)]
    {
        // Listed with an escaped display name; staged by index, never by that name.
        let changes = list(&service, id, generation).await;
        let invalid = changes
            .untracked
            .iter()
            .find(|f| f.path.starts_with("invalid-"))
            .unwrap();
        service
            .stage_file(
                id,
                generation,
                changes.revision,
                WorkingArea::Untracked,
                invalid.index,
            )
            .await
            .unwrap();
        use std::os::unix::ffi::OsStrExt;
        let output = twin
            .command()
            .args(["--literal-pathspecs", "add", "--"])
            .arg(std::ffi::OsStr::from_bytes(b"invalid-\xff.txt"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(state(&app), state(&twin));
    }
    // The pair of the rename is now staged as one renamed file.
    let changes = list(&service, id, generation).await;
    let renamed = &changes.staged[find(&changes, WorkingArea::Staged, "new-name.txt") as usize];
    assert_eq!(renamed.kind, ChangeKind::Renamed);
    assert!(changes.unstaged.is_empty() && changes.untracked.is_empty());
    // Staging never touched the working tree.
    assert_eq!(tree(&app.root, false), worktree);
}

#[tokio::test]
async fn unstaging_keeps_the_working_tree_and_matches_git_restore() {
    let (app, twin) = (Fixture::new(), Fixture::new());
    for f in [&app, &twin] {
        local_changes(f);
        f.git(&["add", "-A"]);
        f.write("partial.txt", "ONE\ntwo\nTHREE\nfour\n"); // staged and unstaged at once
    }
    let worktree = tree(&app.root, false);
    let (service, id, generation) = trusted(&app).await;
    let changes = list(&service, id, generation).await;
    let mut paths: Vec<(String, String)> = changes
        .staged
        .iter()
        .map(|f| {
            let real = if f.path.starts_with("tab") {
                "tab\tand \"quotes\" 日本.txt".to_owned()
            } else {
                f.path.clone()
            };
            (f.path.clone(), real)
        })
        .filter(|(listed, _)| !listed.starts_with("invalid-"))
        .collect();
    paths.sort();
    assert!(paths.iter().any(|(p, _)| p == "new-name.txt"));
    for (listed, real) in &paths {
        let before = list(&service, id, generation).await;
        let staged = &before.staged[find(&before, WorkingArea::Staged, listed) as usize];
        let old = staged.old_path.clone();
        let change = unstage(&service, id, generation, listed).await.unwrap();
        let mut args = vec!["--literal-pathspecs", "restore", "--staged", "--"];
        if let Some(old) = &old {
            args.push(old);
        }
        args.push(real);
        twin.git(&args);
        assert_eq!(state(&app), state(&twin), "after unstaging {real}");
        assert_listing_current(&service, id, generation, &change).await;
    }
    assert_eq!(
        tree(&app.root, false),
        worktree,
        "unstage changed the working tree"
    );
}

#[tokio::test]
async fn unstaging_without_a_first_commit_removes_the_entry() {
    for format in ["sha1", "sha256"] {
        let (app, twin) = (Fixture::format(format), Fixture::format(format));
        for f in [&app, &twin] {
            f.write("first.txt", "first\n");
            f.git(&["add", "first.txt"]);
        }
        let (service, id, generation) = trusted(&app).await;
        let change = unstage(&service, id, generation, "first.txt")
            .await
            .unwrap();
        twin.git(&["rm", "--cached", "-q", "--", "first.txt"]);
        assert_eq!(state(&app), state(&twin), "{format}");
        assert_eq!(change.working.untracked[0].path, "first.txt");
        assert_eq!(
            std::fs::read(app.root.join("first.txt")).unwrap(),
            b"first\n"
        );
        // And back.
        stage(
            &service,
            id,
            generation,
            WorkingArea::Untracked,
            "first.txt",
        )
        .await
        .unwrap();
        twin.git(&["add", "first.txt"]);
        assert_eq!(state(&app), state(&twin));
    }
}

#[tokio::test]
async fn external_changes_and_stale_selections_are_refused_without_writing() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("base");
    f.write("a.txt", "a2\n");
    f.write("b.txt", "b2\n");
    let (service, id, generation) = trusted(&f).await;
    let changes = list(&service, id, generation).await;
    let a = find(&changes, WorkingArea::Unstaged, "a.txt");
    // The file changed after the listing.
    f.write("a.txt", "a3\n");
    let before = tree(&f.root, true);
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, a)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    assert_eq!(
        tree(&f.root, true),
        before,
        "a refused write changed the repository"
    );
    // The index changed after the listing (another program staged a file).
    let changes = list(&service, id, generation).await;
    f.git(&["add", "b.txt"]);
    let before = tree(&f.root, true);
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, a)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    assert_eq!(tree(&f.root, true), before);
    // Old revision, wrong group, out of range, stale session.
    let fresh = list(&service, id, generation).await;
    for (revision, area, index, code) in [
        (
            changes.revision,
            WorkingArea::Unstaged,
            0,
            "WorkingTreeChanged",
        ),
        (fresh.revision, WorkingArea::Staged, 0, "InvalidInput"),
        (fresh.revision, WorkingArea::Unstaged, 9, "InvalidInput"),
    ] {
        let error = service
            .stage_file(id, generation, revision, area, index)
            .await
            .unwrap_err();
        assert_eq!(format!("{:?}", error.code), code);
    }
    let error = service
        .stage_file(id, generation + 1, fresh.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::StaleSession));
    assert_eq!(tree(&f.root, true), before);
}

#[tokio::test]
async fn concurrent_writes_never_both_run() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("base");
    f.write("a.txt", "a2\n");
    f.write("b.txt", "b2\n");
    let (service, id, generation) = trusted(&f).await;
    let service = Arc::new(service);
    let changes = list(&service, id, generation).await;
    let tasks: Vec<_> = ["a.txt", "b.txt"]
        .into_iter()
        .map(|path| {
            let service = service.clone();
            let index = find(&changes, WorkingArea::Unstaged, path);
            let revision = changes.revision;
            tokio::spawn(async move {
                service
                    .stage_file(id, generation, revision, WorkingArea::Unstaged, index)
                    .await
            })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    let ok = results.iter().filter(|r| r.is_ok()).count();
    // Same revision: the second either finds the permit taken or the listing replaced.
    assert_eq!(
        ok,
        1,
        "{:?}",
        results
            .iter()
            .map(|r| r.as_ref().err().map(|e| format!("{:?}", e.code)))
            .collect::<Vec<_>>()
    );
    for error in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(matches!(
            error.code,
            ErrorCode::Busy | ErrorCode::WorkingTreeChanged
        ));
    }
    let staged = f.git(&["diff", "--cached", "--name-only"]);
    assert_eq!(staged.lines().count(), 1);
}

#[tokio::test]
async fn trust_is_explicit_bound_to_identity_and_revocable() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    f.write("a.txt", "a2\n");
    let store = f.root.parent().unwrap().join("app-data").join("trust.json");
    let service = RepositoryService::with_trust(TrustStore::file(store.clone()));
    let opened = service.open(&f.root).await.unwrap();
    let (id, generation) = (opened.session_id, opened.generation);
    let changes = list(&service, id, generation).await;
    assert!(!changes.trusted, "opening must not grant trust");
    let before = tree(&f.root, true);
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::TrustRequired));
    let error = service
        .unstage_file(id, generation, changes.revision, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::TrustRequired));
    assert_eq!(tree(&f.root, true), before);
    assert!(
        !store.exists(),
        "nothing persisted before an explicit grant"
    );

    service.set_trust(id, generation, true).await.unwrap();
    // The listing read without trust is not used for a write.
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(
        matches!(error.code, ErrorCode::WorkingTreeChanged),
        "{error:?}"
    );
    assert!(list(&service, id, generation).await.trusted);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // Persisted for a new app instance.
    let other = RepositoryService::with_trust(TrustStore::file(store.clone()));
    let reopened = other.open(&f.root).await.unwrap();
    assert!(
        list(&other, reopened.session_id, reopened.generation)
            .await
            .trusted
    );

    // Revocation applies at once.
    service.set_trust(id, generation, false).await.unwrap();
    let changes = list(&service, id, generation).await;
    assert!(!changes.trusted);
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::TrustRequired));

    // Another repository created at the same path is a different identity.
    service.set_trust(id, generation, true).await.unwrap();
    std::fs::remove_dir_all(f.root.join(".git")).unwrap();
    f.git(&["init", "-q", "--initial-branch=main"]);
    let fresh = RepositoryService::with_trust(TrustStore::file(store.clone()));
    let replaced = fresh.open(&f.root).await.unwrap();
    assert!(
        !list(&fresh, replaced.session_id, replaced.generation)
            .await
            .trusted
    );

    // A damaged store is never treated as trust, nor silently rewritten.
    std::fs::write(&store, "{ not json").unwrap();
    let broken = RepositoryService::with_trust(TrustStore::file(store.clone()));
    let opened = broken.open(&f.root).await.unwrap();
    let error = broken
        .set_trust(opened.session_id, opened.generation, true)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::CommandFailed));
    assert_eq!(std::fs::read(&store).unwrap(), b"{ not json");
}

#[tokio::test]
async fn filters_run_only_after_trust_like_git_add() {
    // Controlled stand-in for Git LFS (not installed here): a clean filter that stores a
    // pointer with the content's hash and size, and leaves a marker each time it runs.
    let (app, twin) = (Fixture::new(), Fixture::new());
    let markers: Vec<PathBuf> = [&app, &twin]
        .iter()
        .map(|f| f.root.parent().unwrap().join("filter-ran"))
        .collect();
    for (f, marker) in [&app, &twin].into_iter().zip(&markers) {
        f.write(".gitattributes", "*.bin filter=pointer -text\n");
        f.write("seed.txt", "seed\n");
        f.commit("base");
        let clean = format!(
            "sh -c 'echo ran >> \"{}\"; c=$(cat | sha256sum | cut -d\" \" -f1); printf \"version https://git-lfs.github.com/spec/v1\\noid sha256:%s\\n\" \"$c\"'",
            marker.display()
        );
        f.git(&["config", "filter.pointer.clean", &clean]);
        f.git(&["config", "filter.pointer.required", "true"]);
        // Deterministic analogue of an additional host-configured driver (e.g. LFS).
        f.git(&["config", "filter.unused.clean", &clean]);
        std::fs::write(f.root.join("asset.bin"), [0u8, 1, 2, 3, 255]).unwrap();
        // Force both Git and the inert inspection to examine content instead
        // of accepting a cached stat match. This keeps the no-filter assertion
        // independent of filesystem timestamp granularity and scheduling.
        std::fs::File::open(f.root.join("asset.bin"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
    }
    let service = RepositoryService::default();
    let opened = service.open(&app.root).await.unwrap();
    let (id, generation) = (opened.session_id, opened.generation);
    let untrusted = list(&service, id, generation).await;
    // Other drivers (e.g. Git LFS) may also be configured.
    // The security invariant is that our fixture's driver is discovered but not run.
    assert!(untrusted
        .filter_drivers
        .iter()
        .any(|driver| driver == "pointer"));
    assert!(untrusted
        .filter_drivers
        .iter()
        .any(|driver| driver == "unused"));
    assert!(!markers[0].exists(), "a filter ran without trust");
    let error = service
        .stage_file(
            id,
            generation,
            untrusted.revision,
            WorkingArea::Untracked,
            0,
        )
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::TrustRequired));
    assert!(!markers[0].exists(), "a rejected write ran the filter");
    service.set_trust(id, generation, true).await.unwrap();
    let change = stage(
        &service,
        id,
        generation,
        WorkingArea::Untracked,
        "asset.bin",
    )
    .await
    .unwrap();
    assert!(
        markers[0].exists(),
        "the trusted write did not run the filter"
    );
    twin.git(&["add", "asset.bin"]);
    assert_eq!(state(&app), state(&twin));
    let blob = app.git(&["cat-file", "-p", ":asset.bin"]);
    assert!(blob.starts_with("version https://git-lfs.github.com/spec/v1"));
    // Inspection is now raw even after trust. The pointer/blob oracle above
    // still proves that writes preserved the authorized filter semantics.
    assert_eq!(change.working.unstaged.len(), 1);
    assert_eq!(change.working.unstaged[0].path, "asset.bin");
    let executions_after_write = std::fs::read(&markers[0]).unwrap();
    assert_listing_current(&service, id, generation, &change).await;
    assert_eq!(std::fs::read(&markers[0]).unwrap(), executions_after_write);
    service.set_trust(id, generation, false).await.unwrap();
    let executions = std::fs::read(&markers[0]).unwrap();
    let revoked = list(&service, id, generation).await;
    assert!(!revoked.trusted);
    assert_eq!(std::fs::read(&markers[0]).unwrap(), executions);
    let error = service
        .unstage_file(id, generation, revoked.revision, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::TrustRequired));
    assert_eq!(std::fs::read(&markers[0]).unwrap(), executions);
}

#[tokio::test]
async fn repeated_repository_recreation_never_inherits_persisted_trust() {
    let f = Fixture::new();
    let store = f.temp.path().join("trust.json");
    for _ in 0..24 {
        let service = RepositoryService::with_trust(TrustStore::file(store.clone()));
        let opened = service.open(&f.root).await.unwrap();
        assert!(
            !list(&service, opened.session_id, opened.generation)
                .await
                .trusted
        );
        service
            .set_trust(opened.session_id, opened.generation, true)
            .await
            .unwrap();
        // Ordinary Git directory mutations must not revoke a valid identity.
        f.git(&["config", "test.identity", "unchanged"]);
        assert!(
            list(&service, opened.session_id, opened.generation)
                .await
                .trusted
        );
        std::fs::remove_dir_all(f.root.join(".git")).unwrap();
        f.git(&["init", "-q", "--initial-branch=main"]);
    }
    let fresh = RepositoryService::with_trust(TrustStore::file(store));
    let opened = fresh.open(&f.root).await.unwrap();
    assert!(
        !list(&fresh, opened.session_id, opened.generation)
            .await
            .trusted
    );
}

#[tokio::test]
async fn conflicts_operations_submodules_and_sparse_checkout_are_blocked() {
    let blocked = |error: AppError| {
        assert!(
            matches!(error.code, ErrorCode::UnsupportedState),
            "{error:?}"
        )
    };
    // Merge with a conflict: the conflicted file and every other write are refused.
    let f = Fixture::new();
    f.write("conflict.txt", "base\n");
    f.write("other.txt", "x\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("conflict.txt", "topic\n");
    f.commit("topic");
    f.git(&["switch", "-q", "main"]);
    f.write("conflict.txt", "main\n");
    f.commit("main");
    assert!(!f.output(&["merge", "topic"]).status.success());
    f.write("other.txt", "y\n");
    let (service, id, generation) = trusted(&f).await;
    let changes = list(&service, id, generation).await;
    assert!(changes.write_block.is_some());
    let before = tree(&f.root, true);
    for path in ["conflict.txt", "other.txt"] {
        blocked(
            stage(&service, id, generation, WorkingArea::Unstaged, path)
                .await
                .unwrap_err(),
        );
    }
    assert_eq!(tree(&f.root, true), before);

    // Unmerged index without a sequencer marker must still block every file.
    std::fs::remove_file(f.root.join(".git/MERGE_HEAD")).unwrap();
    assert!(list(&service, id, generation)
        .await
        .write_block
        .unwrap()
        .contains("conflitos"));
    let before = tree(&f.root, true);
    blocked(
        stage(&service, id, generation, WorkingArea::Unstaged, "other.txt")
            .await
            .unwrap_err(),
    );
    assert_eq!(tree(&f.root, true), before);

    // Submodule whose commit moved, and a nested repository.
    let f = Fixture::new();
    f.write("seed.txt", "seed\n");
    std::fs::create_dir(f.root.join("sub")).unwrap();
    let sub = |args: &[&str]| {
        let output = f
            .command()
            .current_dir(f.root.join("sub"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
    };
    let id = [
        "-c",
        "user.name=Sub",
        "-c",
        "user.email=sub@example.invalid",
    ];
    sub(&["init", "-q"]);
    sub(&[&id[..], &["commit", "-q", "--allow-empty", "-m", "one"]].concat());
    f.commit("with gitlink");
    sub(&[&id[..], &["commit", "-q", "--allow-empty", "-m", "two"]].concat());
    std::fs::create_dir(f.root.join("nested")).unwrap();
    let nested = f
        .command()
        .current_dir(f.root.join("nested"))
        .args(["init", "-q"])
        .output()
        .unwrap();
    assert!(nested.status.success());
    let (service, id, generation) = trusted(&f).await;
    let before = tree(&f.root, true);
    blocked(
        stage(&service, id, generation, WorkingArea::Unstaged, "sub")
            .await
            .unwrap_err(),
    );
    blocked(
        stage(&service, id, generation, WorkingArea::Untracked, "nested/")
            .await
            .unwrap_err(),
    );
    assert_eq!(tree(&f.root, true), before);

    // Sparse checkout.
    let f = Fixture::new();
    std::fs::create_dir(f.root.join("in")).unwrap();
    f.write("in/a.txt", "a\n");
    f.write("top.txt", "t\n");
    f.commit("base");
    f.git(&["sparse-checkout", "set", "in"]);
    f.write("in/a.txt", "a2\n");
    let (service, id, generation) = trusted(&f).await;
    assert!(list(&service, id, generation).await.write_block.is_some());
    let before = tree(&f.root, true);
    blocked(
        stage(&service, id, generation, WorkingArea::Unstaged, "in/a.txt")
            .await
            .unwrap_err(),
    );
    assert_eq!(tree(&f.root, true), before);
}

#[tokio::test]
async fn an_existing_index_lock_is_reported_and_left_alone() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    f.write("a.txt", "a2\n");
    let (service, id, generation) = trusted(&f).await;
    let changes = list(&service, id, generation).await;
    let lock = f.root.join(".git").join("index.lock");
    std::fs::write(&lock, b"held by another process").unwrap();
    let before = tree(&f.root, true);
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::IndexLocked));
    assert_eq!(
        tree(&f.root, true),
        before,
        "the lock or the index was touched"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn index_hooks_and_detached_head_are_blocked_without_writing() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    f.write("a.txt", "changed\n");
    let (service, id, generation) = trusted(&f).await;
    // A hook introduced after listing is caught by backend preflight as well.
    for custom in [false, true] {
        let changes = list(&service, id, generation).await;
        let hook_dir = if custom { "custom-hooks" } else { ".git/hooks" };
        if custom {
            f.git(&["config", "core.hooksPath", hook_dir]);
        }
        std::fs::create_dir_all(f.root.join(hook_dir)).unwrap();
        let hook = f.root.join(hook_dir).join("post-index-change");
        std::fs::write(&hook, "#!/bin/sh\necho hook > a.txt\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let before = tree(&f.root, true);
        let error = service
            .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
            .await
            .unwrap_err();
        assert!(
            matches!(error.code, ErrorCode::UnsupportedState),
            "{error:?}"
        );
        assert_eq!(tree(&f.root, true), before);
        assert!(list(&service, id, generation)
            .await
            .write_block
            .unwrap()
            .contains("post-index-change"));
        std::fs::remove_file(hook).unwrap();
    }
    f.git(&["checkout", "--detach", "-q"]);
    let changes = list(&service, id, generation).await;
    assert!(changes.write_block.as_ref().unwrap().contains("HEAD"));
    let before = tree(&f.root, true);
    let error = service
        .stage_file(id, generation, changes.revision, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::UnsupportedState));
    assert_eq!(tree(&f.root, true), before);
}

#[cfg(unix)]
#[tokio::test]
async fn parent_trust_never_executes_child_filters() {
    let child = Fixture::new();
    child.write("a.txt", "a\n");
    child.commit("child");
    let f = Fixture::new();
    f.write("seed", "seed\n");
    f.commit("parent");
    f.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        child.root.to_str().unwrap(),
        "sub",
    ]);
    f.commit("with child");
    let cmd = |args: &[&str]| {
        let out = f.command().args(["-C", "sub"]).args(args).output().unwrap();
        assert!(out.status.success(), "{:?}", out.stderr);
    };
    f.write("sub/.gitattributes", "*.txt filter=hostile\n");
    cmd(&[
        "config",
        "filter.hostile.clean",
        "sh -c 'echo ran > filter-ran; cat'",
    ]);
    f.write("sub/a.txt", "b\n");
    let (service, id, _) = trusted(&f).await;
    service.refresh(id).await.unwrap();
    assert!(!f.root.join("sub/filter-ran").exists());
    // Positive control: inspecting the child directly really invokes its filter.
    cmd(&["--no-optional-locks", "status", "--porcelain"]);
    assert!(f.root.join("sub/filter-ran").exists());
}
