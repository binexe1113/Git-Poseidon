//! Separate executable: PATH/host environment instrumentation is process-wide.
#![cfg(unix)]
use poseidon_core::{domain::*, RepositoryService};
use std::{os::unix::fs::PermissionsExt, path::Path};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;

fn hook(f: &Fixture, name: &str) {
    let path = f.root.join(".git/hooks").join(name);
    std::fs::write(&path, "#!/bin/sh\n: > PROGRAM_RAN\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn native(f: &Fixture, real: &Path, args: &[&str]) {
    let template = f.command();
    let mut command = std::process::Command::new(real);
    command.current_dir(&f.root);
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    let out = command.args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[tokio::test]
async fn late_config_and_hooks_do_not_execute_repository_programs() {
    let control = Fixture::new();
    control.write("a", "base");
    control.commit("base");
    hook(&control, "post-index-change");
    control.write("a", "changed");
    control.git(&["add", "a"]);
    assert!(control.root.join("PROGRAM_RAN").exists());
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
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("git");
    std::fs::write(&wrapper, r#"#!/bin/sh
export HOME="$POSEIDON_POLICY_RACE/home" XDG_CONFIG_HOME="$POSEIDON_POLICY_RACE/xdg"
if [ -f "$POSEIDON_POLICY_RACE/system" ]; then export GIT_CONFIG_SYSTEM="$POSEIDON_POLICY_RACE/system"; fi
external() {
 GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_DIR="$POSEIDON_POLICY_ROOT/.git" GIT_COMMON_DIR="$POSEIDON_POLICY_ROOT/.git" GIT_WORK_TREE="$POSEIDON_POLICY_ROOT" GIT_CONFIG_COUNT=0 "$POSEIDON_POLICY_GIT" -C "$POSEIDON_POLICY_ROOT" "$@"
}
if [ -f "$POSEIDON_POLICY_RACE/trigger" ]; then
 read -r expected mode < "$POSEIDON_POLICY_RACE/trigger"
 for arg in "$@"; do
  if [ "$arg" = "$expected" ]; then
   rm -f "$POSEIDON_POLICY_RACE/trigger"
   case "$mode" in
    hooks-path) mkdir -p "$POSEIDON_POLICY_ROOT/late-hooks"; printf '#!/bin/sh\n: > PROGRAM_RAN\n' > "$POSEIDON_POLICY_ROOT/late-hooks/post-index-change"; cp "$POSEIDON_POLICY_ROOT/late-hooks/post-index-change" "$POSEIDON_POLICY_ROOT/late-hooks/post-checkout"; cp "$POSEIDON_POLICY_ROOT/late-hooks/post-index-change" "$POSEIDON_POLICY_ROOT/late-hooks/reference-transaction"; chmod 700 "$POSEIDON_POLICY_ROOT/late-hooks/"*; external config core.hooksPath late-hooks;;
    hook) printf '#!/bin/sh\n: > PROGRAM_RAN\n' > "$POSEIDON_POLICY_ROOT/.git/hooks/post-index-change"; chmod 700 "$POSEIDON_POLICY_ROOT/.git/hooks/post-index-change";;
    configured) external config hook.late.command ': > PROGRAM_RAN'; external config hook.late.event reference-transaction; external config --add hook.late.event post-checkout; external config --add hook.late.event post-index-change;;
    global|system|include|include-if)
     printf '[filter "late"]\n clean = : > PROGRAM_RAN; cat\n[hook "late"]\n command = : > PROGRAM_RAN\n event = reference-transaction\n' > "$POSEIDON_POLICY_RACE/late.config"
     printf 'a filter=late\n' > "$POSEIDON_POLICY_ROOT/.gitattributes"
     case "$mode" in
      global) cp "$POSEIDON_POLICY_RACE/late.config" "$HOME/.gitconfig";;
      system) cp "$POSEIDON_POLICY_RACE/late.config" "$POSEIDON_POLICY_RACE/system"; export GIT_CONFIG_SYSTEM="$POSEIDON_POLICY_RACE/system";;
      include) external config include.path "$POSEIDON_POLICY_RACE/late.config";;
      include-if) external config includeIf.onbranch:main.path "$POSEIDON_POLICY_RACE/late.config";;
     esac;;
    aba) external update-ref refs/heads/victim "$POSEIDON_POLICY_REPLACEMENT"; external update-ref refs/heads/victim "$POSEIDON_POLICY_ORIGINAL";;
    attributes) printf 'a filter=second\n' > "$POSEIDON_POLICY_ROOT/.gitattributes";;
    occupancy) GIT_DIR="$POSEIDON_POLICY_ROOT/.git/worktrees/other" GIT_COMMON_DIR="$POSEIDON_POLICY_ROOT/.git" GIT_WORK_TREE="$POSEIDON_POLICY_OTHER" GIT_CONFIG_COUNT=0 "$POSEIDON_POLICY_GIT" symbolic-ref HEAD refs/heads/victim 2> "$POSEIDON_POLICY_RACE/external-wt-stderr"; echo $? > "$POSEIDON_POLICY_RACE/external-wt-status";;
    kill-after) "$POSEIDON_POLICY_GIT" "$@"; kill -KILL $$;;
    symref) external symbolic-ref refs/heads/victim refs/heads/other;;
    ref) external update-ref refs/heads/victim "$POSEIDON_POLICY_REPLACEMENT"; echo $? > "$POSEIDON_POLICY_RACE/external-ref-status";;
    recreated-source) printf 'unreviewed recreated\n' > "$POSEIDON_POLICY_ROOT/a";;
    content) printf 'changed after review\n' > "$POSEIDON_POLICY_ROOT/a";;
    replace-hook) printf '#!/bin/sh\n: > PROGRAM_RAN\n' > "$POSEIDON_POLICY_ROOT/.git/hooks/pre-commit"; chmod 700 "$POSEIDON_POLICY_ROOT/.git/hooks/pre-commit";;
    signing) external config commit.gpgSign true; external config gpg.program "$POSEIDON_POLICY_ROOT/.git/signer";;
    process) external config filter.late.process ': > PROGRAM_RAN; exit 1'; printf 'a filter=late\n' > "$POSEIDON_POLICY_ROOT/.gitattributes";;
    filter) external config filter.late.clean ': > PROGRAM_RAN; cat'; printf 'a filter=late\n' > "$POSEIDON_POLICY_ROOT/.gitattributes";;
   esac
   break
  fi
 done
fi
exec "$POSEIDON_POLICY_GIT" "$@"
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("POSEIDON_POLICY_GIT", &real);
    std::env::set_var("POSEIDON_POLICY_RACE", dir.path());
    std::env::set_var(
        "PATH",
        std::env::join_paths(
            std::iter::once(dir.path().to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap(),
    );
    std::fs::create_dir(dir.path().join("home")).unwrap();
    for format in ["sha1", "sha256"] {
        for (operation, mode) in [
            ("read", "filter"),
            ("stage", "hook"),
            ("stage", "filter"),
            ("stage", "configured"),
            ("unstage", "configured"),
            ("unstage", "hook"),
            ("read", "process"),
            ("stage", "process"),
            ("commit", "process"),
            ("commit", "configured"),
            ("commit", "filter"),
            ("branch", "configured"),
            ("switch", "configured"),
            ("switch", "filter"),
            ("switch", "ref"),
            ("rename", "configured"),
            ("rename", "ref"),
            ("delete", "configured"),
            ("delete", "ref"),
            ("rename", "symref"),
            ("delete", "symref"),
            ("rename", "aba"),
            ("delete", "aba"),
            ("stage", "attributes"),
            ("stage", "recreated-source"),
            ("switch", "occupancy"),
            ("rename", "kill-after"),
            ("stage", "content"),
            ("commit", "replace-hook"),
            ("commit", "signing"),
            ("read", "global"),
            ("read", "system"),
            ("read", "include"),
            ("read", "include-if"),
            ("stage", "global"),
            ("stage", "system"),
            ("stage", "include-if"),
            ("stage", "hooks-path"),
            ("commit", "hooks-path"),
            ("switch", "hooks-path"),
            ("branch", "hooks-path"),
        ] {
            for file in ["home/.gitconfig", "system", "late.config"] {
                std::fs::remove_file(dir.path().join(file)).ok();
            }
            let f = Fixture::format(format);
            std::env::set_var("POSEIDON_POLICY_ROOT", &f.root);
            f.write("a", "base\n");
            f.commit("base");
            f.git(&["branch", "victim"]);
            f.git(&["branch", "other"]);
            std::env::set_var("POSEIDON_POLICY_ORIGINAL", f.git(&["rev-parse", "victim"]));
            f.write("b", "base\n");
            f.commit("advance main");
            let replacement = f.git(&["rev-parse", "HEAD"]);
            std::env::set_var("POSEIDON_POLICY_REPLACEMENT", &replacement);
            if mode == "replace-hook" {
                f.write(
                    ".git/hooks/pre-commit",
                    "#!/bin/sh\n: > AUTHORIZED_HOOK_RAN\n",
                );
                std::fs::set_permissions(
                    f.root.join(".git/hooks/pre-commit"),
                    std::fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            f.write(".git/signer", "#!/bin/sh\n: > PROGRAM_RAN\nexit 1\n");
            std::fs::set_permissions(
                f.root.join(".git/signer"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            if mode == "attributes" {
                f.write(".gitattributes", "a filter=first\n");
                native(&f, &real, &["config", "filter.first.clean", "cat"]);
                native(
                    &f,
                    &real,
                    &[
                        "config",
                        "filter.second.clean",
                        ": > ALLOWED_SECOND_RAN; cat",
                    ],
                );
            }
            if mode == "occupancy" {
                let other = f.temp.path().join("other");
                native(
                    &f,
                    &real,
                    &["worktree", "add", "-b", "spare", other.to_str().unwrap()],
                );
                std::env::set_var("POSEIDON_POLICY_OTHER", other);
            }
            f.write("a", "staged!\n");
            native(&f, &real, &["add", "a"]);
            f.write("a", "working\n");
            if mode == "recreated-source" {
                f.write("a", "staged!\n");
                std::fs::rename(f.root.join("a"), f.root.join("moved")).unwrap();
                native(&f, &real, &["add", "-N", "moved"]);
            }
            let s = RepositoryService::default();
            let r = s.open(&f.root).await.unwrap();
            let (id, g) = (r.session_id, r.generation);
            if operation != "read" {
                s.set_trust(id, g, true).await.unwrap();
            }
            if mode == "replace-hook" {
                s.set_commit_hook_trust(id, g, true).await.unwrap();
            }
            let w = s.working_changes(id, g).await.unwrap().data;
            let index_before = std::fs::read(f.root.join(".git/index")).unwrap();
            let commit = if operation == "commit" {
                Some(s.prepare_commit(id, g, w.revision).await.unwrap().data)
            } else {
                None
            };
            let branch_action = match operation {
                "branch" => Some(BranchAction::Create {
                    name: "new".into(),
                    start_oid: replacement.clone(),
                }),
                "switch" => Some(BranchAction::Switch {
                    name: "victim".into(),
                    expected_oid: f.git(&["rev-parse", "victim"]),
                }),
                "rename" => Some(BranchAction::Rename {
                    name: "victim".into(),
                    expected_oid: f.git(&["rev-parse", "victim"]),
                    new_name: "new".into(),
                }),
                "delete" => Some(BranchAction::Delete {
                    name: "victim".into(),
                    expected_oid: f.git(&["rev-parse", "victim"]),
                }),
                _ => None,
            };
            let branch = if let Some(action) = branch_action {
                Some(s.prepare_branch(id, g, action).await.unwrap().data)
            } else {
                None
            };
            let command = match operation {
                "read" => "diff-files",
                "stage" => "add",
                "unstage" => "update-index",
                "commit" => "commit",
                "switch" => "switch",
                "rename" | "delete" => "update-ref",
                _ => "branch",
            };
            std::fs::write(dir.path().join("trigger"), format!("{command} {mode}")).unwrap();
            let observed = match operation {
                "read" => s.working_changes(id, g).await.map(|_| String::from("Read")),
                "stage" => s
                    .stage_file(id, g, w.revision, WorkingArea::Unstaged, 0)
                    .await
                    .map(|_| String::from("IndexChanged")),
                "unstage" => s
                    .unstage_file(id, g, w.revision, 0)
                    .await
                    .map(|_| String::from("IndexChanged")),
                "commit" => s
                    .create_commit(id, g, commit.unwrap().token, "guarded")
                    .await
                    .map(|r| format!("Commit::{:?}", r.data.outcome)),
                _ => s
                    .execute_branch(id, g, branch.unwrap().token)
                    .await
                    .map(|r| format!("Branch::{:?}", r.data.outcome)),
            };
            let outcome = observed.unwrap_or_else(|e| format!("Error::{:?}", e.code));
            println!("T22_CASE format={format} operation={operation} injection={mode} outcome={outcome} trusted={} hook_authorized={} unauthorized_marker={} authorized_old_hook={} authorized_second_filter={} index_changed={}",
                operation!="read",mode=="replace-hook",f.root.join("PROGRAM_RAN").exists(),f.root.join("AUTHORIZED_HOOK_RAN").exists(),f.root.join("ALLOWED_SECOND_RAN").exists(),std::fs::read(f.root.join(".git/index")).unwrap()!=index_before);
            assert!(
                !f.root.join("PROGRAM_RAN").exists(),
                "unauthorized execution: {format}/{operation}/{mode}"
            );
            if mode == "replace-hook" {
                assert!(f.root.join("AUTHORIZED_HOOK_RAN").exists());
            }
            if operation == "stage"
                && (mode == "filter"
                    || mode == "content"
                    || mode == "attributes"
                    || mode == "recreated-source")
            {
                assert_eq!(
                    std::fs::read(f.root.join(".git/index")).unwrap(),
                    index_before,
                    "unreviewed content published for {format}/{mode}"
                );
            }
            if mode == "occupancy" {
                assert_ne!(
                    std::fs::read_to_string(dir.path().join("external-wt-status"))
                        .unwrap()
                        .trim(),
                    "0",
                    "registered worktree HEAD was not locked"
                );
                assert!(
                    std::fs::read_to_string(dir.path().join("external-wt-stderr"))
                        .unwrap()
                        .contains("HEAD.lock"),
                    "writer failed for a reason other than the shared lock"
                );
                assert_eq!(
                    std::fs::read(f.root.join(".git/worktrees/other/HEAD")).unwrap(),
                    b"ref: refs/heads/spare\n"
                );
            }
            if mode == "aba" {
                assert_eq!(
                    f.git(&["rev-parse", "victim"]),
                    std::env::var("POSEIDON_POLICY_ORIGINAL").unwrap()
                );
                assert!(
                    !f.output(&["show-ref", "--verify", "refs/heads/new"])
                        .status
                        .success(),
                    "reflog ABA was not prevented under reference locks"
                );
                let log = std::fs::read(f.root.join(".git/logs/refs/heads/victim")).unwrap();
                assert!(String::from_utf8_lossy(&log).contains(&replacement));
            }
            if mode == "symref" {
                assert!(
                    f.output(&["show-ref", "--verify", "refs/heads/other"])
                        .status
                        .success(),
                    "symref target was deleted"
                );
                assert!(
                    f.output(&["symbolic-ref", "refs/heads/victim"])
                        .status
                        .success(),
                    "changed symref was removed after review"
                );
                assert!(!f
                    .output(&["show-ref", "--verify", "refs/heads/new"])
                    .status
                    .success());
            }
            if mode == "ref" {
                if operation == "switch" {
                    assert_eq!(
                        std::fs::read_to_string(dir.path().join("external-ref-status"))
                            .unwrap()
                            .trim(),
                        "128",
                        "external ref writer must be locked out"
                    );
                } else {
                    assert_eq!(
                        f.git(&["rev-parse", "victim"]),
                        replacement,
                        "replacement ref was removed/renamed"
                    );
                    assert!(!f
                        .output(&["show-ref", "--verify", "refs/heads/new"])
                        .status
                        .success());
                }
            }
            std::fs::remove_file(dir.path().join("trigger")).ok();
        }
    }
    for file in ["home/.gitconfig", "system", "late.config"] {
        std::fs::remove_file(dir.path().join(file)).ok();
    }
    for format in ["sha1", "sha256"] {
        let f = Fixture::format(format);
        std::env::set_var("POSEIDON_POLICY_ROOT", &f.root);
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "working\n");
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        let (id, g) = (r.session_id, r.generation);
        s.set_trust(id, g, true).await.unwrap();
        native(
            &f,
            &real,
            &["config", "filter.late.clean", ": > FILTER_ALLOWED; cat"],
        );
        f.write(".gitattributes", "a filter=late\n");
        let w = s.working_changes(id, g).await.unwrap().data;
        let selected = w
            .unstaged
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .index;
        let before = std::fs::read(f.root.join(".git/index")).unwrap();
        let error = s
            .stage_file(id, g, w.revision, WorkingArea::Unstaged, selected)
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::TrustRequired));
        assert!(!f.root.join("FILTER_ALLOWED").exists());
        assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), before);
        s.set_trust(id, g, false).await.unwrap();
        s.set_trust(id, g, true).await.unwrap();
        let w = s.working_changes(id, g).await.unwrap().data;
        let selected = w
            .unstaged
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .index;
        s.stage_file(id, g, w.revision, WorkingArea::Unstaged, selected)
            .await
            .unwrap();
        assert!(
            f.root.join("FILTER_ALLOWED").exists(),
            "explicit reauthorization did not preserve filter semantics"
        );

        // A boolean hook grant must not authorize a newly introduced script.
        s.set_commit_hook_trust(id, g, true).await.unwrap();
        hook(&f, "pre-commit");
        let w = s.working_changes(id, g).await.unwrap().data;
        let plan = s.prepare_commit(id, g, w.revision).await.unwrap().data;
        assert!(!plan.hooks_trusted);
        let error = s
            .create_commit(id, g, plan.token, "refused new hook")
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::TrustRequired));
        assert!(!f.root.join("PROGRAM_RAN").exists());

        // Predictable legacy temp names must never truncate a symlink target.
        let app_data = f.temp.path().join("app-data");
        std::fs::create_dir(&app_data).unwrap();
        let victim = f.temp.path().join("precious");
        std::fs::write(&victim, b"preserve bytes").unwrap();
        let old_temp = app_data.join(format!(".trust-{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&victim, &old_temp).unwrap();
        let t = RepositoryService::with_trust(poseidon_core::TrustStore::file(
            app_data.join("trust.json"),
        ));
        let r = t.open(&f.root).await.unwrap();
        t.set_trust(r.session_id, r.generation, true).await.unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve bytes");
        assert!(old_temp.is_symlink());
    }
    {
        let f = Fixture::new();
        std::env::set_var("POSEIDON_POLICY_ROOT", &f.root);
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "working\n");
        f.write(".gitattributes", "a filter=safe\n");
        native(&f, &real, &["config", "filter.safe.clean", "bash -c cat"]);
        let startup = f.temp.path().join("startup");
        std::fs::write(&startup, "printf injected > ENV_RAN\n").unwrap();
        assert!(std::process::Command::new("/usr/bin/bash")
            .current_dir(&f.root)
            .args(["-c", "true"])
            .env("BASH_ENV", &startup)
            .status()
            .unwrap()
            .success());
        assert!(f.root.join("ENV_RAN").exists());
        std::fs::remove_file(f.root.join("ENV_RAN")).unwrap();
        let other = Fixture::new();
        let other_index = std::fs::read(other.root.join(".git/HEAD")).unwrap();
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        let (id, g) = (r.session_id, r.generation);
        s.set_trust(id, g, true).await.unwrap();
        let w = s.working_changes(id, g).await.unwrap().data;
        let selected = w
            .unstaged
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .index;
        std::env::set_var("BASH_ENV", &startup);
        std::env::set_var("GIT_DIR", other.root.join(".git"));
        std::env::set_var("GIT_WORK_TREE", &other.root);
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        std::env::set_var("GIT_CONFIG_KEY_0", "filter.safe.clean");
        std::env::set_var("GIT_CONFIG_VALUE_0", ": > PROGRAM_RAN; cat");
        s.stage_file(id, g, w.revision, WorkingArea::Unstaged, selected)
            .await
            .unwrap();
        assert!(!f.root.join("ENV_RAN").exists());
        assert!(!f.root.join("PROGRAM_RAN").exists());
        assert_eq!(f.git(&["show", ":a"]), "working");
        assert_eq!(
            std::fs::read(other.root.join(".git/HEAD")).unwrap(),
            other_index
        );
        assert!(!other.root.join(".git/index").exists());
        for key in [
            "BASH_ENV",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
        ] {
            std::env::remove_var(key);
        }
    }
}
