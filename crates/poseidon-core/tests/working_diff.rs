use poseidon_core::{
    domain::{
        AppError, ChangeKind, DiffContent, DiffLine, DiffLineKind, DiffTarget, ErrorCode, FileDiff,
        WorkingArea, WorkingChanges, DIFF_LINE_LIMIT, DIFF_PAGE_LINES,
    },
    RepositoryService,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[path = "support/mod.rs"]
mod support;
use support::Fixture;

fn render(lines: &[DiffLine]) -> String {
    let mut out = String::new();
    for line in lines {
        let cr = if line.cr { "\r" } else { "" };
        match line.kind {
            DiffLineKind::Hunk => out.push_str(&line.text),
            DiffLineKind::Context => out.push_str(&format!(" {}{cr}", line.text)),
            DiffLineKind::Added => out.push_str(&format!("+{}{cr}", line.text)),
            DiffLineKind::Deleted => out.push_str(&format!("-{}{cr}", line.text)),
            DiffLineKind::Note => out.push_str(&format!("\\ {}", line.text)),
        }
        out.push('\n');
    }
    out
}

/// Oracle: Git's own patch text from the first hunk on. `--no-index` exits with 1
/// when the files differ.
fn git_hunks(f: &Fixture, args: &[&str]) -> String {
    let mut all = vec!["-c", "core.quotePath=false"];
    all.extend_from_slice(args);
    let output = f.output(&all);
    assert!(output.status.code().is_some_and(|c| c <= 1), "{args:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let start = text.find("\n@@ ").map(|i| i + 1).unwrap_or(text.len());
    text[start..].to_owned()
}

/// Every byte under the root, `.git` included: inspection must not write anything.
fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            let relative = entry.path().strip_prefix(root).unwrap().to_owned();
            if kind.is_dir() {
                walk(root, &entry.path(), files);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(entry.path()).unwrap();
                files.insert(relative, target.to_string_lossy().as_bytes().to_vec());
            } else {
                files.insert(relative, std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

async fn open(f: &Fixture) -> (RepositoryService, u32, u32) {
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    (service, opened.session_id, opened.generation)
}

async fn list(service: &RepositoryService, id: u32, generation: u32) -> WorkingChanges {
    service.working_changes(id, generation).await.unwrap().data
}

async fn diff(
    service: &RepositoryService,
    id: u32,
    generation: u32,
    changes: &WorkingChanges,
    area: WorkingArea,
    index: u32,
) -> Result<FileDiff, AppError> {
    service
        .working_diff(id, generation, changes.revision, area, index, 0, false)
        .await
        .map(|r| r.data)
}

fn names(files: &[poseidon_core::domain::ChangedFile]) -> Vec<&str> {
    files.iter().map(|f| f.path.as_str()).collect()
}

#[tokio::test]
async fn groups_kinds_and_patches_match_git() {
    let f = Fixture::new();
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    f.write("staged.txt", "a\nb\nc\n");
    f.write("partial.txt", "one\ntwo\nthree\n");
    f.write("removed.txt", "bye\n");
    f.write("worktree-deleted.txt", "gone\n");
    f.write("old-name.txt", &body);
    let head = f.commit("base");
    // Staged: modify, add, delete, rename. Partial: one change staged, another not.
    f.write("staged.txt", "a\nB\nc\n");
    f.write("added.txt", "new\n");
    f.git(&["rm", "-q", "removed.txt"]);
    f.git(&["mv", "old-name.txt", "new-name.txt"]);
    f.write("partial.txt", "ONE\ntwo\nthree\n");
    f.git(&["add", "staged.txt", "added.txt", "partial.txt"]);
    f.write("partial.txt", "ONE\ntwo\nTHREE\n");
    std::fs::remove_file(f.root.join("worktree-deleted.txt")).unwrap();
    // Untracked: text, CRLF without final newline, Unicode path, binary, empty, symlink.
    f.write("notes.md", "# notas\n\ntexto\n");
    f.write("crlf.ini", "a=1\r\nb=2");
    std::fs::create_dir(f.root.join("pasta ✓")).unwrap();
    f.write("pasta ✓/日本 \"quoted\".txt", "olá\n");
    std::fs::write(f.root.join("image.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
    f.write("empty.txt", "");
    #[cfg(unix)]
    std::os::unix::fs::symlink("notes.md", f.root.join("link")).unwrap();
    let before = tree_bytes(&f.root);
    let (service, id, generation) = open(&f).await;
    let changes = list(&service, id, generation).await;
    assert_eq!(changes.head_oid.as_deref(), Some(head.as_str()));

    // Lists against independent Git output.
    let staged = f.git(&["diff", "--cached", "-M", "--name-only", "-z"]);
    assert_eq!(
        names(&changes.staged),
        staged
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    );
    let unstaged = f.git(&["diff", "--name-only", "-z"]);
    assert_eq!(
        names(&changes.unstaged),
        unstaged
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    );
    let untracked = f.git(&["ls-files", "-z", "--others", "--exclude-standard"]);
    assert_eq!(
        names(&changes.untracked),
        untracked
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    );
    let kind = |files: &[poseidon_core::domain::ChangedFile], path: &str| {
        files.iter().find(|f| f.path == path).unwrap().kind
    };
    assert_eq!(kind(&changes.staged, "staged.txt"), ChangeKind::Modified);
    assert_eq!(kind(&changes.staged, "added.txt"), ChangeKind::Added);
    assert_eq!(kind(&changes.staged, "removed.txt"), ChangeKind::Deleted);
    assert_eq!(kind(&changes.staged, "new-name.txt"), ChangeKind::Renamed);
    assert_eq!(
        kind(&changes.unstaged, "worktree-deleted.txt"),
        ChangeKind::Deleted
    );
    assert!(changes
        .untracked
        .iter()
        .all(|f| f.kind == ChangeKind::Untracked));
    // Stats against numstat.
    for (area, args) in [
        (&changes.staged, vec!["diff", "--cached", "-M", "--numstat"]),
        (&changes.unstaged, vec!["diff", "--numstat"]),
    ] {
        let numstat = f.git(&args);
        for (file, line) in area.iter().zip(numstat.lines()) {
            let mut parts = line.split('\t');
            let added = parts.next().unwrap().parse().ok();
            let deleted = parts.next().unwrap().parse().ok();
            assert_eq!((file.additions, file.deletions), (added, deleted), "{line}");
        }
    }

    // Patches are byte-equal to Git's, per group.
    for file in &changes.staged {
        let got = diff(
            &service,
            id,
            generation,
            &changes,
            WorkingArea::Staged,
            file.index,
        )
        .await
        .unwrap();
        assert_eq!(
            got.target,
            DiffTarget::Working {
                revision: changes.revision,
                area: WorkingArea::Staged
            }
        );
        let mut paths = vec![file.path.as_str()];
        if let Some(old) = &file.old_path {
            paths.insert(0, old);
        }
        let mut args = vec![
            "diff",
            "--cached",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--",
        ];
        args.extend(paths);
        assert_eq!(render(&got.lines), git_hunks(&f, &args), "{}", file.path);
    }
    for file in &changes.unstaged {
        let got = diff(
            &service,
            id,
            generation,
            &changes,
            WorkingArea::Unstaged,
            file.index,
        )
        .await
        .unwrap();
        let args = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--",
            file.path.as_str(),
        ];
        assert_eq!(render(&got.lines), git_hunks(&f, &args), "{}", file.path);
    }
    for file in &changes.untracked {
        let got = diff(
            &service,
            id,
            generation,
            &changes,
            WorkingArea::Untracked,
            file.index,
        )
        .await
        .unwrap();
        match file.path.as_str() {
            "image.bin" => assert_eq!(got.content, DiffContent::Binary),
            "empty.txt" => assert_eq!(got.content, DiffContent::NoContent),
            "link" => {
                assert_eq!(
                    render(&got.lines),
                    "@@ -0,0 +1 @@\n+notes.md\n\\ No newline at end of file\n"
                );
            }
            path => {
                assert_eq!(got.content, DiffContent::Text, "{path}");
                let args = ["diff", "--no-index", "--no-color", "--", "/dev/null", path];
                assert_eq!(render(&got.lines), git_hunks(&f, &args), "{path}");
                assert_eq!(
                    got.new_size,
                    Some(std::fs::metadata(f.root.join(path)).unwrap().len())
                );
            }
        }
    }
    let crlf = changes
        .untracked
        .iter()
        .find(|f| f.path == "crlf.ini")
        .unwrap();
    let crlf = diff(
        &service,
        id,
        generation,
        &changes,
        WorkingArea::Untracked,
        crlf.index,
    )
    .await
    .unwrap();
    assert!(crlf.lines[1].cr && crlf.lines.last().unwrap().kind == DiffLineKind::Note);
    // Inspection wrote nothing, not even the index or a lock file.
    assert_eq!(tree_bytes(&f.root), before);
}

#[tokio::test]
async fn unborn_branch_compares_staged_files_with_the_empty_tree() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::format(format);
        f.write("first.txt", "first\nfile\n");
        f.git(&["add", "first.txt"]);
        f.write("loose.txt", "loose\n");
        let (service, id, generation) = open(&f).await;
        let changes = list(&service, id, generation).await;
        assert_eq!(changes.head_oid, None, "{format}");
        assert_eq!(names(&changes.staged), ["first.txt"]);
        assert_eq!(names(&changes.untracked), ["loose.txt"]);
        let got = diff(&service, id, generation, &changes, WorkingArea::Staged, 0)
            .await
            .unwrap();
        let args = ["diff", "--cached", "--no-color", "--", "first.txt"];
        assert_eq!(render(&got.lines), git_hunks(&f, &args), "{format}");
    }
}

#[tokio::test]
async fn external_changes_invalidate_listing_and_cached_patches() {
    let f = Fixture::new();
    f.write("file.txt", "one\n");
    f.write("other.txt", "x\n");
    f.commit("base");
    f.write("file.txt", "two\n");
    f.write("new.txt", "fresh\n");
    let (service, id, generation) = open(&f).await;
    let first = list(&service, id, generation).await;
    // Nothing changed: the same revision, so open diffs stay valid.
    assert_eq!(
        list(&service, id, generation).await.revision,
        first.revision
    );
    let page = diff(&service, id, generation, &first, WorkingArea::Unstaged, 0)
        .await
        .unwrap();
    assert_eq!(render(&page.lines), "@@ -1 +1 @@\n-one\n+two\n");

    // Worktree edit after the listing: the cached patch is not served.
    f.write("file.txt", "three\n");
    let error = diff(&service, id, generation, &first, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    let second = list(&service, id, generation).await;
    assert_ne!(second.revision, first.revision);
    // The old revision is no longer served, even for files that did not change.
    let error = diff(&service, id, generation, &first, WorkingArea::Untracked, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    let page = diff(&service, id, generation, &second, WorkingArea::Unstaged, 0)
        .await
        .unwrap();
    assert_eq!(render(&page.lines), "@@ -1 +1 @@\n-one\n+three\n");

    // Untracked edit and an external `git add` (index change).
    f.write("new.txt", "fresher\n");
    let error = diff(&service, id, generation, &second, WorkingArea::Untracked, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    let third = list(&service, id, generation).await;
    f.git(&["add", "file.txt"]);
    let error = diff(&service, id, generation, &third, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    let fourth = list(&service, id, generation).await;
    assert_eq!(names(&fourth.staged), ["file.txt"]);
    assert!(fourth.unstaged.is_empty());

    // An external commit moves HEAD: staged diffs of the old listing are refused.
    f.git(&["commit", "-q", "-m", "external"]);
    let error = diff(&service, id, generation, &fourth, WorkingArea::Staged, 0)
        .await
        .unwrap_err();
    assert!(matches!(error.code, ErrorCode::WorkingTreeChanged));
    let fifth = list(&service, id, generation).await;
    assert!(fifth.staged.is_empty());
    assert_eq!(fifth.head_oid.unwrap(), f.git(&["rev-parse", "HEAD"]));
}

#[tokio::test]
async fn stat_only_changes_and_intent_to_add_follow_git_status() {
    let f = Fixture::new();
    f.write("touched.txt", "same\n");
    f.commit("base");
    let file = std::fs::File::options()
        .write(true)
        .open(f.root.join("touched.txt"))
        .unwrap();
    file.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
        .unwrap();
    drop(file);
    f.write("intent.txt", "planned\n");
    f.git(&["add", "-N", "intent.txt"]);
    let (service, id, generation) = open(&f).await;
    let changes = list(&service, id, generation).await;
    assert_eq!(names(&changes.unstaged), ["intent.txt"]);
    assert_eq!(changes.unstaged[0].kind, ChangeKind::Added);
    let got = diff(&service, id, generation, &changes, WorkingArea::Unstaged, 0)
        .await
        .unwrap();
    let args = ["diff", "--no-color", "--", "intent.txt"];
    assert_eq!(render(&got.lines), git_hunks(&f, &args));
}

#[tokio::test]
async fn conflicts_are_listed_once_without_a_combined_patch() {
    let f = Fixture::new();
    f.write("conflict.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "other"]);
    f.write("conflict.txt", "other\n");
    f.commit("other");
    f.git(&["switch", "-q", "main"]);
    f.write("conflict.txt", "main\n");
    f.commit("main");
    let merge = f.output(&["merge", "other"]);
    assert!(!merge.status.success());
    let (service, id, generation) = open(&f).await;
    let changes = list(&service, id, generation).await;
    assert_eq!(names(&changes.unstaged), ["conflict.txt"]);
    assert_eq!(changes.unstaged[0].kind, ChangeKind::Unmerged);
    let got = diff(&service, id, generation, &changes, WorkingArea::Unstaged, 0)
        .await
        .unwrap();
    assert_eq!(got.content, DiffContent::NoContent);
}

#[tokio::test]
async fn configured_filters_never_run_during_inspection() {
    let f = Fixture::new();
    f.write("data.txt", "a\nb\n");
    f.commit("base");
    let marker = f.root.parent().unwrap().join("filter-ran");
    let script = format!("echo ran >> '{}'; cat", marker.display());
    f.write(".gitattributes", "*.txt filter=evil\n");
    f.git(&[
        "config",
        "filter.evil.clean",
        &format!("sh -c \"{script}\""),
    ]);
    f.git(&[
        "config",
        "filter.evil.process",
        &format!("sh -c \"{script}\""),
    ]);
    f.git(&["config", "filter.evil.required", "true"]);
    f.write("data.txt", "a\nchanged\n");
    let (service, id, generation) = open(&f).await;
    let changes = list(&service, id, generation).await;
    let file = changes
        .unstaged
        .iter()
        .find(|f| f.path == "data.txt")
        .unwrap();
    let got = diff(
        &service,
        id,
        generation,
        &changes,
        WorkingArea::Unstaged,
        file.index,
    )
    .await
    .unwrap();
    assert_eq!(render(&got.lines), "@@ -1,2 +1,2 @@\n a\n-b\n+changed\n");
    service.refresh(id).await.unwrap();
    assert!(
        !marker.exists(),
        "a configured filter ran during inspection"
    );
}

#[tokio::test]
async fn large_untracked_files_are_gated_and_paged() {
    let f = Fixture::new();
    f.write("seed.txt", "seed\n");
    f.commit("seed");
    let big: String = (0..30_000).map(|i| format!("{i:0>100}\n")).collect();
    f.write("big.log", &big);
    let (service, id, generation) = open(&f).await;
    let changes = list(&service, id, generation).await;
    let gated = diff(
        &service,
        id,
        generation,
        &changes,
        WorkingArea::Untracked,
        0,
    )
    .await
    .unwrap();
    assert_eq!(gated.content, DiffContent::Large);
    assert!(gated.lines.is_empty());
    let first = service
        .working_diff(
            id,
            generation,
            changes.revision,
            WorkingArea::Untracked,
            0,
            0,
            true,
        )
        .await
        .unwrap()
        .data;
    assert_eq!(first.total_lines as usize, 30_001);
    assert_eq!(first.lines.len(), DIFF_PAGE_LINES);
    assert!(!first.truncated);
    let last = service
        .working_diff(
            id,
            generation,
            changes.revision,
            WorkingArea::Untracked,
            0,
            30_000,
            true,
        )
        .await
        .unwrap()
        .data;
    assert_eq!(last.lines[0].text, format!("{:0>100}", 29_999));
    assert_eq!(last.lines[0].new_line, Some(30_000));
    // Below the soft gate but above the default line budget: truncated, not silent.
    let lines: String = (0..25_000).map(|i| format!("{i}\n")).collect();
    f.write("many.txt", &lines);
    let changes = list(&service, id, generation).await;
    let many = changes
        .untracked
        .iter()
        .find(|f| f.path == "many.txt")
        .unwrap();
    let got = diff(
        &service,
        id,
        generation,
        &changes,
        WorkingArea::Untracked,
        many.index,
    )
    .await
    .unwrap();
    assert!(got.truncated);
    assert_eq!(got.total_lines as usize, DIFF_LINE_LIMIT);
}

#[tokio::test]
async fn invalid_requests_are_rejected() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("a");
    f.write("a.txt", "b\n");
    let (service, id, generation) = open(&f).await;
    let changes = list(&service, id, generation).await;
    let out_of_range = diff(&service, id, generation, &changes, WorkingArea::Unstaged, 7)
        .await
        .unwrap_err();
    assert!(matches!(out_of_range.code, ErrorCode::InvalidInput));
    let unknown = service
        .working_diff(
            id,
            generation,
            changes.revision + 50,
            WorkingArea::Unstaged,
            0,
            0,
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(unknown.code, ErrorCode::WorkingTreeChanged));
    let stale = service
        .working_changes(id, generation + 1)
        .await
        .unwrap_err();
    assert!(matches!(stale.code, ErrorCode::StaleSession));
    // A refresh makes earlier listings of the session unreachable.
    let refreshed = service.refresh(id).await.unwrap();
    let old = diff(&service, id, generation, &changes, WorkingArea::Unstaged, 0)
        .await
        .unwrap_err();
    assert!(matches!(old.code, ErrorCode::StaleSession));
    let fresh = list(&service, id, refreshed.generation).await;
    assert!(diff(
        &service,
        id,
        refreshed.generation,
        &fresh,
        WorkingArea::Unstaged,
        0
    )
    .await
    .is_ok());
}
