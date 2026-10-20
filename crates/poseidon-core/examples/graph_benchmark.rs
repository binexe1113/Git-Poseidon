#[path = "../tests/support/mod.rs"]
mod support;
use poseidon_core::{
    benchmark::parse_commits, graph::Layout, process::GitProcess, RepositoryService,
};
use serde_json::json;
use std::{hint::black_box, time::Instant};
fn stats(mut times: Vec<u128>) -> serde_json::Value {
    times.sort_unstable();
    json!({"samples":times.len(),"p50_ns":times[(times.len()-1)/2],"p95_ns":times[(times.len()*95).div_ceil(100)-1],"min_ns":times[0],"max_ns":times[times.len()-1]})
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    for (count, merges) in [(1000, false), (10000, false), (10000, true)] {
        let f = support::Fixture::scale(count, 1000, merges);
        let output = GitProcess::discover()?
            .run(
                &f.root,
                &[
                    "log",
                    "--all",
                    "--topo-order",
                    "-z",
                    "--format=%H%x00%T%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s",
                    "--",
                ],
            )
            .await?;
        assert!(output.success);
        let commits = parse_commits(&output.stdout)?;
        assert_eq!(commits.len(), count);
        let mut samples = Vec::new();
        let mut segments = 0;
        let mut lanes = 0;
        for i in 0..35 {
            let mut layout = Layout::default();
            let start = Instant::now();
            let rows = commits
                .iter()
                .map(|c| layout.push(c))
                .collect::<Result<Vec<_>, _>>()?;
            let elapsed = start.elapsed().as_nanos();
            if i >= 5 {
                samples.push(elapsed);
            }
            segments = rows.iter().map(|r| r.segments.len()).sum::<usize>();
            lanes = rows.iter().map(|r| r.width).max().unwrap_or(0);
            black_box(&rows);
        }
        let mut first = Vec::new();
        let mut traverse = Vec::new();
        let mut maxcache = 0;
        let mut max_payload = 0;
        for i in 0..6 {
            let s = RepositoryService::default();
            let start = Instant::now();
            let opened = s.open(&f.root).await?;
            let first_time = start.elapsed().as_nanos();
            let start = Instant::now();
            let mut offset = 0;
            let mut seen = 0;
            loop {
                let page = s
                    .history_page(opened.session_id, opened.generation, offset)
                    .await?
                    .data;
                seen += page.commits.len();
                maxcache = maxcache.max(page.cache_bytes);
                max_payload = max_payload.max(serde_json::to_vec(&page)?.len());
                if let Some(next) = page.next_offset {
                    offset = next;
                } else {
                    break;
                }
            }
            assert_eq!(seen, count);
            if i > 0 {
                first.push(first_time);
                traverse.push(start.elapsed().as_nanos());
            }
        }
        results.push(json!({"commits":count,"files":1000,"merges":merges,"head":f.git(&["rev-parse","HEAD"]),"lanes":lanes,"segments":segments,"git_capture_ns":output.duration.as_nanos(),"layout_all_commits":stats(samples),"open_first_page":stats(first),"all_pages_with_ref_validation_and_json_measurement":stats(traverse),"max_backend_cache_bytes":maxcache,"max_json_page_bytes":max_payload}));
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
