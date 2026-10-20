//! Local-only branch latency, excluding fixture setup; small and 10k-commit histories.
use poseidon_core::{domain::*, RepositoryService};
use serde_json::json;
use std::{collections::BTreeMap, time::Instant};
#[path = "../tests/support/mod.rs"]
mod support;
use support::Fixture;
fn oid(f: &Fixture, r: &str) -> String {
    f.git(&["rev-parse", r]).trim().into()
}
fn stats(mut values: Vec<u32>) -> serde_json::Value {
    values.sort();
    json!({"samples":values.len(),"p50_ms":values[values.len().div_ceil(2)-1],"p95_ms":values[(values.len()*95).div_ceil(100)-1],"max_ms":values.last()})
}
fn memory() -> serde_json::Value {
    let value = |path: &str, key: &str| {
        std::fs::read_to_string(path).ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse::<u64>().ok())
        })
    };
    json!({"rss_kib":value("/proc/self/status","VmRSS:"),"pss_kib":value("/proc/self/smaps_rollup","Pss:")})
}
#[tokio::main]
async fn main() {
    let mut reports = vec![];
    for (commits, files) in [(10, 10), (10_000, 1_000)] {
        let f = Fixture::scale(commits, files, false);
        let s = RepositoryService::default();
        let mut r = s.open(&f.root).await.unwrap();
        s.set_trust(r.session_id, r.generation, true).await.unwrap();
        let main = oid(&f, "HEAD");
        let target = oid(&f, "HEAD~1");
        let memory_before = memory();
        let mut rows: BTreeMap<&str, Vec<(u32, u32, u32, u32)>> = BTreeMap::new();
        for _ in 0..10 {
            for (kind, action) in [
                (
                    "create",
                    BranchAction::Create {
                        name: "bench".into(),
                        start_oid: target.clone(),
                    },
                ),
                (
                    "switch",
                    BranchAction::Switch {
                        name: "bench".into(),
                        expected_oid: target.clone(),
                    },
                ),
                (
                    "rename",
                    BranchAction::Rename {
                        name: "bench".into(),
                        expected_oid: target.clone(),
                        new_name: "renamed".into(),
                    },
                ),
                (
                    "switch",
                    BranchAction::Switch {
                        name: "main".into(),
                        expected_oid: main.clone(),
                    },
                ),
                (
                    "delete",
                    BranchAction::Delete {
                        name: "renamed".into(),
                        expected_oid: target.clone(),
                    },
                ),
            ] {
                let t = Instant::now();
                let plan = s
                    .prepare_branch(r.session_id, r.generation, action)
                    .await
                    .unwrap()
                    .data;
                assert!(plan.block.is_none(), "{:?}", plan.block);
                let prepare = t.elapsed().as_millis() as u32;
                let change = s
                    .execute_branch(r.session_id, r.generation, plan.token)
                    .await
                    .unwrap()
                    .data;
                assert_eq!(change.outcome, BranchOutcome::Applied, "{}", change.message);
                rows.entry(kind).or_default().push((
                    prepare,
                    change.git_ms,
                    change.refresh_ms,
                    change.elapsed_ms,
                ));
                r = change.snapshot.unwrap();
                match &r.data.head {
                    Head::Attached {
                        name,
                        oid: expected,
                    } => {
                        assert_eq!(oid(&f, "HEAD"), *expected);
                        assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]).trim(), name);
                    }
                    _ => panic!("unexpected HEAD"),
                }
            }
        }
        assert_eq!(oid(&f, "HEAD"), main);
        assert!(!f
            .output(&["show-ref", "--verify", "refs/heads/renamed"])
            .status
            .success());
        let summary:BTreeMap<_,_>=rows.iter().map(|(k,v)|(*k,json!({"prepare":stats(v.iter().map(|r|r.0).collect()),"git":stats(v.iter().map(|r|r.1).collect()),"refresh":stats(v.iter().map(|r|r.2).collect()),"execute":stats(v.iter().map(|r|r.3).collect())}))).collect();
        reports.push(json!({"commits":commits,"files":files,"operations":summary,"backend_memory_before":memory_before,"backend_memory_after":memory()}));
    }
    println!("{}",serde_json::to_string_pretty(&json!({"method":"Release Rust; ten rounds per corpus; setup excluded; includes preflight double-read in prepare, Git subprocess and bounded page-zero reconciliation in execute. Each final HEAD verified independently with Git. No GUI/paint timings.","reports":reports})).unwrap());
}
