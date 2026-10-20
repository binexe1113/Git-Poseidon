//! Real commits, only owned temporary fixtures. cargo run --release -p poseidon-core --features bench --example commit_benchmark
use poseidon_core::{domain::CommitOutcome, RepositoryService};
use serde_json::json;
#[path = "../tests/support/mod.rs"]
mod support;
use support::Fixture;
fn stats(mut data: Vec<u32>) -> serde_json::Value {
    data.sort_unstable();
    json!({"samples":data.len(),"p50_ms":data[(data.len()*50).div_ceil(100)-1],"p95_ms":data[(data.len()*95).div_ceil(100)-1],"max_ms":data.last()})
}
#[tokio::main]
async fn main() {
    let mut reports = Vec::new();
    for (files, hook) in [(1, false), (500, false), (1, true)] {
        let mut rows = Vec::new();
        for sample in 0..10 {
            let f = Fixture::new();
            f.write("base", "base");
            f.commit("base");
            for index in 0..files {
                f.write(&format!("file-{index}.txt"), "prepared\n");
            }
            f.git(&["add", "-A"]);
            if hook {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let path = f.root.join(".git/hooks/pre-commit");
                    std::fs::write(&path, "#!/bin/sh\nsleep 0.05\n").unwrap();
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
                }
            }
            let s = RepositoryService::default();
            let o = s.open(&f.root).await.unwrap();
            let (id, g) = (o.session_id, o.generation);
            s.set_trust(id, g, true).await.unwrap();
            if hook {
                s.set_commit_hook_trust(id, g, true).await.unwrap();
            }
            let w = s.working_changes(id, g).await.unwrap().data;
            let p = s.prepare_commit(id, g, w.revision).await.unwrap().data;
            assert!(p.block.is_none(), "{:?}", p.block);
            let r = s
                .create_commit(id, g, p.token, &format!("Sample {sample}"))
                .await
                .unwrap()
                .data;
            assert_eq!(r.outcome, CommitOutcome::Created, "{}", r.message);
            rows.push(json!({"invalidation_us":r.invalidation_us,"git_ms":r.git_ms,"hooks_ms":r.hooks_ms,"refresh_ms":r.refresh_ms,"total_ms":r.elapsed_ms}));
        }
        let column = |key: &str| {
            rows.iter()
                .map(|r| r[key].as_u64().unwrap() as u32)
                .collect()
        };
        reports.push(json!({"files":files,"hook_delay_ms":if hook{50}else{0},"git_excluding_wrappers":stats(column("git_ms")),"hook_wrappers_and_delegated_hooks":stats(column("hooks_ms")),"refresh":stats(column("refresh_ms")),"cache_invalidation_raw_us":column("invalidation_us"),"total":stats(column("total_ms")),"samples":rows}));
    }
    println!("{}", serde_json::to_string_pretty(&reports).unwrap());
}
