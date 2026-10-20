#[path = "support/mod.rs"]
mod support;
use support::{Fixture, Scenario};

#[test]
fn scenarios_have_reproducible_objects_topology_and_status() {
    for scenario in [
        Scenario::Empty,
        Scenario::Initial,
        Scenario::Linear,
        Scenario::Divergent,
        Scenario::Dirty,
        Scenario::Unicode,
        Scenario::Detached,
    ] {
        let a = Fixture::scenario(scenario);
        let b = Fixture::scenario(scenario);
        for args in [
            vec!["show-ref", "--head"],
            vec!["log", "--all", "--format=%H %P %s"],
            vec!["status", "--porcelain=v2", "-z"],
        ] {
            let left = a.output(&args);
            let right = b.output(&args);
            assert_eq!(left.status.code(), right.status.code(), "{scenario:?}");
            assert_eq!(left.stdout, right.stdout, "{scenario:?}");
        }
        match scenario {
            Scenario::Empty => assert!(!a
                .output(&["rev-parse", "--verify", "HEAD"])
                .status
                .success()),
            Scenario::Initial | Scenario::Unicode => {
                assert_eq!(a.git(&["rev-list", "--all", "--count"]), "1")
            }
            Scenario::Linear => assert_eq!(a.git(&["rev-list", "--all", "--count"]), "4"),
            Scenario::Divergent => assert_eq!(
                a.git(&["rev-list", "--left-right", "--count", "main...feature"]),
                "3\t1"
            ),
            Scenario::Dirty => {
                let status = a.git(&["status", "--porcelain"]);
                assert!(status.contains("MM linear.txt") && status.contains("?? untracked.txt"));
            }
            Scenario::Detached => {
                assert!(!a.output(&["symbolic-ref", "-q", "HEAD"]).status.success())
            }
        }
        assert!(a.git(&["ls-files"]).is_empty() || !matches!(scenario, Scenario::Empty));
    }
}

#[test]
fn rejects_root_outside_owned_temporary_directory() {
    let outside = tempfile::tempdir().unwrap();
    let mut f = Fixture::new();
    f.root = outside.path().to_owned();
    assert!(std::panic::catch_unwind(|| f.command()).is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn rejects_path_traversal_and_symlink_writes() {
    let f = Fixture::new();
    assert!(std::panic::catch_unwind(|| f.write("../escape.txt", "bad")).is_err());
    #[cfg(unix)]
    {
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), f.root.join("link")).unwrap();
        assert!(std::panic::catch_unwind(|| f.write("link", "bad")).is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"");
    }
}

#[tokio::test]
async fn runner_passes_hostile_arguments_literally() {
    let f = Fixture::scenario(Scenario::Initial);
    let name = "$(literal) ; ' arquivo 日本.txt";
    f.write(name, "literal bytes\n");
    f.commit("hostile path");
    let argument = format!("HEAD:{name}");
    let result = poseidon_core::process::GitProcess::discover()
        .unwrap()
        .run(
            &f.root,
            &["show", "--no-ext-diff", "--no-textconv", &argument],
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.stdout, b"literal bytes\n");
}
