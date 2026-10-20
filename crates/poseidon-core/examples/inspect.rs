//! Small, repeatable read-only measurement of the Rust service (not a UI benchmark).
use poseidon_core::RepositoryService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: inspect /path/to/repository [samples]")?;
    let samples: usize = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "1".into())
        .parse()?;
    if !(1..=100).contains(&samples) {
        return Err("samples must be between 1 and 100".into());
    }
    let service = RepositoryService::default();
    let mut readings = Vec::new();
    for _ in 0..samples {
        let response = service.open(std::path::Path::new(&path)).await?;
        readings.push(response.data.read_elapsed_ms);
        if samples == 1 {
            println!(
                "commits={} branches={} changed_files={} truncated={} snapshot_ms={}",
                response.data.commits.len(),
                response.data.branches.len(),
                response.data.status.changes.len(),
                response.data.has_more_commits,
                response.data.read_elapsed_ms
            );
        }
    }
    if samples > 1 {
        readings.sort_unstable();
        println!(
            "samples={} p50_ms={} p95_ms={} min_ms={} max_ms={}",
            samples,
            readings[(samples - 1) / 2],
            readings[(samples * 95).div_ceil(100) - 1],
            readings[0],
            readings[samples - 1]
        );
    }
    Ok(())
}
