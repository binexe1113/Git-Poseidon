//! Backend-only diff costs: Git subprocesses + Rust parsing + JSON size (docs/16).
#[path = "../tests/support/mod.rs"]
mod support;
use poseidon_core::{
    benchmark::parse_patch, domain::WorkingArea, process::GitProcess, RepositoryService,
};
use serde_json::json;
use std::time::Instant;

fn stats(mut times: Vec<u128>) -> serde_json::Value {
    times.sort_unstable();
    let ms = |ns: u128| ns as f64 / 1e6;
    json!({"samples": times.len(), "p50_ms": ms(times[(times.len() - 1) / 2]),
        "p95_ms": ms(times[(times.len() * 95).div_ceil(100) - 1]), "max_ms": ms(times[times.len() - 1])})
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let f = support::Fixture::new();
    f.write(
        "small.rs",
        &(0..200)
            .map(|i| format!("fn f{i}() {{}}\n"))
            .collect::<String>(),
    );
    std::fs::create_dir_all(f.root.join("many"))?;
    for i in 0..1000 {
        f.write(&format!("many/file-{i:04}.txt"), "a\n");
    }
    f.commit("base");
    f.write(
        "small.rs",
        &(0..200)
            .map(|i| {
                if i % 50 == 7 {
                    format!("fn f{i}() {{ todo!() }}\n")
                } else {
                    format!("fn f{i}() {{}}\n")
                }
            })
            .collect::<String>(),
    );
    for i in 0..1000 {
        f.write(&format!("many/file-{i:04}.txt"), "b\n");
    }
    for (name, lines, width) in [
        ("lines-12k.csv", 12_000, 20),
        ("lines-30k.log", 30_000, 99),
        ("lines-150k.txt", 150_000, 20),
    ] {
        f.write(
            name,
            &(0..lines)
                .map(|i| format!("{i:0>w$}\n", w = width))
                .collect::<String>(),
        );
    }
    let head = f.commit("changes");
    let service = RepositoryService::default();
    let opened = service.open(&f.root).await?;
    let id = opened.session_id;
    let mut generation = opened.generation;
    let listed = service.commit_files(id, generation, &head).await?.data;
    let index = |name: &str| listed.files.iter().find(|f| f.path == name).unwrap().index;

    let mut results = serde_json::Map::new();
    let mut list = Vec::new();
    for i in 0..25 {
        generation = service.refresh(id).await?.generation; // new cache keys: cold path
        let start = Instant::now();
        let files = service.commit_files(id, generation, &head).await?;
        if i >= 5 {
            list.push(start.elapsed().as_nanos());
        }
        assert_eq!(files.data.total_files, 1004);
    }
    results.insert("file_list_1004_files".into(), stats(list));
    for (name, allow_large) in [
        ("small.rs", false),
        ("lines-12k.csv", false),
        ("lines-30k.log", true),
        ("lines-150k.txt", true),
    ] {
        let (mut cold, mut warm, mut bytes, mut total) = (Vec::new(), Vec::new(), 0, 0);
        for i in 0..15 {
            generation = service.refresh(id).await?.generation;
            service.commit_files(id, generation, &head).await?;
            let start = Instant::now();
            let page = service
                .file_diff(id, generation, &head, index(name), 0, allow_large)
                .await?;
            let elapsed = start.elapsed().as_nanos();
            let start = Instant::now();
            service
                .file_diff(id, generation, &head, index(name), 1000, allow_large)
                .await?;
            let cached = start.elapsed().as_nanos();
            if i >= 3 {
                cold.push(elapsed);
                warm.push(cached);
            }
            bytes = serde_json::to_vec(&page)?.len();
            total = page.data.total_lines;
        }
        results.insert(
            name.into(),
            json!({"total_lines": total, "first_page_json_bytes": bytes,
            "cold_first_page": stats(cold), "cached_next_page": stats(warm)}),
        );
    }
    // Local changes: 100 staged, 1002 unstaged and 1002 untracked files.
    for i in 0..1000 {
        f.write(&format!("many/file-{i:04}.txt"), "c\n");
    }
    f.git(&["add", "--", "many/file-00*"]);
    for i in 0..100 {
        f.write(&format!("many/file-{i:04}.txt"), "d\n");
    }
    f.write("small.rs", "fn changed() {}\n");
    f.write(
        "lines-12k.csv",
        &(0..12_000)
            .map(|i| format!("{i:0>20}x\n"))
            .collect::<String>(),
    );
    std::fs::create_dir_all(f.root.join("new"))?;
    for i in 0..1000 {
        f.write(&format!("new/file-{i:04}.txt"), "n\n");
    }
    for (name, lines, width) in [
        ("untracked-12k.txt", 12_000, 20),
        ("untracked-30k.log", 30_000, 99),
    ] {
        f.write(
            name,
            &(0..lines)
                .map(|i| format!("{i:0>w$}\n", w = width))
                .collect::<String>(),
        );
    }
    let mut lists = Vec::new();
    let mut counts = (0, 0, 0);
    for i in 0..25 {
        generation = service.refresh(id).await?.generation;
        let start = Instant::now();
        let changes = service.working_changes(id, generation).await?.data;
        if i >= 5 {
            lists.push(start.elapsed().as_nanos());
        }
        counts = (
            changes.total_staged,
            changes.total_unstaged,
            changes.total_untracked,
        );
    }
    results.insert(
        "working_list".into(),
        json!({"staged": counts.0, "unstaged": counts.1, "untracked": counts.2, "timing": stats(lists)}),
    );
    for (area, name, allow_large) in [
        (WorkingArea::Staged, "many/file-0000.txt", false),
        (WorkingArea::Unstaged, "small.rs", false),
        (WorkingArea::Unstaged, "lines-12k.csv", false),
        (WorkingArea::Untracked, "untracked-12k.txt", false),
        (WorkingArea::Untracked, "untracked-30k.log", true),
    ] {
        let (mut cold, mut warm, mut bytes, mut total) = (Vec::new(), Vec::new(), 0, 0);
        for i in 0..15 {
            generation = service.refresh(id).await?.generation;
            let changes = service.working_changes(id, generation).await?.data;
            let group = match area {
                WorkingArea::Staged => &changes.staged,
                WorkingArea::Unstaged => &changes.unstaged,
                WorkingArea::Untracked => &changes.untracked,
            };
            let index = group.iter().find(|f| f.path == name).unwrap().index;
            let rev = changes.revision;
            let start = Instant::now();
            let page = service
                .working_diff(id, generation, rev, area, index, 0, allow_large)
                .await?;
            let elapsed = start.elapsed().as_nanos();
            let start = Instant::now();
            service
                .working_diff(id, generation, rev, area, index, 1000, allow_large)
                .await?;
            let cached = start.elapsed().as_nanos();
            if i >= 3 {
                cold.push(elapsed);
                warm.push(cached);
            }
            bytes = serde_json::to_vec(&page)?.len();
            total = page.data.total_lines;
        }
        results.insert(
            format!("{area:?}:{name}"),
            json!({"total_lines": total, "first_page_json_bytes": bytes,
            "cold_first_page": stats(cold), "cached_next_page": stats(warm)}),
        );
    }
    // Parsing alone, on Git's own patch output for the 150k-line file.
    let patch = GitProcess::discover()?
        .run(
            &f.root,
            &[
                "diff-tree",
                "-p",
                "--root",
                "--no-color",
                "HEAD^",
                "HEAD",
                "--",
                "lines-150k.txt",
            ],
        )
        .await?;
    let mut parse = Vec::new();
    for i in 0..15 {
        let start = Instant::now();
        let lines = parse_patch(&patch.stdout, 200_000)?;
        if i >= 3 {
            parse.push(start.elapsed().as_nanos());
        }
        assert_eq!(lines, 150_001);
    }
    results.insert(
        "parse_only_150k_lines".into(),
        json!({"patch_bytes": patch.stdout.len(), "timing": stats(parse)}),
    );
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
