//! Dedicated executable: process-wide Git instrumentation cannot race other tests.
//! These are prevention assertions: no Git launch, no program marker, no mutation.
#![cfg(unix)]
use poseidon_core::{domain::*, RepositoryService};
use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;
fn external(f: &Fixture, real: &Path, args: &[&str]) -> Vec<u8> {
    let template = f.command();
    let mut cmd = Command::new(real);
    cmd.current_dir(&f.root);
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            cmd.env(key, value);
        } else {
            cmd.env_remove(key);
        }
    }
    let out = cmd.args(args).output().unwrap();
    assert!(
        out.status.success(),
        "external fixture writer: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}
fn protected(f: &Fixture) -> Vec<Option<Vec<u8>>> {
    [
        "a",
        "u",
        ".git/index",
        ".git/HEAD",
        ".git/refs/heads/main",
        ".git/refs/stash",
        ".git/logs/refs/stash",
        ".git/index.lock",
    ]
    .map(|p| std::fs::read(f.root.join(p)).ok())
    .into()
}
#[tokio::test]
async fn external_reflog_and_late_program_changes_are_prevented_before_any_git_launch() {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("git");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
if [ -f "$POSEIDON_STASH_RACE/armed" ]; then
 : > "$POSEIDON_STASH_RACE/GIT_STARTED"
fi
exec "$POSEIDON_STASH_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("POSEIDON_STASH_REAL_GIT", &real);
    std::env::set_var("POSEIDON_STASH_RACE", dir.path());
    std::env::set_var(
        "PATH",
        std::env::join_paths(
            std::iter::once(dir.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap(),
    );
    for format in ["sha1", "sha256"] {
        for change in [
            "insert",
            "delete",
            "reorder",
            "traditional-hook",
            "configured-hook",
            "hooks-path",
            "clean",
            "smudge",
            "process",
            "merge",
            "include",
            "include-if",
            "global",
            "system",
            "environment",
            "executable-replaced",
            "index-lock",
        ] {
            let f = Fixture::format(format);
            std::fs::create_dir_all(f.root.join(".git/info")).unwrap();
            f.write("a", "base\n");
            f.commit("base");
            for text in ["oldest\n", "reviewed\n", "newest\n"] {
                f.write("a", text);
                f.git(&["stash", "push", "-m", text.trim()]);
            }
            let s = RepositoryService::default();
            let r = s.open(&f.root).await.unwrap();
            let (id, g) = (r.session_id, r.generation);
            s.set_trust(id, g, true).await.unwrap();
            let entries = s.stashes(id, g).await.unwrap().data.entries;
            let selection = entries[1].selection.clone();
            assert!(matches!(
                s.prepare_stash(
                    id,
                    g,
                    StashAction::Drop {
                        selection: selection.clone()
                    }
                )
                .await
                .unwrap_err()
                .code,
                ErrorCode::UnsupportedState
            ));
            // Make the change after a client attempted preparation, at the last
            // point preceding execute. Real external writers bypass our spy.
            match change {
                "insert" => {
                    f.write("a", "external\n");
                    external(&f, &real, &["stash", "push", "-m", "external"]);
                }
                "delete" => {
                    external(&f, &real, &["stash", "drop", "stash@{0}"]);
                }
                "reorder" => {
                    external(
                        &f,
                        &real,
                        &[
                            "stash",
                            "store",
                            "-m",
                            "move-oldest",
                            &entries[2].selection.oid,
                        ],
                    );
                    external(
                        &f,
                        &real,
                        &[
                            "reflog",
                            "delete",
                            "--rewrite",
                            "--updateref",
                            "refs/stash@{3}",
                        ],
                    );
                }
                "traditional-hook" | "executable-replaced" => {
                    f.write(
                        ".git/hooks/post-index-change",
                        "#!/bin/sh\n: > PROGRAM_RAN\n",
                    );
                    if change == "executable-replaced" {
                        f.write(
                            ".git/hooks/post-index-change",
                            "#!/bin/sh\n: > PROGRAM_RAN\nexit 42\n",
                        );
                    }
                    std::fs::set_permissions(
                        f.root.join(".git/hooks/post-index-change"),
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                }
                "configured-hook" => {
                    external(
                        &f,
                        &real,
                        &["config", "hook.late.command", ": > PROGRAM_RAN"],
                    );
                    external(
                        &f,
                        &real,
                        &["config", "hook.late.event", "post-index-change"],
                    );
                }
                "hooks-path" => {
                    std::fs::create_dir(f.root.join("hooks")).unwrap();
                    f.write("hooks/post-index-change", "#!/bin/sh\n: > PROGRAM_RAN\n");
                    std::fs::set_permissions(
                        f.root.join("hooks/post-index-change"),
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                    external(&f, &real, &["config", "core.hooksPath", "hooks"]);
                }
                "clean" | "smudge" | "process" => {
                    f.write(".gitattributes", "a filter=unknown-after-preview\n");
                    f.write(".git/info/attributes", "a filter=unknown-after-preview\n");
                    external(
                        &f,
                        &real,
                        &[
                            "config",
                            &format!("filter.unknown-after-preview.{change}"),
                            ": > PROGRAM_RAN; cat",
                        ],
                    );
                }
                "merge" => {
                    f.write(".gitattributes", "a merge=unknown-after-preview\n");
                    external(
                        &f,
                        &real,
                        &[
                            "config",
                            "merge.unknown-after-preview.driver",
                            ": > PROGRAM_RAN; exit 1",
                        ],
                    );
                }
                "include" | "include-if" => {
                    f.write("late.config","[filter \"unknown-after-preview\"]\n clean = : > PROGRAM_RAN; cat\n[hook \"late\"]\n command = : > PROGRAM_RAN\n event = post-index-change\n");
                    let key = if change == "include" {
                        "include.path"
                    } else {
                        "includeIf.onbranch:main.path"
                    };
                    external(
                        &f,
                        &real,
                        &["config", key, &f.root.join("late.config").to_string_lossy()],
                    );
                    f.write(".gitattributes", "a filter=unknown-after-preview\n");
                }
                "global" | "system" => {
                    let path = dir.path().join(change);
                    std::fs::write(
                        &path,
                        "[hook \"late\"]\n command = : > PROGRAM_RAN\n event = post-index-change\n",
                    )
                    .unwrap();
                    std::env::set_var(
                        if change == "global" {
                            "GIT_CONFIG_GLOBAL"
                        } else {
                            "GIT_CONFIG_SYSTEM"
                        },
                        path,
                    );
                }
                "environment" => {
                    std::env::set_var("GIT_CONFIG_COUNT", "1");
                    std::env::set_var("GIT_CONFIG_KEY_0", "filter.late.clean");
                    std::env::set_var("GIT_CONFIG_VALUE_0", ": > PROGRAM_RAN; cat");
                }
                _ => f.write(".git/index.lock", "external lock, never remove"),
            }
            f.write("a", "precious dirty\n");
            f.write("u", "untracked\n");
            let before = protected(&f);
            std::fs::write(dir.path().join("armed"), "").unwrap();
            // Before prepare, after refused prepare, and repeated/delayed execute:
            // neither a stale client nor a guessed/replayed token can launch Git.
            for action in [
                StashAction::Create {
                    message: "late".into(),
                    include_untracked: true,
                },
                StashAction::Apply {
                    selection: selection.clone(),
                    restore_index: false,
                },
                StashAction::Apply {
                    selection: selection.clone(),
                    restore_index: true,
                },
                StashAction::Drop { selection },
            ] {
                assert!(matches!(
                    s.prepare_stash(id, g, action).await.unwrap_err().code,
                    ErrorCode::UnsupportedState
                ));
                tokio::task::yield_now().await;
                for token in [0, 1, u32::MAX, 1] {
                    assert!(matches!(
                        s.execute_stash(id, g, token).await.unwrap_err().code,
                        ErrorCode::UnsupportedState
                    ));
                }
            }
            assert!(
                !dir.path().join("GIT_STARTED").exists(),
                "Git launched for {format}/{change}"
            );
            assert!(
                !f.root.join("PROGRAM_RAN").exists(),
                "unauthorized program ran for {format}/{change}"
            );
            assert_eq!(
                protected(&f),
                before,
                "external state changed for {format}/{change}"
            );
            std::fs::remove_file(dir.path().join("armed")).unwrap();
            // The remaining operations inspect immutable objects. Configured
            // programs must not run here either; index/reflog stay byte-identical.
            let list = s.stashes(id, g).await.unwrap().data;
            let layers = s
                .stash_details(id, g, list.entries[0].selection.clone())
                .await
                .unwrap()
                .data;
            for layer in layers {
                for file in layer.files {
                    s.file_diff(id, g, &layer.oid, file.index, 0, false)
                        .await
                        .unwrap();
                }
            }
            assert!(
                !f.root.join("PROGRAM_RAN").exists(),
                "read launched a program for {format}/{change}"
            );
            assert_eq!(
                protected(&f),
                before,
                "read changed state for {format}/{change}"
            );

            for key in [
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_SYSTEM",
                "GIT_CONFIG_COUNT",
                "GIT_CONFIG_KEY_0",
                "GIT_CONFIG_VALUE_0",
            ] {
                std::env::remove_var(key);
            }
        }
    }
}
