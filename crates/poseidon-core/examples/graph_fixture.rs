//! Keeps an owned fixture alive while the external desktop test holds stdin open.
#[path = "../tests/support/mod.rs"]
mod support;
fn main() {
    use std::io::Write;
    let count = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(10000);
    let f = support::Fixture::scale(count, 1000, true);
    let merge = f.git(&["rev-list", "--first-parent", "--merges", "-1", "HEAD"]);
    if !merge.is_empty() {
        f.git(&["tag", "-a", "v0.1.0", "-m", "Graph fixture tag", &merge]);
    }
    f.git(&[
        "update-ref",
        "refs/remotes/origin/main",
        &f.git(&["rev-parse", "HEAD"]),
    ]);
    println!("{}", serde_json::json!({"path":f.root,"commits":count}));
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}
