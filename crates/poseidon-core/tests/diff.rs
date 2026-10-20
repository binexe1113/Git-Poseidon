use poseidon_core::{
    domain::{
        ChangeKind, DiffBase, DiffContent, DiffLine, DiffLineKind, DiffTarget, ErrorCode,
        DIFF_LINE_LIMIT, DIFF_PAGE_LINES,
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

async fn open(f: &Fixture) -> (RepositoryService, u32, u32) {
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await.unwrap();
    (service, opened.session_id, opened.generation)
}

/// Rebuilds patch text from parsed lines, to compare with `git diff-tree -p`.
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

/// Oracle: Git's own patch for one path, from the first hunk on.
fn git_hunks(f: &Fixture, range: &[&str], paths: &[&str]) -> String {
    let mut args = vec!["diff-tree", "-p", "-r", "-M", "--no-color", "--no-ext-diff"];
    args.extend_from_slice(range);
    args.push("--");
    args.extend_from_slice(paths);
    let output = f.output(&args);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let start = text.find("\n@@ ").map(|i| i + 1).unwrap_or(text.len());
    text[start..].to_owned()
}

#[tokio::test]
async fn root_commit_is_compared_with_the_empty_tree() {
    let f = Fixture::scenario(support::Scenario::Initial);
    let head = f.git(&["rev-parse", "HEAD"]);
    let (service, id, generation) = open(&f).await;
    let files = service
        .commit_files(id, generation, &head)
        .await
        .unwrap()
        .data;
    assert_eq!(files.base, DiffBase::EmptyTree);
    assert_eq!(files.files.len(), 1);
    let file = &files.files[0];
    assert_eq!(file.kind, ChangeKind::Added);
    assert_eq!(file.path, "arquivo com espaços 日本.txt");
    assert_eq!((file.additions, file.deletions), (Some(1), Some(0)));
    let diff = service
        .file_diff(id, generation, &head, 0, 0, false)
        .await
        .unwrap()
        .data;
    assert_eq!(diff.content, DiffContent::Text);
    assert_eq!(
        render(&diff.lines),
        git_hunks(&f, &["--root", &head], &["arquivo com espaços 日本.txt"])
    );
    assert_eq!(diff.lines[1].new_line, Some(1));
    assert_eq!(diff.lines[1].old_line, None);
}

#[tokio::test]
async fn kinds_stats_and_patches_match_git() {
    let f = Fixture::new();
    let body: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    f.write("modified.txt", "a\nb\nc\nd\n");
    f.write("deleted.txt", "bye\n");
    f.write("before-rename.txt", &body);
    f.write("crlf.txt", "one\r\ntwo\r\n");
    f.write("script.sh", "echo hi\n");
    f.commit("base");
    f.write("modified.txt", "a\nB\nc\nd\nend");
    std::fs::remove_file(f.root.join("deleted.txt")).unwrap();
    std::fs::remove_file(f.root.join("before-rename.txt")).unwrap();
    f.write("after-rename.txt", &(body.clone() + "line 41\n"));
    f.write("added.txt", "new\n");
    f.write("crlf.txt", "one\r\nTWO\r\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = f.root.join("script.sh");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let head = f.commit("changes");
    let parent = f.git(&["rev-parse", "HEAD^"]);
    let before = all_files(&f.root);
    let (service, id, generation) = open(&f).await;
    let listed = service
        .commit_files(id, generation, &head)
        .await
        .unwrap()
        .data;
    assert!(
        matches!(listed.base, DiffBase::FirstParent { ref oid, parent_count: 1 } if *oid == parent)
    );
    // Oracle: independent name-status and numstat output of the Git CLI.
    let status = f.git(&["diff-tree", "-r", "-M", "--name-status", &parent, &head]);
    let numstat = f.git(&["diff-tree", "-r", "-M", "--numstat", &parent, &head]);
    assert_eq!(listed.files.len(), status.lines().count());
    for (file, (status_line, stat_line)) in
        listed.files.iter().zip(status.lines().zip(numstat.lines()))
    {
        let fields: Vec<&str> = status_line.split('\t').collect();
        let expected = match &fields[0][..1] {
            "A" => ChangeKind::Added,
            "M" => ChangeKind::Modified,
            "D" => ChangeKind::Deleted,
            "R" => ChangeKind::Renamed,
            other => panic!("unexpected status {other}"),
        };
        assert_eq!(file.kind, expected, "{status_line}");
        assert_eq!(file.path, *fields.last().unwrap());
        let stats: Vec<&str> = stat_line.split('\t').collect();
        assert_eq!(file.additions, stats[0].parse().ok());
        assert_eq!(file.deletions, stats[1].parse().ok());
    }
    let renamed = listed
        .files
        .iter()
        .find(|f| f.kind == ChangeKind::Renamed)
        .unwrap();
    assert_eq!(renamed.old_path.as_deref(), Some("before-rename.txt"));
    #[cfg(unix)]
    {
        let script = listed.files.iter().find(|f| f.path == "script.sh").unwrap();
        assert_eq!(
            (script.old_mode.as_str(), script.new_mode.as_str()),
            ("100644", "100755")
        );
    }
    for file in &listed.files {
        let diff = service
            .file_diff(id, generation, &head, file.index, 0, false)
            .await
            .unwrap()
            .data;
        let mut paths = vec![file.path.as_str()];
        if let Some(old) = &file.old_path {
            paths.insert(0, old);
        }
        assert_eq!(
            render(&diff.lines),
            git_hunks(&f, &[&parent, &head], &paths),
            "{}",
            file.path
        );
        match file.path.as_str() {
            "script.sh" => assert_eq!(diff.content, DiffContent::NoContent),
            "crlf.txt" => assert!(diff.lines.iter().any(|l| l.cr && l.text == "TWO")),
            "modified.txt" => assert!(diff.lines.iter().any(|l| l.kind == DiffLineKind::Note)),
            _ => {}
        }
    }
    assert_eq!(before, all_files(&f.root), "diff inspection changed files");
}

#[tokio::test]
async fn merge_is_compared_with_its_first_parent() {
    let f = Fixture::new();
    f.write("base.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-c", "feature"]);
    f.write("feature.txt", "feature\n");
    f.commit("feature");
    f.git(&["switch", "main"]);
    f.write("main.txt", "main\n");
    f.commit("main");
    f.git(&["merge", "--no-ff", "-m", "merge feature", "feature"]);
    let merge = f.git(&["rev-parse", "HEAD"]);
    let first = f.git(&["rev-parse", "HEAD^1"]);
    let (service, id, generation) = open(&f).await;
    let listed = service
        .commit_files(id, generation, &merge)
        .await
        .unwrap()
        .data;
    assert!(
        matches!(listed.base, DiffBase::FirstParent { ref oid, parent_count: 2 } if *oid == first)
    );
    let expected = f.git(&["diff-tree", "-r", "--name-only", &first, &merge]);
    let names: Vec<&str> = listed.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, expected.lines().collect::<Vec<_>>());
    assert_eq!(names, ["feature.txt"]);
}

#[tokio::test]
async fn special_and_unicode_paths_are_listed_and_diffed_by_index() {
    let f = Fixture::new();
    f.write("seed.txt", "seed\n");
    f.commit("seed");
    let names = [
        "espaço e 日本語 ✓.md",
        "tab\there.txt",
        "quote\"and'apostrophe.txt",
        "new\nline.txt",
        "-starts-with-dash.txt",
        "glob*star?.txt",
        "globXstarY.txt",
        ":(magic).txt",
    ];
    for (i, name) in names.iter().enumerate() {
        f.write(name, &format!("content {i}\n"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::write(
            f.root
                .join(std::ffi::OsStr::from_bytes(b"invalid-\xff-utf8.txt")),
            "raw bytes\n",
        )
        .unwrap();
    }
    let head = f.commit("special");
    let (service, id, generation) = open(&f).await;
    let listed = service
        .commit_files(id, generation, &head)
        .await
        .unwrap()
        .data;
    let expected = names.len() + usize::from(cfg!(unix));
    assert_eq!(listed.files.len(), expected);
    for file in &listed.files {
        assert_eq!(file.kind, ChangeKind::Added, "{}", file.path);
        let diff = service
            .file_diff(id, generation, &head, file.index, 0, false)
            .await
            .unwrap()
            .data;
        // Literal pathspecs: each request returns exactly this file's single line.
        let added: Vec<&str> = diff
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Added)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(added.len(), 1, "{}", file.path);
        if let Some(i) = names
            .iter()
            .position(|n| n.replace('\t', "\\t").replace('\n', "\\n") == file.path)
        {
            assert_eq!(added[0], format!("content {i}"));
        } else {
            assert_eq!(file.path, "invalid-\\xff-utf8.txt");
            assert_eq!(added[0], "raw bytes");
        }
    }
}

#[tokio::test]
async fn binary_files_report_sizes_without_text() {
    let f = Fixture::new();
    std::fs::write(f.root.join("image.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
    let head = f.commit("binary");
    let (service, id, generation) = open(&f).await;
    let listed = service
        .commit_files(id, generation, &head)
        .await
        .unwrap()
        .data;
    assert!(listed.files[0].binary);
    assert_eq!(listed.files[0].additions, None);
    let diff = service
        .file_diff(id, generation, &head, 0, 0, false)
        .await
        .unwrap()
        .data;
    assert_eq!(diff.content, DiffContent::Binary);
    assert!(diff.lines.is_empty());
    assert_eq!((diff.old_size, diff.new_size), (None, Some(6)));
}

#[tokio::test]
async fn large_diffs_are_paged_truncated_and_gated() {
    let f = Fixture::new();
    f.write("seed.txt", "seed\n");
    f.commit("seed");
    let lines = 30_000;
    let big: String = (0..lines).map(|i| format!("row {i}\n")).collect();
    f.write("big.txt", &big);
    // 3 MiB: above the 2 MiB soft limit, below the hard limit.
    f.write("huge.txt", &("x".repeat(79) + "\n").repeat(40_000));
    let head = f.commit("large");
    let (service, id, generation) = open(&f).await;
    let listed = service
        .commit_files(id, generation, &head)
        .await
        .unwrap()
        .data;
    let big_index = listed
        .files
        .iter()
        .find(|f| f.path == "big.txt")
        .unwrap()
        .index;
    let huge_index = listed
        .files
        .iter()
        .find(|f| f.path == "huge.txt")
        .unwrap()
        .index;

    let first = service
        .file_diff(id, generation, &head, big_index, 0, false)
        .await
        .unwrap()
        .data;
    assert!(first.truncated);
    assert_eq!(first.total_lines as usize, DIFF_LINE_LIMIT);
    assert_eq!(first.lines.len(), DIFF_PAGE_LINES);
    let last = service
        .file_diff(
            id,
            generation,
            &head,
            big_index,
            (DIFF_LINE_LIMIT - 10) as u32,
            false,
        )
        .await
        .unwrap()
        .data;
    assert_eq!(last.lines.len(), 10);
    // Line numbers continue across pages: header is line 0, "row i" is new line i+1.
    assert_eq!(last.lines[0].new_line, Some((DIFF_LINE_LIMIT - 10) as u32));
    assert_eq!(last.lines[0].text, format!("row {}", DIFF_LINE_LIMIT - 11));

    let full = service
        .file_diff(id, generation, &head, big_index, 0, true)
        .await
        .unwrap()
        .data;
    assert!(!full.truncated);
    assert_eq!(full.total_lines as usize, lines + 1);

    let gated = service
        .file_diff(id, generation, &head, huge_index, 0, false)
        .await
        .unwrap()
        .data;
    assert_eq!(gated.content, DiffContent::Large);
    assert!(gated.lines.is_empty());
    assert!(gated.new_size.unwrap() > 2 * 1024 * 1024);
    let opened = service
        .file_diff(id, generation, &head, huge_index, 0, true)
        .await
        .unwrap()
        .data;
    assert_eq!(opened.content, DiffContent::Text);
    assert_eq!(opened.total_lines, 40_001);
}

#[tokio::test]
async fn invalid_requests_and_git_failures_are_reported() {
    let f = Fixture::scenario(support::Scenario::Linear);
    let head = f.git(&["rev-parse", "HEAD"]);
    let tree = f.git(&["rev-parse", "HEAD^{tree}"]);
    let (service, id, generation) = open(&f).await;
    let error = |oid: &str| {
        let service = &service;
        let oid = oid.to_owned();
        async move {
            service
                .commit_files(id, generation, &oid)
                .await
                .unwrap_err()
                .code
        }
    };
    assert!(matches!(error("not-an-oid").await, ErrorCode::InvalidInput));
    // Well-formed but absent object, and an object that is not a commit: Git fails.
    assert!(matches!(
        error(&"f".repeat(40)).await,
        ErrorCode::CommandFailed
    ));
    assert!(matches!(error(&tree).await, ErrorCode::CommandFailed));
    assert!(matches!(
        service
            .commit_files(id, generation + 1, &head)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    ));
    assert!(matches!(
        service
            .file_diff(id, generation, &head, 99, 0, false)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    ));
}

#[tokio::test]
async fn a_newer_selection_supersedes_the_running_diff() {
    let f = Fixture::new();
    let big: String = (0..150_000).map(|i| format!("row {i}\n")).collect();
    f.write("big.txt", &big);
    let first = f.commit("big");
    f.write("small.txt", "small\n");
    let second = f.commit("small");
    let (service, id, generation) = open(&f).await;
    service.commit_files(id, generation, &first).await.unwrap();
    let service = std::sync::Arc::new(service);
    let slow = {
        let service = service.clone();
        let first = first.clone();
        tokio::spawn(async move { service.file_diff(id, generation, &first, 0, 0, true).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let latest = service.commit_files(id, generation, &second).await.unwrap();
    assert_eq!(latest.data.oid, second);
    match slow.await.unwrap() {
        Ok(page) => assert_eq!(page.data.target, DiffTarget::Commit { oid: first }),
        Err(error) => assert!(matches!(error.code, ErrorCode::Cancelled)),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_report_mode_and_target_as_text() {
    let f = Fixture::new();
    f.write("target.txt", "x\n");
    std::os::unix::fs::symlink("target.txt", f.root.join("link")).unwrap();
    let head = f.commit("symlink");
    let (service, id, generation) = open(&f).await;
    let listed = service
        .commit_files(id, generation, &head)
        .await
        .unwrap()
        .data;
    let link = listed.files.iter().find(|f| f.path == "link").unwrap();
    assert_eq!(link.new_mode, "120000");
    let diff = service
        .file_diff(id, generation, &head, link.index, 0, false)
        .await
        .unwrap()
        .data;
    assert_eq!(
        render(&diff.lines),
        git_hunks(&f, &["--root", &head], &["link"])
    );
    assert!(diff
        .lines
        .iter()
        .any(|l| l.kind == DiffLineKind::Added && l.text == "target.txt"));
}
