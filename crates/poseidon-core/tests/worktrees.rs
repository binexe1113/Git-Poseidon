//! T23-B1: worktree List / Inspect / Open are observational and identity-checked.
#![cfg(unix)]
use poseidon_core::{domain::*, RepositoryService};
use std::{
    ffi::OsStr,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;

const FORMATS: [&str; 2] = ["sha1", "sha256"];

fn base(format: &str) -> Fixture {
    let f = Fixture::format(format);
    f.write("a.txt", "base\n");
    f.write(".gitignore", "ignored/\n*.log\n");
    f.commit("base");
    for b in ["feature", "spare", "other"] {
        f.git(&["branch", b]);
    }
    f
}
fn add(f: &Fixture, path: &Path, args: &[&str]) {
    let mut c = f.command();
    c.args(["worktree", "add", "-q"])
        .args(args.iter().take(args.len().saturating_sub(1)));
    c.arg(path);
    if let Some(last) = args.last() {
        c.arg(last);
    }
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn git_in(f: &Fixture, dir: &Path, args: &[&str]) -> std::process::Output {
    let mut c = f.command();
    c.current_dir(dir).args(args).output().unwrap()
}
async fn open(s: &RepositoryService, p: &Path) -> (u32, u32) {
    let r = s.open(p).await.unwrap();
    (r.session_id, r.generation)
}
fn find<'a>(list: &'a WorktreeList, suffix: &str) -> &'a Worktree {
    list.worktrees
        .iter()
        .find(|w| w.path.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "{suffix} not in {:?}",
                list.worktrees.iter().map(|w| &w.path).collect::<Vec<_>>()
            )
        })
}
fn code<T>(r: &Result<T>) -> Option<String> {
    r.as_ref().err().map(|e| format!("{:?}", e.code))
}

/// Bytes and metadata of every entry under `root` (symlinks not followed), so a
/// write, a transient lock or a rewritten file shows up as a difference.
fn snapshot(root: &Path, skip: &Path) -> Vec<(Vec<u8>, String)> {
    fn walk(dir: &Path, root: &Path, skip: &Path, out: &mut Vec<(Vec<u8>, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p == skip {
                continue;
            }
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec();
            let mut desc = format!(
                "mode={:o} ino={} len={} mtime={}.{} ctime={}.{}",
                m.mode(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec()
            );
            if m.file_type().is_symlink() {
                desc.push_str(&format!(" -> {:?}", std::fs::read_link(&p).unwrap()));
            } else if m.is_file() {
                desc.push_str(&format!(" {:?}", std::fs::read(&p).unwrap()));
            }
            out.push((rel, desc));
            if m.is_dir() {
                walk(&p, root, skip, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, skip, &mut out);
    out
}

/// A repository with a main and four linked worktrees, hostile configuration and
/// harmless marker programs everywhere Git could run something.
struct Hostile {
    f: Fixture,
    markers: PathBuf,
}
fn hostile(format: &str) -> Hostile {
    let f = base(format);
    let markers = f.temp.path().join("markers");
    std::fs::create_dir(&markers).unwrap();
    let mark = |name: &str| format!("sh -c ': > \"{}/{name}\"; cat'", markers.display());
    let linked = f.temp.path().join("wt linked 日本 ; $(x)");
    add(&f, &linked, &["feature"]);
    add(
        &f,
        &f.temp.path().join("wt-detached"),
        &["--detach", "HEAD"],
    );
    add(&f, &f.temp.path().join("wt-locked"), &["spare"]);
    assert!(f
        .output(&[
            "worktree",
            "lock",
            "--reason",
            "usb disk",
            f.temp.path().join("wt-locked").to_str().unwrap()
        ])
        .status
        .success());
    add(&f, &f.temp.path().join("wt-gone"), &["other"]);
    std::fs::remove_dir_all(f.temp.path().join("wt-gone")).unwrap();
    std::fs::write(linked.join("staged.txt"), "s\n").unwrap();
    assert!(git_in(&f, &linked, &["add", "staged.txt"]).status.success());
    for (key, value) in [
        ("filter.mark.clean", mark("clean")),
        ("filter.mark.smudge", mark("smudge")),
        ("filter.mark.process", mark("process")),
        ("extensions.worktreeConfig", "true".into()),
    ] {
        f.git(&["config", key, &value]);
    }
    let include = f.temp.path().join("include.config");
    let hooks = f.temp.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    std::fs::write(
        &include,
        format!(
            "[core]\n\tfsmonitor = {}\n\thooksPath = {}\n",
            mark("fsmonitor"),
            hooks.display()
        ),
    )
    .unwrap();
    f.git(&["config", "include.path", include.to_str().unwrap()]);
    assert!(git_in(
        &f,
        &linked,
        &[
            "config",
            "--worktree",
            "filter.wtonly.clean",
            &mark("wtonly")
        ]
    )
    .status
    .success());
    for dir in [f.root.join(".git/hooks"), hooks] {
        for hook in [
            "post-checkout",
            "reference-transaction",
            "post-index-change",
            "pre-commit",
            "pre-auto-gc",
            "post-rewrite",
        ] {
            let p = dir.join(hook);
            std::fs::write(
                &p,
                format!("#!/bin/sh\n: > \"{}/hook-{hook}\"\n", markers.display()),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
    }
    for wt in [
        f.root.clone(),
        linked.clone(),
        f.temp.path().join("wt-detached"),
    ] {
        std::fs::write(
            wt.join(".gitattributes"),
            "*.txt filter=mark\n*.dat filter=wtonly\n",
        )
        .unwrap();
        std::fs::write(wt.join("a.txt"), "modified\n").unwrap();
        std::fs::write(wt.join("untracked.txt"), "u\n").unwrap();
        std::fs::write(wt.join("x.dat"), "d\n").unwrap();
        std::fs::create_dir_all(wt.join("ignored")).unwrap();
        std::fs::write(wt.join("ignored/keep.log"), "ignored\n").unwrap();
    }
    Hostile { f, markers }
}

#[tokio::test]
async fn list_inspect_and_open_are_observational_under_hostile_configuration() {
    for format in FORMATS {
        for trusted in [false, true] {
            let h = hostile(format);
            let f = &h.f;
            let s = RepositoryService::default();
            let (id, g) = open(&s, &f.root).await;
            if trusted {
                s.set_trust(id, g, true).await.unwrap();
            }
            let before = snapshot(f.temp.path(), &h.markers);
            let list = s.worktrees(id, g).await.unwrap().data;
            assert_eq!(list.worktrees.len(), 5);
            for w in &list.worktrees {
                let status = s.worktree_status(id, g, list.revision, w.key).await;
                assert_eq!(
                    status.is_ok(),
                    w.availability == WorktreeAvailability::Available,
                    "{} {:?}",
                    w.path,
                    code(&status)
                );
            }
            let linked = find(&list, "$(x)").key;
            let opened = s.open_worktree(id, g, list.revision, linked).await.unwrap();
            let (lid, lg) = (opened.session_id, opened.generation);
            let list = s.worktrees(lid, lg).await.unwrap().data;
            let main = list
                .worktrees
                .iter()
                .find(|w| w.kind == WorktreeKind::Main)
                .unwrap()
                .key;
            s.open_worktree(lid, lg, list.revision, main).await.unwrap();
            let after = snapshot(f.temp.path(), &h.markers);
            assert_eq!(
                before.len(),
                after.len(),
                "{format} trusted={trusted}: entries appeared or vanished"
            );
            for (b, a) in before.iter().zip(&after) {
                assert_eq!(
                    b,
                    a,
                    "{format} trusted={trusted}: {}",
                    String::from_utf8_lossy(&b.0)
                );
            }
            let ran: Vec<_> = std::fs::read_dir(&h.markers)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert!(
                ran.is_empty(),
                "{format} trusted={trusted}: programs ran: {ran:?}"
            );
            // Positive control: plain native status in the same setup does run them,
            // so the empty marker directory above is a real observation.
            git_in(
                f,
                &f.temp.path().join("wt linked 日本 ; $(x)"),
                &["status", "--porcelain"],
            );
            assert!(
                std::fs::read_dir(&h.markers).unwrap().next().is_some(),
                "{format}: control did not run"
            );
        }
    }
}

#[tokio::test]
async fn lists_kinds_states_and_raw_paths() {
    for format in FORMATS {
        let f = base(format);
        let newline = f.temp.path().join("wt new\nline");
        add(&f, &newline, &["feature"]);
        let raw = f.temp.path().join(OsStr::from_bytes(b"wt-\xff-raw"));
        add(&f, &raw, &["--detach", "HEAD"]);
        add(&f, &f.temp.path().join("wt-locked-plain"), &["spare"]);
        assert!(f
            .output(&[
                "worktree",
                "lock",
                f.temp.path().join("wt-locked-plain").to_str().unwrap()
            ])
            .status
            .success());
        // The marker `git worktree add` keeps while it runs (and leaves on SIGKILL).
        add(&f, &f.temp.path().join("wt-init"), &["other"]);
        std::fs::write(f.root.join(".git/worktrees/wt-init/locked"), "initializing").unwrap();
        // A .git link pointing at another repository's admin directory.
        let other = base(format);
        add(
            &other,
            &other.temp.path().join("other-linked"),
            &["feature"],
        );
        f.git(&["branch", "swapped"]);
        let swapped = f.temp.path().join("wt-swapped");
        add(&f, &swapped, &["swapped"]);
        std::fs::write(
            swapped.join(".git"),
            std::fs::read(other.temp.path().join("other-linked/.git")).unwrap(),
        )
        .unwrap();
        let s = RepositoryService::default();
        let (id, g) = open(&s, &f.root).await;
        let list = s.worktrees(id, g).await.unwrap().data;
        assert!(!list.truncated);
        let main = &list.worktrees[0];
        assert_eq!(
            (main.kind, main.current, main.branch.as_deref()),
            (WorktreeKind::Main, true, Some("main"))
        );
        let nl = find(&list, "wt new\\nline");
        assert_eq!(
            (nl.kind, nl.branch.as_deref(), nl.availability),
            (
                WorktreeKind::Linked,
                Some("feature"),
                WorktreeAvailability::Available
            )
        );
        let r = find(&list, "wt-\\xff-raw");
        assert!(
            r.detached
                && r.branch.is_none()
                && r.head
                    .as_ref()
                    .is_some_and(|h| h.len() == if format == "sha1" { 40 } else { 64 })
        );
        let l = find(&list, "wt-locked-plain");
        assert!(
            l.locked
                && l.lock_reason.is_none()
                && l.availability == WorktreeAvailability::Available
        );
        assert_eq!(
            find(&list, "wt-init").availability,
            WorktreeAvailability::Initializing
        );
        assert_eq!(
            find(&list, "wt-swapped").availability,
            WorktreeAvailability::Inconsistent
        );
        for suffix in ["wt-init", "wt-swapped"] {
            let refused = s
                .open_worktree(id, g, list.revision, find(&list, suffix).key)
                .await;
            assert_eq!(code(&refused), Some("UnsupportedState".into()), "{suffix}");
        }
        // Opening by raw bytes works and lands on that exact worktree.
        let opened = s.open_worktree(id, g, list.revision, r.key).await.unwrap();
        assert!(matches!(opened.data.head, Head::Detached { .. }));
        let list = s
            .worktrees(opened.session_id, opened.generation)
            .await
            .unwrap()
            .data;
        assert!(find(&list, "wt-\\xff-raw").current);
    }
}

#[tokio::test]
async fn bare_main_and_inaccessible_entries() {
    for format in FORMATS {
        let f = base(format);
        let bare = f.temp.path().join("bare.git");
        assert!(f
            .output(&[
                "clone",
                "-q",
                "--bare",
                f.root.to_str().unwrap(),
                bare.to_str().unwrap()
            ])
            .status
            .success());
        let linked = f.temp.path().join("bare-linked");
        let out = git_in(
            &f,
            &bare,
            &["worktree", "add", "-q", linked.to_str().unwrap(), "feature"],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let closed = f.temp.path().join("bare-closed");
        assert!(git_in(
            &f,
            &bare,
            &["worktree", "add", "-q", closed.to_str().unwrap(), "spare"]
        )
        .status
        .success());
        let s = RepositoryService::default();
        let (id, g) = open(&s, &linked).await;
        std::fs::set_permissions(&closed, std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .unwrap();
        let list = s.worktrees(id, g).await.unwrap().data;
        std::fs::set_permissions(&closed, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let main = &list.worktrees[0];
        assert_eq!(
            (main.kind, main.availability),
            (WorktreeKind::Bare, WorktreeAvailability::Available)
        );
        assert!(find(&list, "bare-linked").current);
        assert_eq!(
            find(&list, "bare-closed").availability,
            WorktreeAvailability::Inaccessible
        );
        assert_eq!(
            code(&s.worktree_status(id, g, list.revision, main.key).await),
            Some("UnsupportedState".into())
        );
        // Restoring access changed the registry the listing described.
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, main.key).await),
            Some("StaleSession".into())
        );
        let list = s.worktrees(id, g).await.unwrap().data;
        assert_eq!(
            find(&list, "bare-closed").availability,
            WorktreeAvailability::Available
        );
        let opened = s.open_worktree(id, g, list.revision, 0).await.unwrap();
        assert!(opened.data.bare);
    }
}

#[tokio::test]
async fn open_main_linked_main_keeps_each_head_and_index() {
    for format in FORMATS {
        let f = base(format);
        let linked = f.temp.path().join("wt-feature");
        add(&f, &linked, &["feature"]);
        std::fs::write(f.root.join("main-only.txt"), "m\n").unwrap();
        std::fs::write(linked.join("linked-only.txt"), "l\n").unwrap();
        let s = RepositoryService::default();
        let (id, g) = open(&s, &f.root).await;
        let list = s.worktrees(id, g).await.unwrap().data;
        let opened = s
            .open_worktree(id, g, list.revision, find(&list, "wt-feature").key)
            .await
            .unwrap();
        let (lid, lg) = (opened.session_id, opened.generation);
        assert_ne!(lid, id);
        assert!(matches!(&opened.data.head, Head::Attached { name, .. } if name == "feature"));
        assert_eq!(
            opened.data.git_dir,
            f.root
                .join(".git/worktrees/wt-feature")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
        );
        let w = s.working_changes(lid, lg).await.unwrap().data;
        assert!(w.untracked.iter().any(|c| c.path == "linked-only.txt"));
        assert!(!w.untracked.iter().any(|c| c.path == "main-only.txt"));
        // The previous session and its listing are gone.
        assert_eq!(
            code(&s.history_page(id, g, 0).await),
            Some("StaleSession".into())
        );
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, 0).await),
            Some("StaleSession".into())
        );
        let list = s.worktrees(lid, lg).await.unwrap().data;
        assert!(find(&list, "wt-feature").current && !list.worktrees[0].current);
        let back = s.open_worktree(lid, lg, list.revision, 0).await.unwrap();
        assert!(matches!(&back.data.head, Head::Attached { name, .. } if name == "main"));
        let w = s
            .working_changes(back.session_id, back.generation)
            .await
            .unwrap()
            .data;
        assert!(w.untracked.iter().any(|c| c.path == "main-only.txt"));
        // Opening never switched anything.
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), "refs/heads/main");
        assert_eq!(
            String::from_utf8_lossy(&git_in(&f, &linked, &["symbolic-ref", "HEAD"]).stdout).trim(),
            "refs/heads/feature"
        );
    }
}

#[tokio::test]
async fn stale_selections_are_refused_without_replacing_the_session() {
    for format in FORMATS {
        let f = base(format);
        let a = f.temp.path().join("wt-a");
        add(&f, &a, &["feature"]);
        let s = RepositoryService::default();
        let (id, g) = open(&s, &f.root).await;
        let list = s.worktrees(id, g).await.unwrap().data;
        let key = find(&list, "wt-a").key;
        // Wrong revision, wrong key.
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision + 1, key).await),
            Some("StaleSession".into())
        );
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, 99).await),
            Some("InvalidInput".into())
        );
        // Registry changed: another worktree registered after the listing.
        add(&f, &f.temp.path().join("wt-new"), &["spare"]);
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
        assert_eq!(
            code(&s.worktree_status(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
        // HEAD moves in another worktree do not invalidate the selection.
        let list = s.worktrees(id, g).await.unwrap().data;
        std::fs::write(a.join("c.txt"), "c\n").unwrap();
        assert!(git_in(&f, &a, &["add", "c.txt"]).status.success());
        assert!(git_in(&f, &a, &["commit", "-qm", "c"]).status.success());
        assert!(s
            .worktree_status(id, g, list.revision, find(&list, "wt-a").key)
            .await
            .is_ok());
        // Removed between List and Open.
        let list = s.worktrees(id, g).await.unwrap().data;
        assert!(f
            .output(&[
                "worktree",
                "remove",
                "--force",
                f.temp.path().join("wt-new").to_str().unwrap()
            ])
            .status
            .success());
        assert_eq!(
            code(
                &s.open_worktree(id, g, list.revision, find(&list, "wt-new").key)
                    .await
            ),
            Some("StaleSession".into())
        );
        // Moved between List and Open (no Git involved): refused, then listed as missing.
        let list = s.worktrees(id, g).await.unwrap().data;
        std::fs::rename(&a, f.temp.path().join("wt-a-moved")).unwrap();
        assert_eq!(
            code(
                &s.open_worktree(id, g, list.revision, find(&list, "wt-a").key)
                    .await
            ),
            Some("StaleSession".into())
        );
        let list = s.worktrees(id, g).await.unwrap().data;
        let moved = find(&list, "wt-a");
        assert_eq!(moved.availability, WorktreeAvailability::Missing);
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, moved.key).await),
            Some("UnsupportedState".into())
        );
        // The original session survived every refusal.
        assert!(s.working_changes(id, g).await.is_ok());
        // A refused selection stays refused even when the registry returns to the
        // exact state it was listed in.
        let list = s.worktrees(id, g).await.unwrap().data;
        let key = list.worktrees[0].key;
        let back = f.temp.path().join("wt-back");
        add(&f, &back, &["--detach", "HEAD"]);
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
        assert!(f
            .output(&["worktree", "remove", back.to_str().unwrap()])
            .status
            .success());
        assert!(f.output(&["worktree", "prune"]).status.success());
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
        assert_eq!(
            code(&s.worktree_status(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
    }
}

#[tokio::test]
async fn replaced_worktree_identity_is_refused() {
    for format in FORMATS {
        let f = base(format);
        let a = f.temp.path().join("wt-a");
        add(&f, &a, &["feature"]);
        let s = RepositoryService::default();
        let (id, g) = open(&s, &f.root).await;
        let list = s.worktrees(id, g).await.unwrap().data;
        let key = find(&list, "wt-a").key;
        // Same path, same admin id, new directories: the inodes no longer match.
        assert!(f
            .output(&["worktree", "remove", "--force", a.to_str().unwrap()])
            .status
            .success());
        add(&f, &a, &["feature"]);
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
        // A checkout swapped for a copy that links to the same admin directory.
        let list = s.worktrees(id, g).await.unwrap().data;
        let key = find(&list, "wt-a").key;
        let copy = f.temp.path().join("copy");
        std::fs::create_dir(&copy).unwrap();
        std::fs::copy(a.join(".git"), copy.join(".git")).unwrap();
        std::fs::rename(&a, f.temp.path().join("old")).unwrap();
        std::fs::rename(&copy, &a).unwrap();
        assert_eq!(
            code(&s.open_worktree(id, g, list.revision, key).await),
            Some("StaleSession".into())
        );
        assert!(s.working_changes(id, g).await.is_ok());
    }
}

#[tokio::test]
async fn worktree_removed_while_its_session_is_open() {
    for format in FORMATS {
        let f = base(format);
        let a = f.temp.path().join("wt-a");
        add(&f, &a, &["feature"]);
        let s = RepositoryService::default();
        let (id, g) = open(&s, &a).await;
        assert!(f
            .output(&["worktree", "remove", a.to_str().unwrap()])
            .status
            .success());
        assert_eq!(
            code(&s.worktrees(id, g).await),
            Some("InvalidRepository".into())
        );
        assert_eq!(code(&s.refresh(id).await), Some("InvalidRepository".into()));
        // The main worktree can still be opened normally.
        assert!(s.open(&f.root).await.is_ok());
    }
}

#[tokio::test]
async fn shared_trust_with_distinct_filter_permission() {
    for format in FORMATS {
        let f = base(format);
        f.git(&["config", "filter.shared.clean", "cat"]);
        let a = f.temp.path().join("wt-a");
        add(&f, &a, &["feature"]);
        let b = f.temp.path().join("wt-b");
        add(&f, &b, &["spare"]);
        f.git(&["config", "extensions.worktreeConfig", "true"]);
        assert!(git_in(
            &f,
            &b,
            &["config", "--worktree", "filter.extra.clean", "cat"]
        )
        .status
        .success());
        let s = RepositoryService::default();
        let (id, g) = open(&s, &f.root).await;
        let list = s.worktrees(id, g).await.unwrap().data;
        assert!(!list.trusted);
        let status = |key| s.worktree_status(id, g, list.revision, key);
        assert_eq!(
            status(find(&list, "wt-a").key).await.unwrap().data.filters,
            WorktreeFilters::ReviewRequired
        );
        s.set_trust(id, g, true).await.unwrap();
        let list = s.worktrees(id, g).await.unwrap().data;
        assert!(list.trusted);
        let status = |key| s.worktree_status(id, g, list.revision, key);
        assert_eq!(
            status(0).await.unwrap().data.filters,
            WorktreeFilters::Authorized
        );
        assert_eq!(
            status(find(&list, "wt-a").key).await.unwrap().data.filters,
            WorktreeFilters::Authorized
        );
        let extra = status(find(&list, "wt-b").key).await.unwrap().data;
        assert!(extra.trusted);
        assert_eq!(extra.filters, WorktreeFilters::ReviewRequired);
        // Base trust alone does not authorize the extra filter for writes there.
        let list = s.worktrees(id, g).await.unwrap().data;
        let opened = s
            .open_worktree(id, g, list.revision, find(&list, "wt-b").key)
            .await
            .unwrap();
        std::fs::write(b.join("x.txt"), "x\n").unwrap();
        let w = s
            .working_changes(opened.session_id, opened.generation)
            .await
            .unwrap()
            .data;
        let index = w
            .untracked
            .iter()
            .find(|c| c.path == "x.txt")
            .unwrap()
            .index;
        let staged = s
            .stage_file(
                opened.session_id,
                opened.generation,
                w.revision,
                WorkingArea::Untracked,
                index,
            )
            .await;
        assert_eq!(code(&staged), Some("TrustRequired".into()));
    }
}

#[tokio::test]
async fn protected_status_counts_staged_modified_and_untracked() {
    for format in FORMATS {
        let f = base(format);
        let a = f.temp.path().join("wt-a");
        add(&f, &a, &["feature"]);
        std::fs::write(a.join("a.txt"), "modified\n").unwrap();
        std::fs::write(a.join("new.txt"), "n\n").unwrap();
        std::fs::write(a.join("u1.txt"), "u\n").unwrap();
        std::fs::write(a.join("u2.txt"), "u\n").unwrap();
        std::fs::write(a.join("debug.log"), "ignored\n").unwrap();
        assert!(git_in(&f, &a, &["add", "new.txt"]).status.success());
        let s = RepositoryService::default();
        let (id, g) = open(&s, &f.root).await;
        let list = s.worktrees(id, g).await.unwrap().data;
        let st = s
            .worktree_status(id, g, list.revision, find(&list, "wt-a").key)
            .await
            .unwrap()
            .data;
        assert_eq!(
            (st.staged, st.modified, st.untracked, st.conflicts),
            (1, 1, 2, 0)
        );
        let main = s
            .worktree_status(id, g, list.revision, 0)
            .await
            .unwrap()
            .data;
        assert_eq!((main.staged, main.modified, main.untracked), (0, 0, 0));
    }
}

/// inotify on every directory under `root` (except `skip`): any create, delete,
/// write, rename or attribute change in the repository or its checkouts, even a
/// lock created and removed within one operation. Reads are not watched.
#[cfg(target_os = "linux")]
struct Watch {
    fd: i32,
    dirs: std::collections::HashMap<i32, PathBuf>,
}
#[cfg(target_os = "linux")]
impl Watch {
    fn new(root: &Path, skip: &Path) -> Self {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0);
        let mut watch = Watch {
            fd,
            dirs: Default::default(),
        };
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).unwrap();
            let mask = libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MODIFY
                | libc::IN_ATTRIB
                | libc::IN_CLOSE_WRITE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF
                | libc::IN_DONT_FOLLOW
                | libc::IN_ONLYDIR;
            let wd = unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) };
            assert!(wd >= 0, "watch {}", dir.display());
            watch.dirs.insert(wd, dir.clone());
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p != skip && std::fs::symlink_metadata(&p).unwrap().is_dir() {
                    stack.push(p);
                }
            }
        }
        watch
    }
    fn events(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return out;
            }
            let mut at = 0;
            while at < n as usize {
                let event: libc::inotify_event =
                    unsafe { std::ptr::read_unaligned(buf[at..].as_ptr().cast()) };
                let start = at + std::mem::size_of::<libc::inotify_event>();
                let name = &buf[start..start + event.len as usize];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                let dir = self.dirs.get(&event.wd).map(|d| d.display().to_string());
                out.push(format!(
                    "{:?}/{} mask={:#x}",
                    dir,
                    String::from_utf8_lossy(name),
                    event.mask
                ));
                at = start + event.len as usize;
            }
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for Watch {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn no_filesystem_change_is_observed_during_list_inspect_open() {
    for format in FORMATS {
        for trusted in [false, true] {
            let h = hostile(format);
            let f = &h.f;
            let s = RepositoryService::default();
            let (id, g) = open(&s, &f.root).await;
            if trusted {
                s.set_trust(id, g, true).await.unwrap();
            }
            let watch = Watch::new(f.temp.path(), &h.markers);
            let list = s.worktrees(id, g).await.unwrap().data;
            for w in &list.worktrees {
                let _ = s.worktree_status(id, g, list.revision, w.key).await;
            }
            let linked = find(&list, "$(x)").key;
            let opened = s.open_worktree(id, g, list.revision, linked).await.unwrap();
            let list = s
                .worktrees(opened.session_id, opened.generation)
                .await
                .unwrap()
                .data;
            s.open_worktree(opened.session_id, opened.generation, list.revision, 0)
                .await
                .unwrap();
            let seen = watch.events();
            assert!(seen.is_empty(), "{format} trusted={trusted}: {seen:#?}");
            // Positive control: a native status that refreshes the index is seen.
            let now = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
            std::fs::File::options()
                .write(true)
                .open(f.root.join("a.txt"))
                .unwrap()
                .set_modified(now)
                .unwrap();
            watch.events();
            git_in(
                f,
                &f.root,
                &[
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "status",
                    "--porcelain",
                ],
            );
            let control = watch.events();
            assert!(
                control.iter().any(|e| e.contains("index.lock")),
                "{format}: control not observed: {control:#?}"
            );
        }
    }
}
