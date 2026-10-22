//! One process owns the PATH injector; all repositories and programs are disposable.
#![cfg(unix)]
use poseidon_core::{domain::*, RepositoryService};
use std::{os::unix::fs::PermissionsExt, path::Path};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;

fn snapshot(f: &Fixture, label: &str) {
    fn walk(path: &Path, base: &Path, rows: &mut Vec<serde_json::Value>) {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for p in entries {
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                walk(&p, base, rows);
            } else {
                rows.push(serde_json::json!({
                    "path": p.strip_prefix(base).unwrap().as_os_str().as_encoded_bytes(),
                    "mode": m.permissions().mode(),
                    "bytes": if m.is_file() { std::fs::read(&p).unwrap() } else { Vec::new() },
                    "symlink": std::fs::read_link(&p).ok(),
                }));
            }
        }
    }
    if let Some(directory) = std::env::var_os("POSEIDON_T221_EVIDENCE") {
        let mut rows = Vec::new();
        walk(f.temp.path(), f.temp.path(), &mut rows);
        std::fs::write(
            Path::new(&directory).join(format!("{label}.json")),
            serde_json::to_vec(&rows).unwrap(),
        )
        .unwrap();
    }
}

fn hook(f: &Fixture, name: &str, body: &str) {
    let p = f.root.join(".git/hooks").join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn fixture(format: &str) -> Fixture {
    let f = Fixture::format(format);
    for name in ["a", "b", "c", "local", "space 日本 ; $(literal)\nfile"] {
        f.write(name, "base\n");
    }
    f.write(".gitignore", "ignored\n");
    f.commit("base");
    f.git(&["switch", "-c", "victim"]);
    for name in ["a", "b", "c"] {
        f.write(name, "target\n");
    }
    f.commit("target");
    f.git(&["switch", "main"]);
    f
}

async fn partial_switches(injector: &Path) {
    for format in ["sha1", "sha256"] {
        for mode in [
            "normal",
            "smudge",
            "post-checkout",
            "external-file",
            "external-dir",
            "private-index",
            "index-publish",
            "head-publish",
            "signal-before",
            "signal-after",
            "smudge-signal",
            "smudge-external-file",
            "smudge-external-dir",
        ] {
            let mut native_mid_checkout = None;
            for native in [true, false] {
                let f = fixture(format);
                std::env::set_var("POSEIDON_T221_ROOT", &f.root);
                f.write("local", "staged local\n");
                f.git(&["add", "local"]);
                f.write("local", "unstaged local\n");
                f.write("space 日本 ; $(literal)\nfile", "special unstaged\n");
                f.write("untracked", "untracked keep\n");
                f.write("ignored", "ignored keep\n");
                if mode.starts_with("smudge") {
                    let program = f.temp.path().join("smudge");
                    let failure = if mode.starts_with("smudge-external") {
                        ": > ../filter-ready; while [ ! -f ../writer-done ]; do sleep 0.01; done"
                    } else if mode == "smudge-signal" {
                        "kill -TERM \"$PPID\"; exit 1"
                    } else {
                        "exit 1"
                    };
                    std::fs::write(&program,format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> ../filter-invocations\nif [ \"$1\" = b ]; then {failure}; fi\ncat\n")).unwrap();
                    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                    f.git(&[
                        "config",
                        "filter.guarded.smudge",
                        &format!("{} %f", program.display()),
                    ]);
                    f.git(&["config", "filter.guarded.required", "true"]);
                    f.git(&["config", "filter.guarded.clean", "cat"]);
                    f.write(
                        ".gitattributes",
                        "a filter=guarded\nb filter=guarded\nc filter=guarded\n",
                    );
                }
                if mode == "post-checkout" {
                    hook(
                        &f,
                        "post-checkout",
                        "printf 'ran\\n' >> ../hook-invocations; exit 1",
                    );
                }
                let s = RepositoryService::default();
                let r = s.open(&f.root).await.unwrap();
                s.set_trust(r.session_id, r.generation, true).await.unwrap();
                if mode == "post-checkout" {
                    s.set_branch_hook_trust(r.session_id, r.generation, true)
                        .await
                        .unwrap();
                }
                let p = s
                    .prepare_branch(
                        r.session_id,
                        r.generation,
                        BranchAction::Switch {
                            name: "victim".into(),
                            expected_oid: f.git(&["rev-parse", "victim"]),
                        },
                    )
                    .await
                    .unwrap()
                    .data;
                assert!(p.block.is_none(), "{mode}: {:?}", p.block);
                let label = format!(
                    "{format}-partial-{mode}-{}",
                    if native { "native" } else { "poseidon" }
                );
                snapshot(&f, &format!("{label}-before"));
                let writer = if mode.starts_with("smudge-external") {
                    let root = f.root.clone();
                    let parent = f.temp.path().to_path_buf();
                    Some(std::thread::spawn(move || {
                        let start = std::time::Instant::now();
                        while !parent.join("filter-ready").exists() {
                            assert!(start.elapsed().as_secs() < 20, "smudge barrier not reached");
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        if mode == "smudge-external-dir" {
                            if root.join("b").exists() {
                                std::fs::remove_file(root.join("b")).unwrap();
                            }
                            std::fs::create_dir(root.join("b")).unwrap();
                            std::fs::write(root.join("b/note"), b"external during checkout\n")
                                .unwrap();
                        } else {
                            std::fs::write(root.join("b"), b"external during checkout\n").unwrap();
                        }
                        std::fs::write(parent.join("writer-done"), b"done").unwrap();
                    }))
                } else {
                    None
                };
                let extra_publication =
                    matches!(mode, "index-publish" | "head-publish" | "private-index");
                if !(mode.starts_with("smudge")
                    || matches!(mode, "normal" | "post-checkout")
                    || native && extra_publication)
                {
                    std::fs::write(injector.join("trigger"), format!("switch {mode}")).unwrap();
                }
                let outcome = if native {
                    format!(
                        "exit={:?}",
                        f.output(&["switch", "--no-overwrite-ignore", "victim"])
                            .status
                            .code()
                    )
                } else {
                    format!(
                        "{:?}",
                        s.execute_branch(r.session_id, r.generation, p.token)
                            .await
                            .map(|r| r.data.outcome)
                            .map_err(|e| e.code)
                    )
                };
                if let Some(writer) = writer {
                    writer.join().unwrap();
                }
                std::fs::set_permissions(
                    f.root.join(".git"),
                    std::fs::Permissions::from_mode(0o700),
                )
                .unwrap();
                println!(
                    "T221_PARTIAL {label} {outcome} head={} index_bytes={}",
                    f.git(&["symbolic-ref", "--short", "HEAD"]),
                    std::fs::metadata(f.root.join(".git/index")).unwrap().len()
                );
                snapshot(&f, &format!("{label}-after"));
                assert!(f.output(&["ls-files", "--stage"]).status.success());
                assert_eq!(f.git(&["show", ":local"]), "staged local");
                for (path, bytes) in [
                    ("local", "unstaged local\n"),
                    ("space 日本 ; $(literal)\nfile", "special unstaged\n"),
                    ("untracked", "untracked keep\n"),
                    ("ignored", "ignored keep\n"),
                ] {
                    assert_eq!(
                        std::fs::read(f.root.join(path)).unwrap(),
                        bytes.as_bytes(),
                        "{label}/{path}"
                    );
                }
                if !native {
                    if matches!(mode, "normal") {
                        assert_eq!(outcome, "Ok(Applied)");
                    } else if !mode.starts_with("smudge-external") {
                        assert_ne!(outcome, "Ok(Applied)", "false success: {label}");
                    }
                    assert!(
                        s.execute_branch(r.session_id, r.generation, p.token)
                            .await
                            .is_err(),
                        "used plan must never retry"
                    );
                }
                if mode.starts_with("smudge-external") {
                    let bytes = if f.root.join("b").is_dir() {
                        std::fs::read(f.root.join("b/note")).unwrap()
                    } else {
                        std::fs::read(f.root.join("b")).unwrap()
                    };
                    if native {
                        native_mid_checkout = Some((outcome == "exit=Some(0)", bytes));
                    } else {
                        let (success, native_bytes) = native_mid_checkout.as_ref().unwrap();
                        assert_eq!(
                            &bytes, native_bytes,
                            "checkout differs from native under the same writer"
                        );
                        assert_eq!(outcome == "Ok(Applied)", *success);
                    }
                }
                if mode == "external-file" {
                    assert_eq!(std::fs::read(f.root.join("b")).unwrap(), b"external edit\n");
                }
                if mode == "external-dir" {
                    assert_eq!(
                        std::fs::read(f.root.join("b/note")).unwrap(),
                        b"external edit\n"
                    );
                }
                if mode == "post-checkout" {
                    assert_eq!(
                        std::fs::read_to_string(f.temp.path().join("hook-invocations")).unwrap(),
                        "ran\n"
                    );
                }
                if mode == "head-publish" && !native {
                    assert_eq!(
                        std::fs::read(f.root.join(".git/HEAD.lock")).unwrap(),
                        b"foreign lock"
                    );
                }
                if mode.starts_with("smudge") {
                    let calls =
                        std::fs::read_to_string(f.temp.path().join("filter-invocations")).unwrap();
                    assert_eq!(
                        calls.lines().filter(|p| *p == "b").count(),
                        1,
                        "filter retried"
                    );
                }
                std::fs::remove_file(injector.join("trigger")).ok();
            }
        }
    }
}

#[tokio::test]
async fn publication_and_new_worktree_registration_against_native_git() {
    let real = std::env::var_os("POSEIDON_TEST_REAL_GIT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .map(|p| p.join("git"))
                .find(|p| p.is_file())
                .unwrap()
        })
        .canonicalize()
        .unwrap();
    let injector = tempfile::tempdir().unwrap();
    let wrapper = injector.path().join("git");
    std::fs::write(&wrapper, r#"#!/bin/sh
external() {
 GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_COUNT=0 GIT_DIR="$POSEIDON_T221_ROOT/.git" GIT_COMMON_DIR="$POSEIDON_T221_ROOT/.git" GIT_WORK_TREE="$POSEIDON_T221_ROOT" "$POSEIDON_T221_GIT" "$@"
}
if [ -f "$POSEIDON_T221_TRIGGER" ]; then
 read -r expected mode < "$POSEIDON_T221_TRIGGER"
 if [ "$mode" = late-register ] && [ -z "$POSEIDON_REF_SOURCE" ]; then exec "$POSEIDON_T221_GIT" "$@"; fi
 for arg in "$@"; do
  if [ "$arg" = "$expected" ]; then
   if [ "$mode" = late-register ] && [ ! -f "$POSEIDON_T221_ROOT/../first-guard-check" ]; then
    : > "$POSEIDON_T221_ROOT/../first-guard-check"
    exec "$POSEIDON_T221_GIT" "$@"
   fi
   rm -f "$POSEIDON_T221_TRIGGER"
   printf '%s\n' "$mode" >> "$POSEIDON_T221_ROOT/../invocations"
   case "$mode" in
    late-register) "$POSEIDON_T221_GIT" "$@"; status=$?; test -f "$POSEIDON_T221_ROOT/.git/refs/heads/victim.lock" && printf 'locked\n' > "$POSEIDON_T221_ROOT/../ref-was-locked"; external worktree add --no-checkout "$POSEIDON_T221_ROOT/../late" victim > "$POSEIDON_T221_ROOT/../registration.out" 2> "$POSEIDON_T221_ROOT/../registration.err"; echo $? > "$POSEIDON_T221_ROOT/../registration.status"; exit "$status";;
    register-*) branch=${mode#register-}; external worktree add --no-checkout "$POSEIDON_T221_ROOT/../late" "$branch" > "$POSEIDON_T221_ROOT/../registration.out" 2> "$POSEIDON_T221_ROOT/../registration.err"; echo $? > "$POSEIDON_T221_ROOT/../registration.status";;
    signal-before) kill -TERM $$;;
    signal-after) "$POSEIDON_T221_GIT" "$@"; kill -TERM $$;;
    index-publish) "$POSEIDON_T221_GIT" "$@"; status=$?; chmod 500 "$POSEIDON_T221_ROOT/.git"; exit "$status";;
    head-publish) "$POSEIDON_T221_GIT" "$@"; status=$?; mv "$POSEIDON_T221_ROOT/.git/HEAD.lock" "$POSEIDON_T221_ROOT/../original-head-lock"; printf 'foreign lock' > "$POSEIDON_T221_ROOT/.git/HEAD.lock"; exit "$status";;
    private-index) mkdir "$GIT_INDEX_FILE.lock";;
    external-file) printf 'external edit\n' > "$POSEIDON_T221_ROOT/b";;
    external-dir) rm "$POSEIDON_T221_ROOT/b"; mkdir "$POSEIDON_T221_ROOT/b"; printf 'external edit\n' > "$POSEIDON_T221_ROOT/b/note";;
   esac
   break
  fi
 done
fi
exec "$POSEIDON_T221_GIT" "$@"
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("POSEIDON_T221_GIT", real);
    std::env::set_var("POSEIDON_T221_TRIGGER", injector.path().join("trigger"));
    std::env::set_var(
        "PATH",
        std::env::join_paths(
            std::iter::once(injector.path().to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap(),
    );
    partial_switches(injector.path()).await;
    // Current-worktree records may change normally; other HEADs must stay intact.
    for format in ["sha1", "sha256"] {
        let f = fixture(format);
        let linked = f.temp.path().join("linked");
        f.git(&["worktree", "add", "-b", "spare", linked.to_str().unwrap()]);
        let main_head = std::fs::read(f.root.join(".git/HEAD")).unwrap();
        let s = RepositoryService::default();
        let r = s.open(&linked).await.unwrap();
        s.set_trust(r.session_id, r.generation, true).await.unwrap();
        let p = s
            .prepare_branch(
                r.session_id,
                r.generation,
                BranchAction::Rename {
                    name: "spare".into(),
                    expected_oid: f.git(&["rev-parse", "spare"]),
                    new_name: "linked-renamed".into(),
                },
            )
            .await
            .unwrap()
            .data;
        assert!(p.block.is_none());
        assert_eq!(
            s.execute_branch(r.session_id, r.generation, p.token)
                .await
                .unwrap()
                .data
                .outcome,
            BranchOutcome::Applied
        );
        assert_eq!(std::fs::read(f.root.join(".git/HEAD")).unwrap(), main_head);
        assert_eq!(
            std::fs::read(f.root.join(".git/worktrees/linked/HEAD")).unwrap(),
            b"ref: refs/heads/linked-renamed\n"
        );
        println!("T221_LINKED {format} current_rename=Applied primary_head_preserved=true");
    }
    let mut violations = Vec::new();
    for format in ["sha1", "sha256"] {
        for operation in ["switch", "rename", "delete", "rename-current", "create"] {
            for branch in ["victim", "main", "new"] {
                for native in [true, false] {
                    let f = fixture(format);
                    // All refs have the same OID: delete ancestry must not mask occupancy.
                    f.git(&[
                        "update-ref",
                        "refs/heads/victim",
                        &f.git(&["rev-parse", "main"]),
                    ]);
                    std::env::set_var("POSEIDON_T221_ROOT", &f.root);
                    let mode = format!("register-{branch}");
                    let label = format!(
                        "{format}-{operation}-{branch}-{}",
                        if native { "native" } else { "poseidon" }
                    );
                    let command = match operation {
                        "switch" => "switch",
                        "create" => "branch",
                        _ => "update-ref",
                    };
                    let before = f.git(&["rev-parse", "HEAD"]);
                    let source_bytes =
                        std::fs::read(f.root.join(".git/refs/heads/victim")).unwrap();
                    let source_log =
                        std::fs::read(f.root.join(".git/logs/refs/heads/victim")).unwrap();
                    let (service, opened, plan) = if !native {
                        let s = RepositoryService::default();
                        let r = s.open(&f.root).await.unwrap();
                        s.set_trust(r.session_id, r.generation, true).await.unwrap();
                        let action = match operation {
                            "switch" => BranchAction::Switch {
                                name: "victim".into(),
                                expected_oid: before.clone(),
                            },
                            "delete" => BranchAction::Delete {
                                name: "victim".into(),
                                expected_oid: before.clone(),
                            },
                            "create" => BranchAction::Create {
                                name: "new".into(),
                                start_oid: before.clone(),
                            },
                            _ => BranchAction::Rename {
                                name: if operation == "rename-current" {
                                    "main"
                                } else {
                                    "victim"
                                }
                                .into(),
                                expected_oid: before.clone(),
                                new_name: "new".into(),
                            },
                        };
                        let p = s
                            .prepare_branch(r.session_id, r.generation, action)
                            .await
                            .unwrap()
                            .data;
                        assert!(p.block.is_none(), "{:?}", p.block);
                        (Some(s), Some(r), Some(p))
                    } else {
                        (None, None, None)
                    };
                    snapshot(&f, &format!("{label}-before"));
                    if native {
                        let o = f.output(&[
                            "worktree",
                            "add",
                            "--no-checkout",
                            f.temp.path().join("late").to_str().unwrap(),
                            branch,
                        ]);
                        std::fs::write(
                            f.temp.path().join("registration.status"),
                            format!("{}", o.status.code().unwrap()),
                        )
                        .unwrap();
                    } else {
                        std::fs::write(
                            injector.path().join("trigger"),
                            format!("{command} {mode}"),
                        )
                        .unwrap();
                    }
                    let outcome = if native {
                        let args = match operation {
                            "switch" => vec!["switch", "--no-overwrite-ignore", "victim"],
                            "delete" => vec!["branch", "-d", "victim"],
                            "create" => vec!["branch", "new"],
                            "rename-current" => vec!["branch", "-m", "main", "new"],
                            _ => vec!["branch", "-m", "victim", "new"],
                        };
                        format!("exit={:?}", f.output(&args).status.code())
                    } else {
                        let r = opened.as_ref().unwrap();
                        let result = service
                            .as_ref()
                            .unwrap()
                            .execute_branch(
                                r.session_id,
                                r.generation,
                                plan.as_ref().unwrap().token,
                            )
                            .await;
                        format!("{:?}", result.map(|r| r.data.outcome).map_err(|e| e.code))
                    };
                    let registered =
                        std::fs::read_to_string(f.temp.path().join("registration.status"))
                            .unwrap()
                            .trim()
                            == "0";
                    let other_head = if registered {
                        std::fs::read_to_string(f.root.join(".git/worktrees/late/HEAD")).unwrap()
                    } else {
                        String::new()
                    };
                    let missing = registered
                        && !f
                            .output(&[
                                "rev-parse",
                                "--verify",
                                other_head.trim().strip_prefix("ref: ").unwrap(),
                            ])
                            .status
                            .success();
                    let double =
                        registered && f.git(&["symbolic-ref", "--short", "HEAD"]) == branch;
                    println!("T221_REG {label} {outcome} registered={registered} missing={missing} double={double}");
                    snapshot(&f, &format!("{label}-after"));
                    if !native && (missing || (double && operation == "switch")) {
                        violations.push(label);
                    }
                    if !native
                        && registered
                        && branch == "victim"
                        && matches!(operation, "rename" | "delete")
                    {
                        assert_eq!(
                            std::fs::read(f.root.join(".git/refs/heads/victim")).unwrap(),
                            source_bytes,
                            "guard must PREVENT removal of the occupied ref"
                        );
                        assert_eq!(
                            std::fs::read(f.root.join(".git/logs/refs/heads/victim")).unwrap(),
                            source_log,
                            "guard must preserve the original reflog"
                        );
                        assert!(!f
                            .output(&["show-ref", "--verify", "refs/heads/new"])
                            .status
                            .success());
                        assert_eq!(outcome, "Ok(Unknown)");
                    }
                    assert!(f.output(&["ls-files", "--stage"]).status.success());
                    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
                    // --no-checkout must never write files in the other worktree.
                    if registered {
                        assert_eq!(
                            std::fs::read_dir(f.temp.path().join("late"))
                                .unwrap()
                                .count(),
                            1
                        );
                    }
                }
            }
        }
    }
    // A native no-checkout registration does not cooperate with branch ref locks.
    // The guard narrows the window; it cannot provide global exclusion. Preserve
    // this counterexample and require honest reconciliation instead of Applied.
    for format in ["sha1", "sha256"] {
        for delete in [true, false] {
            let f = fixture(format);
            std::env::set_var("POSEIDON_T221_ROOT", &f.root);
            let oid = f.git(&["rev-parse", "main"]);
            f.git(&["update-ref", "refs/heads/victim", &oid]);
            let s = RepositoryService::default();
            let r = s.open(&f.root).await.unwrap();
            s.set_trust(r.session_id, r.generation, true).await.unwrap();
            let action = if delete {
                BranchAction::Delete {
                    name: "victim".into(),
                    expected_oid: oid,
                }
            } else {
                BranchAction::Rename {
                    name: "victim".into(),
                    expected_oid: oid,
                    new_name: "new".into(),
                }
            };
            let p = s
                .prepare_branch(r.session_id, r.generation, action)
                .await
                .unwrap()
                .data;
            assert!(p.block.is_none());
            let label = format!("{format}-late-{}", if delete { "delete" } else { "rename" });
            snapshot(&f, &format!("{label}-before"));
            std::fs::write(injector.path().join("trigger"), "worktree late-register").unwrap();
            let result = s
                .execute_branch(r.session_id, r.generation, p.token)
                .await
                .unwrap();
            snapshot(&f, &format!("{label}-after"));
            assert_eq!(
                std::fs::read_to_string(f.temp.path().join("ref-was-locked")).unwrap(),
                "locked\n"
            );
            assert_eq!(
                std::fs::read_to_string(f.temp.path().join("registration.status"))
                    .unwrap()
                    .trim(),
                "0"
            );
            assert!(!f
                .output(&["show-ref", "--verify", "refs/heads/victim"])
                .status
                .success());
            assert_eq!(
                std::fs::read(f.root.join(".git/worktrees/late/HEAD")).unwrap(),
                b"ref: refs/heads/victim\n"
            );
            assert_eq!(
                result.data.outcome,
                BranchOutcome::Unknown,
                "late registration must not be announced as success"
            );
            println!("T221_LIMIT {label} cooperative_native_registration_while_ref_locked=true missing_ref=true outcome={:?}",result.data.outcome);
        }
    }
    // Baseline mode gathers all evidence before reporting the integrity failures.
    assert!(
        violations.is_empty(),
        "new registrations invalidated refs: {violations:?}"
    );
}
