use poseidon_core::{
    domain::{CommitOutcome, ErrorCode},
    RepositoryService,
};
#[path = "support/mod.rs"]
mod support;
use support::Fixture;

async fn ready(f: &Fixture) -> (RepositoryService, u32, u32) {
    let service = RepositoryService::default();
    let s = service.open(&f.root).await.unwrap();
    service
        .set_trust(s.session_id, s.generation, true)
        .await
        .unwrap();
    (service, s.session_id, s.generation)
}
async fn plan(service: &RepositoryService, id: u32, g: u32) -> poseidon_core::domain::CommitPlan {
    let w = service.working_changes(id, g).await.unwrap().data;
    service
        .prepare_commit(id, g, w.revision)
        .await
        .unwrap()
        .data
}
#[tokio::test]
async fn initial_and_normal_commits_use_only_index_and_multiline_message() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::format(format);
        f.write("file.txt", "prepared\n");
        f.git(&["add", "file.txt"]);
        f.write("file.txt", "working\n");
        let (s, id, g) = ready(&f).await;
        let p = plan(&s, id, g).await;
        assert!(p.block.is_none(), "{:?}", p.block);
        let r = s
            .create_commit(
                id,
                g,
                p.token,
                "Subject $(not-a-shell)\n\nBody one\nBody two\n",
            )
            .await
            .unwrap()
            .data;
        assert_eq!(r.outcome, CommitOutcome::Created, "{}", r.message);
        assert_eq!(f.git(&["show", "HEAD:file.txt"]), "prepared");
        assert_eq!(
            std::fs::read(f.root.join("file.txt")).unwrap(),
            b"working\n"
        );
        assert!(f
            .git(&["log", "-1", "--format=%B"])
            .contains("Body one\nBody two"));
        let snap = r.snapshot.unwrap();
        assert_eq!(snap.data.commits[0].oid, r.oid.unwrap());
        assert_eq!(snap.data.graph[0].oid, snap.data.commits[0].oid);
        let p = plan(&s, id, snap.generation).await;
        assert!(p.block.unwrap().contains("Nenhum"));
        f.git(&["add", "file.txt"]);
        let p = plan(&s, id, snap.generation).await;
        let r = s
            .create_commit(id, snap.generation, p.token, "Second")
            .await
            .unwrap()
            .data;
        assert_eq!(r.outcome, CommitOutcome::Created);
        assert_eq!(r.snapshot.unwrap().data.commits.len(), 2);
    }
}
#[tokio::test]
async fn absent_identity_signing_empty_and_invalid_messages_are_blocked() {
    let f = Fixture::new();
    f.write("a", "a");
    f.git(&["add", "a"]);
    let (s, id, g) = ready(&f).await;
    for message in ["", " \nbody", "a\0b"] {
        let p = plan(&s, id, g).await;
        assert!(matches!(
            s.create_commit(id, g, p.token, message)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        ));
    }
    f.git(&["config", "user.email", ""]);
    let p = plan(&s, id, g).await;
    assert!(p.block.unwrap().contains("Identidade"));
    f.git(&["config", "user.email", "test@example.invalid"]);
    for format in ["openpgp", "ssh"] {
        f.git(&["config", "commit.gpgsign", "true"]);
        f.git(&["config", "gpg.format", format]);
        let p = plan(&s, id, g).await;
        assert!(p.signing.is_some());
        assert!(s
            .create_commit(id, g, p.token, "Blocked signing")
            .await
            .is_err());
    }
    assert!(!f.root.join(".git/refs/heads/main").exists());
}
#[tokio::test]
async fn stale_head_index_config_and_duplicate_tokens_never_write() {
    let f = Fixture::new();
    f.write("a", "base");
    f.commit("base");
    f.write("a", "changed");
    f.git(&["add", "a"]);
    let (s, id, g) = ready(&f).await;
    let p = plan(&s, id, g).await;
    f.write("b", "b");
    f.git(&["add", "b"]);
    let before = f.git(&["rev-parse", "HEAD"]);
    assert!(matches!(
        s.create_commit(id, g, p.token, "old index")
            .await
            .unwrap_err()
            .code,
        ErrorCode::WorkingTreeChanged
    ));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    let p = plan(&s, id, g).await;
    f.git(&["config", "user.name", "Changed identity"]);
    assert!(s
        .create_commit(id, g, p.token, "old identity")
        .await
        .is_err());
    let p = plan(&s, id, g).await;
    let r = s.create_commit(id, g, p.token, "new commit").await.unwrap();
    assert_eq!(r.data.outcome, CommitOutcome::Created);
    assert!(s.create_commit(id, g, p.token, "duplicate").await.is_err());
    assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), "2");
}
#[cfg(unix)]
fn hook(f: &Fixture, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(f.root.join("custom-hooks")).unwrap();
    f.git(&["config", "core.hooksPath", "custom-hooks"]);
    let path = f.root.join("custom-hooks").join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn hooks_need_separate_consent_and_revocation_is_enforced() {
    let f = Fixture::new();
    f.write("a", "a");
    f.git(&["add", "a"]);
    hook(&f, "pre-commit", "echo ran > hook-ran");
    let (s, id, g) = ready(&f).await;
    let p = plan(&s, id, g).await;
    assert!(!p.hooks_trusted);
    assert!(matches!(
        s.create_commit(id, g, p.token, "no consent")
            .await
            .unwrap_err()
            .code,
        ErrorCode::TrustRequired
    ));
    assert!(!f.root.join("hook-ran").exists());
    s.set_commit_hook_trust(id, g, true).await.unwrap();
    let p = plan(&s, id, g).await;
    let r = s
        .create_commit(id, g, p.token, "consented")
        .await
        .unwrap()
        .data;
    assert_eq!(r.outcome, CommitOutcome::Created, "{}", r.message);
    assert!(f.root.join("hook-ran").exists());
    let g = r.snapshot.unwrap().generation;
    f.write("a", "b");
    f.git(&["add", "a"]);
    s.set_commit_hook_trust(id, g, false).await.unwrap();
    let p = plan(&s, id, g).await;
    assert!(s.create_commit(id, g, p.token, "revoked").await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn rejecting_and_index_modifying_hooks_preserve_draft_and_require_review() {
    for (name, body) in [
        ("pre-commit", "exit 1"),
        ("prepare-commit-msg", "echo modified > a; git add a"),
        ("commit-msg", "echo added > extra; git add extra"),
    ] {
        let f = Fixture::new();
        f.write("a", "a");
        f.git(&["add", "a"]);
        hook(&f, name, body);
        let (s, id, g) = ready(&f).await;
        s.set_commit_hook_trust(id, g, true).await.unwrap();
        let p = plan(&s, id, g).await;
        let r = s
            .create_commit(id, g, p.token, "must reject")
            .await
            .unwrap()
            .data;
        assert_eq!(r.outcome, CommitOutcome::Rejected, "{name}: {}", r.message);
        assert!(!f.root.join(".git/refs/heads/main").exists());
    }
}
#[cfg(unix)]
#[tokio::test]
async fn post_commit_failure_is_reported_as_created_and_worktree_hook_changes_remain() {
    let f = Fixture::new();
    f.write("a", "a");
    f.git(&["add", "a"]);
    hook(&f, "pre-commit", "echo editing > a");
    hook(&f, "post-commit", "echo completed > post-ran; exit 1");
    let (s, id, g) = ready(&f).await;
    s.set_commit_hook_trust(id, g, true).await.unwrap();
    let p = plan(&s, id, g).await;
    let r = s
        .create_commit(id, g, p.token, "post failed")
        .await
        .unwrap()
        .data;
    assert_eq!(r.outcome, CommitOutcome::Created, "{}", r.message);
    assert!(r.message.contains("falha"));
    assert_eq!(f.git(&["show", "HEAD:a"]), "a");
    assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"editing\n");
    assert!(f.root.join("post-ran").exists());
}

#[tokio::test]
async fn stale_head_and_git_locks_are_refused_without_touching_files() {
    let f = Fixture::new();
    f.write("a", "a");
    f.commit("base");
    f.write("a", "b");
    f.git(&["add", "a"]);
    let (s, id, g) = ready(&f).await;
    let p = plan(&s, id, g).await;
    f.git(&["commit", "-m", "external"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    assert!(matches!(
        s.create_commit(id, g, p.token, "stale head")
            .await
            .unwrap_err()
            .code,
        ErrorCode::WorkingTreeChanged
    ));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    f.write("a", "c");
    f.git(&["add", "a"]);
    let p = plan(&s, id, g).await;
    let index = std::fs::read(f.root.join(".git/index")).unwrap();
    std::fs::write(f.root.join(".git/index.lock"), "external lock").unwrap();
    assert!(matches!(
        s.create_commit(id, g, p.token, "locked")
            .await
            .unwrap_err()
            .code,
        ErrorCode::IndexLocked
    ));
    assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), index);
    assert_eq!(
        std::fs::read(f.root.join(".git/index.lock")).unwrap(),
        b"external lock"
    );
}

#[tokio::test]
async fn hook_authorization_is_persisted_separately_and_removed_with_base_trust() {
    use poseidon_core::TrustStore;
    let f = Fixture::new();
    f.write("a", "a");
    f.git(&["add", "a"]);
    let path = f.temp.path().join("app/trust.json");
    let s = RepositoryService::with_trust(TrustStore::file(path.clone()));
    let snap = s.open(&f.root).await.unwrap();
    let (id, g) = (snap.session_id, snap.generation);
    assert!(s.set_commit_hook_trust(id, g, true).await.is_err());
    s.set_trust(id, g, true).await.unwrap();
    assert!(!plan(&s, id, g).await.hooks_trusted);
    s.set_commit_hook_trust(id, g, true).await.unwrap();
    let other = RepositoryService::with_trust(TrustStore::file(path));
    let snap = other.open(&f.root).await.unwrap();
    assert!(
        plan(&other, snap.session_id, snap.generation)
            .await
            .hooks_trusted
    );
    s.set_trust(id, g, false).await.unwrap();
    s.set_trust(id, g, true).await.unwrap();
    assert!(
        !plan(&other, snap.session_id, snap.generation)
            .await
            .hooks_trusted
    );
}

#[cfg(unix)]
#[tokio::test]
async fn disabled_hooks_and_symref_head_changes_are_handled_safely() {
    let f = Fixture::new();
    f.write("a", "a");
    f.commit("base");
    f.git(&["config", "core.hooksPath", "/dev/null"]);
    f.write("a", "b");
    f.git(&["add", "a"]);
    let (s, id, g) = ready(&f).await;
    let p = plan(&s, id, g).await;
    let r = s
        .create_commit(id, g, p.token, "disabled hooks")
        .await
        .unwrap()
        .data;
    assert_eq!(r.outcome, CommitOutcome::Created);
    let g = r.snapshot.unwrap().generation;
    // HEAD points through an additional symbolic ref whose file does not change.
    f.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    f.git(&["symbolic-ref", "HEAD", "refs/heads/alias"]);
    f.write("a", "c");
    f.git(&["add", "a"]);
    let p = plan(&s, id, g).await;
    f.git(&["update-ref", "refs/heads/main", "HEAD~1"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    assert!(s
        .create_commit(id, g, p.token, "stale symbolic head")
        .await
        .is_err());
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}
