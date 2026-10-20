#[path = "support/mod.rs"]
mod support;
use poseidon_core::{domain::*, graph::Layout, RepositoryService};
use std::collections::{BTreeMap, BTreeSet};
use support::{Fixture, Scenario};

async fn assert_graph(f: &Fixture) {
    let oracle: BTreeMap<String, Vec<String>> = f
        .git(&["rev-list", "--all", "--parents"])
        .lines()
        .map(|line| {
            let mut ids = line.split_whitespace();
            (
                ids.next().unwrap().to_owned(),
                ids.map(str::to_owned).collect(),
            )
        })
        .collect();
    let service = RepositoryService::default();
    let snap = service.open(&f.root).await.unwrap();
    let mut cursor = 0;
    let mut all = Vec::new();
    let mut commits = Vec::new();
    loop {
        let p = service
            .history_page(snap.session_id, snap.generation, cursor)
            .await
            .unwrap()
            .data;
        assert!(p.commits.len() <= 200);
        assert!(p.cache_bytes <= 64 * 1024 * 1024);
        for (c, row) in p.commits.iter().zip(&p.graph) {
            assert_eq!(c.oid, row.oid);
            assert_eq!(oracle[&c.oid], c.parents);
            for s in &row.segments {
                assert!(oracle[&s.child].contains(&s.parent), "invented edge");
            }
        }
        commits.extend(p.commits);
        all.extend(p.graph);
        match p.next_offset {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(all.len(), oracle.len());
    assert_eq!(
        all.iter().map(|r| &r.oid).collect::<BTreeSet<_>>().len(),
        all.len()
    );
    let positions: BTreeMap<_, _> = all.iter().map(|r| (r.oid.clone(), r.index)).collect();
    let mut expected = Layout::default();
    for (c, row) in commits.iter().zip(&all) {
        let mut direct = expected.push(c).unwrap();
        direct.refs = row.refs.clone();
        assert_eq!(&direct, row, "page boundary changed layout");
        for parent in &c.parents {
            assert!(positions[parent] > row.index);
        }
        for parent in &c.parents {
            let mut previous = None;
            for r in &all[row.index as usize..=positions[parent] as usize] {
                let edge = r
                    .segments
                    .iter()
                    .find(|s| s.child == c.oid && s.parent == *parent)
                    .expect("edge gap");
                if r.index == row.index {
                    assert_eq!(edge.from_lane, row.lane);
                    assert_eq!(edge.from_y, 1);
                }
                if r.index == positions[parent] {
                    assert_eq!(edge.to_lane, r.lane);
                    assert_eq!(edge.to_y, 1);
                }
                if let Some(lane) = previous {
                    assert_eq!(edge.from_lane, lane);
                }
                previous = Some(edge.to_lane);
            }
        }
    }
    if all.len() > 3200 {
        // Frontend may retain a focused page after the backend LRU evicts it.
        let detail = service
            .commit_at(snap.session_id, snap.generation, &commits[0].oid, Some(0))
            .await
            .unwrap();
        assert_eq!(detail.data.commit.oid, commits[0].oid);
        let middle = ((all.len() / 2) / 200 * 200) as u32;
        let page = service
            .history_page(snap.session_id, snap.generation, middle)
            .await
            .unwrap()
            .data;
        assert_eq!(
            page.graph,
            all[middle as usize..middle as usize + page.graph.len()]
        );
    }
    // Cached/replayed pages are byte-for-byte identical in geometry, including after eviction.
    let first = service
        .history_page(snap.session_id, snap.generation, 0)
        .await
        .unwrap();
    assert_eq!(first.data.graph, all[..first.data.graph.len()]);
    if let Some(c) = first.data.commits.first() {
        service
            .commit(snap.session_id, snap.generation, &c.oid)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn initial_and_linear() {
    assert_graph(&Fixture::scenario(Scenario::Initial)).await;
    assert_graph(&Fixture::scenario(Scenario::Linear)).await;
}
#[tokio::test]
async fn divergent_branches() {
    assert_graph(&Fixture::scenario(Scenario::Divergent)).await;
}
#[tokio::test]
async fn two_parent_merge_and_tags() {
    let f = Fixture::scenario(Scenario::Divergent);
    f.git(&["merge", "--no-ff", "feature", "-m", "merge feature"]);
    f.git(&["tag", "v1"]);
    f.git(&["tag", "-a", "v2", "-m", "annotated"]);
    f.git(&["tag", "-a", "v3", "v2", "-m", "nested"]);
    f.git(&[
        "tag",
        "blob-tag",
        &f.git(&["rev-parse", "HEAD:feature.txt"]),
    ]);
    assert_graph(&f).await;
    let snap = RepositoryService::default().open(&f.root).await.unwrap();
    let refs = &snap.data.graph[0].refs;
    for name in ["HEAD", "main", "v1", "v2", "v3"] {
        assert!(refs.iter().any(|r| r.name == name));
    }
    assert!(!refs.iter().any(|r| r.name == "blob-tag"));
}
#[tokio::test]
async fn octopus_and_simultaneous_branches() {
    assert_graph(&Fixture::scale(125, 1, true)).await;
}
#[tokio::test]
async fn detached_head_includes_unreferenced_head() {
    let f = Fixture::scenario(Scenario::Detached);
    f.write("detached.txt", "only HEAD");
    let oid = f.commit("detached unique");
    let s = RepositoryService::default().open(&f.root).await.unwrap();
    assert!(s.data.commits.iter().any(|c| c.oid == oid));
    assert!(s
        .data
        .graph
        .iter()
        .find(|r| r.oid == oid)
        .unwrap()
        .refs
        .iter()
        .any(|r| r.name == "HEAD"));
}
#[tokio::test]
async fn pagination_and_cache_eviction_10000() {
    assert_graph(&Fixture::scale(10000, 1, false)).await;
}
#[tokio::test]
async fn merge_crosses_page_boundary() {
    assert_graph(&Fixture::scale(3650, 1, true)).await;
}
#[tokio::test]
async fn changed_refs_invalidate_even_cached_page() {
    for tags_only in [false, true] {
        let f = Fixture::scale(250, 1, false);
        let s = RepositoryService::default();
        let opened = s.open(&f.root).await.unwrap();
        if tags_only {
            f.git(&["tag", "new-tag"]);
        } else {
            f.write("new.txt", "new");
            f.commit("new external commit");
        }
        let error = s
            .history_page(opened.session_id, opened.generation, 0)
            .await
            .err()
            .unwrap();
        assert!(matches!(error.code, ErrorCode::StaleSession));
        assert!(s
            .history_page(opened.session_id, opened.generation, 200)
            .await
            .is_err());
        let fresh = s.refresh(opened.session_id).await.unwrap();
        assert!(fresh.generation > opened.generation);
        assert!(s
            .history_page(fresh.session_id, fresh.generation, 200)
            .await
            .is_ok());
    }
}
#[tokio::test]
async fn empty_and_invalid_cursor() {
    let f = Fixture::new();
    let s = RepositoryService::default();
    let o = s.open(&f.root).await.unwrap();
    assert!(o.data.graph.is_empty());
    assert!(
        s.history_page(o.session_id, o.generation, 0)
            .await
            .unwrap()
            .data
            .complete
    );
    assert!(matches!(
        s.history_page(o.session_id, o.generation, 1)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidInput
    ));
}

#[tokio::test]
async fn disconnected_roots_are_not_connected() {
    let f = Fixture::scenario(Scenario::Linear);
    let tree = f.git(&["rev-parse", "HEAD^{tree}"]);
    let root = f.git(&["commit-tree", &tree, "-m", "independent root"]);
    f.git(&["update-ref", "refs/heads/orphan", &root]);
    assert_graph(&f).await;
}

#[tokio::test]
async fn layout_allocation_follows_live_edges_after_merge() {
    let f = Fixture::scale(125, 1, true);
    let snapshot = RepositoryService::default().open(&f.root).await.unwrap();
    let mut layout = Layout::default();
    let mut checked = 0;
    for commit in &snapshot.data.commits {
        let row = layout.push(commit).unwrap();
        if row.width >= 5 && row.segments.len() == 2 {
            assert!(
                row.segments.capacity() <= 4,
                "reserved storage for already-ended lanes"
            );
            checked += 1;
        }
    }
    assert!(checked > 10);
}
