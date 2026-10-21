//! Reproducible, owned Git fixtures; no writes to a user repository.
use poseidon_core::{domain::*, RepositoryService};
#[path = "../tests/support/mod.rs"]
mod support;
use std::time::Instant;
use support::Fixture;
fn memory() -> serde_json::Value {
    let read = |path: &str, key: &str| {
        std::fs::read_to_string(path).ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse::<u64>().ok())
        })
    };
    serde_json::json!({"rss_kib":read("/proc/self/status","VmRSS:"),"pss_kib":read("/proc/self/smaps_rollup","Pss:")})
}
#[tokio::main]
async fn main() {
    let mut samples = Vec::new();
    for count in [10, 500, 1000] {
        let f = Fixture::new();
        for i in 0..count {
            f.write(&format!("file-{i}"), "base\n");
        }
        f.commit("base");
        for i in 0..count {
            f.write(&format!("file-{i}"), "dirty\n");
        }
        f.git(&["stash", "push", "-m", "benchmark fixture"]);
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        let before = memory();
        for round in 0..10 {
            let t = Instant::now();
            let list = s.stashes(r.session_id, r.generation).await.unwrap();
            let list_ms = t.elapsed().as_secs_f64() * 1000.;
            let selection = list.data.entries[0].selection.clone();
            let t = Instant::now();
            s.stash_details(r.session_id, r.generation, selection.clone())
                .await
                .unwrap();
            let details_ms = t.elapsed().as_secs_f64() * 1000.;
            samples.push(serde_json::json!({"files":count,"round":round,"operation":"read","list_ms":list_ms,"details_ms":details_ms}));
            for action in [
                StashAction::Create {
                    message: "unavailable".into(),
                    include_untracked: true,
                },
                StashAction::Apply {
                    selection: selection.clone(),
                    restore_index: true,
                },
                StashAction::Drop { selection },
            ] {
                let t = Instant::now();
                let err = s
                    .prepare_stash(r.session_id, r.generation, action)
                    .await
                    .unwrap_err();
                assert!(matches!(err.code, ErrorCode::UnsupportedState));
                samples.push(serde_json::json!({"files":count,"round":round,"operation":"refused_without_git","elapsed_ms":t.elapsed().as_secs_f64()*1000.}));
            }
        }
        samples.push(serde_json::json!({"files":count,"operation":"memory","before":before,"after":memory()}));
    }
    println!("{}", serde_json::to_string_pretty(&samples).unwrap());
}
