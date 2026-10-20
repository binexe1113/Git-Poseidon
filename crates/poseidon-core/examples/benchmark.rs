//! Offline, owned fixture. Outputs aggregate timings, never raw Git output.
#[path = "../tests/support/mod.rs"]
mod support;
use poseidon_core::{benchmark::parse_commits, process::GitProcess};
use serde_json::json;
use std::{hint::black_box, io::Write, process::Stdio, time::Instant};
use support::Fixture;

fn stats(mut ns: Vec<u128>) -> serde_json::Value {
    ns.sort_unstable();
    json!({"samples":ns.len(),"min_ns":ns[0],"p50_ns":ns[(ns.len()-1)/2],"p95_ns":ns[(ns.len()*95).div_ceil(100)-1],"max_ns":ns[ns.len()-1]})
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new();
    let commits = 1000;
    let files = 1000;
    let mut stream = Vec::new();
    for i in 0..commits {
        let content = format!("revision {i}\n");
        write!(stream, "commit refs/heads/main\ncommitter Poseidon Test <test@example.invalid> 1735732800 +0000\ndata {}\ncommit {i}\n", format!("commit {i}").len())?;
        if i == 0 {
            for j in 0..files {
                write!(
                    stream,
                    "M 100644 inline file-{j:04}.txt\ndata {}\n{content}",
                    content.len()
                )?;
            }
        } else {
            write!(
                stream,
                "M 100644 inline file-0000.txt\ndata {}\n{content}",
                content.len()
            )?;
        }
        stream.push(b'\n');
    }
    let mut import = f
        .command()
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    import.stdin.take().unwrap().write_all(&stream)?;
    assert!(import.wait_with_output()?.status.success());
    f.git(&["reset", "--hard", "HEAD"]); // Only our owned temporary fixture.
    assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), "1000");
    let runner = GitProcess::discover()?;
    let args = [
        "log",
        "-z",
        "--max-count=200",
        "--format=%H%x00%T%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s",
        "HEAD",
    ];
    let mut spawn = Vec::new();
    let mut version = Vec::new();
    let mut log = Vec::new();
    let mut parse = Vec::new();
    let mut bytes = 0;
    for i in 0..35 {
        let v = runner.run(&f.root, &["--version"]).await?;
        let output = runner.run(&f.root, &args).await?;
        assert!(v.success && output.success);
        bytes = output.stdout.len();
        let start = Instant::now();
        let parsed = parse_commits(black_box(&output.stdout))?;
        let parsing = start.elapsed();
        assert_eq!(black_box(parsed.len()), 200);
        black_box(parsed);
        if i >= 5 {
            spawn.push(v.spawn_duration.as_nanos());
            version.push(v.duration.as_nanos());
            log.push(output.duration.as_nanos());
            parse.push(parsing.as_nanos());
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"fixture":{"commits":commits,"files":files,"returned_commits":200,"stdout_bytes":bytes,"head":f.git(&["rev-parse","HEAD"])},"warmup":5,"git_version_spawn_call":stats(spawn),"git_version_total_startup_proxy":stats(version),"git_log_execution_and_pipes":stats(log),"rust_parse_log":stats(parse)})
        )?
    );
    Ok(())
}
