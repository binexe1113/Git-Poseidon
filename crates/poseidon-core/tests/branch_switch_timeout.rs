//! Exercise the production 120-second deadline, without shortening its policy.
#![cfg(unix)]
use poseidon_core::{domain::*, RepositoryService};
use std::{os::unix::fs::PermissionsExt, time::Instant};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;

fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.write("local", "base\n");
    f.commit("base");
    f.git(&["switch", "-c", "target"]);
    f.write("a", "target\n");
    f.commit("target");
    f.git(&["switch", "main"]);
    f.write("local", "staged\n");
    f.git(&["add", "local"]);
    f.write("local", "working\n");
    let hook = f.root.join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nprintf 'ran\\n' >> ../calls\nsleep 125\n").unwrap();
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    f
}

#[tokio::test]
async fn real_switch_deadline_reconciles_completed_checkout_without_retry() {
    let f = fixture();
    let twin = fixture();
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    s.set_branch_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let p = s
        .prepare_branch(
            r.session_id,
            r.generation,
            BranchAction::Switch {
                name: "target".into(),
                expected_oid: f.git(&["rev-parse", "target"]),
            },
        )
        .await
        .unwrap()
        .data;
    assert!(p.block.is_none());
    let start = Instant::now();
    let native = tokio::task::spawn_blocking(move || {
        // Same argv/environment as owned fixture Git, supervised by coreutils timeout.
        let template = twin.command();
        let mut cmd = std::process::Command::new("timeout");
        cmd.current_dir(&twin.root);
        for (key, value) in template.get_envs() {
            if let Some(value) = value {
                cmd.env(key, value);
            } else {
                cmd.env_remove(key);
            }
        }
        let output = cmd
            .args([
                "--signal=TERM",
                "--kill-after=2",
                "120",
                "git",
                "switch",
                "target",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(124));
        assert_eq!(twin.git(&["symbolic-ref", "--short", "HEAD"]), "target");
        assert_eq!(twin.git(&["show", ":local"]), "staged");
        assert_eq!(
            std::fs::read(twin.root.join("local")).unwrap(),
            b"working\n"
        );
        assert_eq!(
            std::fs::read_to_string(twin.temp.path().join("calls")).unwrap(),
            "ran\n"
        );
    });
    let result = s
        .execute_branch(r.session_id, r.generation, p.token)
        .await
        .unwrap();
    native.await.unwrap();
    assert!(start.elapsed().as_secs() >= 120);
    assert_eq!(result.data.outcome, BranchOutcome::Unknown);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "target");
    assert_eq!(f.git(&["show", ":local"]), "staged");
    assert_eq!(std::fs::read(f.root.join("local")).unwrap(), b"working\n");
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("calls")).unwrap(),
        "ran\n"
    );
    assert!(s
        .execute_branch(r.session_id, r.generation, p.token)
        .await
        .is_err());
    println!("T221_TIMEOUT production=120s native=124 poseidon=Unknown head=target index_valid=true local_preserved=true calls=1 elapsed={:?}",start.elapsed());
}

fn rename_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("a", "base\n");
    f.commit("base");
    f.git(&["branch", "--track", "victim", "main"]);
    let hook = f.root.join(".git/hooks/reference-transaction");
    std::fs::write(
        &hook,
        "#!/bin/sh\nif [ \"$1\" = prepared ]; then printf 'ran\\n' >> ../calls; sleep 125; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    f
}

fn rename_state(f: &Fixture) -> String {
    let git = f.root.join(".git");
    let exists = |p: &str| git.join(p).exists();
    format!(
        "victim={} renamed={} victim_log={} renamed_log={} tmp_renamed_log={} victim_lock={} renamed_lock={}",
        f.output(&["show-ref", "--verify", "--quiet", "refs/heads/victim"]).status.success(),
        f.output(&["show-ref", "--verify", "--quiet", "refs/heads/renamed"]).status.success(),
        exists("logs/refs/heads/victim"),
        exists("logs/refs/heads/renamed"),
        exists("logs/refs/.tmp-renamed-log"),
        exists("refs/heads/victim.lock"),
        exists("refs/heads/renamed.lock"),
    )
}

#[tokio::test]
async fn real_rename_deadline_leaves_no_orphan_destination_reflog() {
    let f = rename_fixture();
    let twin = rename_fixture();
    let oid = f.git(&["rev-parse", "victim"]);
    let log = std::fs::read(f.root.join(".git/logs/refs/heads/victim")).unwrap();
    let s = RepositoryService::default();
    let r = s.open(&f.root).await.unwrap();
    s.set_trust(r.session_id, r.generation, true).await.unwrap();
    s.set_branch_hook_trust(r.session_id, r.generation, true)
        .await
        .unwrap();
    let p = s
        .prepare_branch(
            r.session_id,
            r.generation,
            BranchAction::Rename {
                name: "victim".into(),
                expected_oid: oid.clone(),
                new_name: "renamed".into(),
            },
        )
        .await
        .unwrap()
        .data;
    assert!(p.block.is_none(), "{:?}", p.block);
    let start = Instant::now();
    let native = tokio::task::spawn_blocking(move || {
        // Same deadline and signal as the production runner: SIGKILL to the group.
        use std::os::unix::process::CommandExt;
        let mut cmd = twin.command();
        let mut child = cmd
            .args(["branch", "-m", "victim", "renamed"])
            .process_group(0)
            .spawn()
            .unwrap();
        while child.try_wait().unwrap().is_none() && start.elapsed().as_secs() < 120 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .status()
            .unwrap();
        let status = child.wait().unwrap();
        assert_eq!(
            std::fs::read_to_string(twin.temp.path().join("calls")).unwrap(),
            "ran\n"
        );
        println!(
            "T221_RENAME_TIMEOUT native status={status:?} {}",
            rename_state(&twin)
        );
    });
    let result = s
        .execute_branch(r.session_id, r.generation, p.token)
        .await
        .unwrap();
    native.await.unwrap();
    println!(
        "T221_RENAME_TIMEOUT poseidon outcome={:?} elapsed={:?} {}",
        result.data.outcome,
        start.elapsed(),
        rename_state(&f)
    );
    assert!(start.elapsed().as_secs() >= 120);
    assert_eq!(result.data.outcome, BranchOutcome::Unknown);
    assert_eq!(f.git(&["rev-parse", "refs/heads/victim"]), oid);
    assert!(!f
        .output(&["show-ref", "--verify", "--quiet", "refs/heads/renamed"])
        .status
        .success());
    assert_eq!(
        std::fs::read(f.root.join(".git/logs/refs/heads/victim")).unwrap(),
        log
    );
    // The destination history is seeded only after the authorized hook returns.
    assert!(!f.root.join(".git/logs/refs/heads/renamed").exists());
    assert_eq!(f.git(&["config", "branch.victim.merge"]), "refs/heads/main");
    assert!(!f
        .output(&["config", "branch.renamed.merge"])
        .status
        .success());
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join("calls")).unwrap(),
        "ran\n"
    );
    assert!(s
        .execute_branch(r.session_id, r.generation, p.token)
        .await
        .is_err());
}
