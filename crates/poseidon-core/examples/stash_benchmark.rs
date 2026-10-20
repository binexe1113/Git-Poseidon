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
        let s = RepositoryService::default();
        let mut r = s.open(&f.root).await.unwrap();
        s.set_trust(r.session_id, r.generation, true).await.unwrap();
        let before = memory();
        for round in 0..10 {
            for i in 0..count {
                f.write(&format!("file-{i}"), "dirty\n");
            }
            // 500 is the explicit safety-preview limit, tested as a refusal for larger cases.
            let a = StashAction::Create {
                message: format!("bench {round}"),
                include_untracked: false,
            };
            let t = Instant::now();
            let p = s
                .prepare_stash(r.session_id, r.generation, a)
                .await
                .unwrap()
                .data;
            let prepare_ms = t.elapsed().as_secs_f64() * 1000.;
            if let Some(block) = p.block {
                samples.push(serde_json::json!({"files":count,"round":round,"operation":"blocked_preview","prepare_ms":prepare_ms,"reason":block}));
                break;
            }
            let c = s
                .execute_stash(r.session_id, r.generation, p.token)
                .await
                .unwrap()
                .data;
            assert_eq!(c.outcome, BranchOutcome::Applied);
            r = c.snapshot.unwrap();
            samples.push(serde_json::json!({"files":count,"round":round,"operation":"create","prepare_ms":prepare_ms,"git_ms":c.git_ms,"execute_ms":c.elapsed_ms}));
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
            for (name, a) in [
                (
                    "apply",
                    StashAction::Apply {
                        selection: selection.clone(),
                        restore_index: false,
                    },
                ),
                ("drop", StashAction::Drop { selection }),
            ] {
                let selection = s
                    .stashes(r.session_id, r.generation)
                    .await
                    .unwrap()
                    .data
                    .entries[0]
                    .selection
                    .clone();
                let a = match a {
                    StashAction::Apply { restore_index, .. } => StashAction::Apply {
                        selection,
                        restore_index,
                    },
                    _ => StashAction::Drop { selection },
                };
                let t = Instant::now();
                let p = s
                    .prepare_stash(r.session_id, r.generation, a)
                    .await
                    .unwrap()
                    .data;
                let prepare_ms = t.elapsed().as_secs_f64() * 1000.;
                assert!(p.block.is_none());
                let c = s
                    .execute_stash(r.session_id, r.generation, p.token)
                    .await
                    .unwrap()
                    .data;
                assert_eq!(c.outcome, BranchOutcome::Applied);
                r = c.snapshot.unwrap();
                samples.push(serde_json::json!({"files":count,"round":round,"operation":name,"prepare_ms":prepare_ms,"git_ms":c.git_ms,"execute_ms":c.elapsed_ms}));
            }
        }
        samples.push(serde_json::json!({"files":count,"operation":"memory","before":before,"after":memory()}));
    }
    println!("{}", serde_json::to_string_pretty(&samples).unwrap());
}
