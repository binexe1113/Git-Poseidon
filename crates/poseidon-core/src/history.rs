use crate::{
    domain::*,
    git::{Git, RepositoryLocation},
    graph::Layout,
    parse,
    process::GitStream,
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};
const CACHE_PAGES: usize = 16;
const CACHE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct History {
    git: Git,
    location: RepositoryLocation,
    fingerprint: Vec<u8>,
    roots: Vec<String>,
    labels: BTreeMap<String, Vec<CommitRef>>,
    stream: Option<GitStream>,
    pending: Option<Commit>,
    layout: Layout,
    next: u32,
    high_water: u32,
    complete: bool,
    last_read: Instant,
    cache: VecDeque<(HistoryPage, usize)>,
    bytes: usize,
}
impl History {
    pub async fn new(git: Git, location: RepositoryLocation) -> Result<Self> {
        let (fingerprint, roots, labels) = git.ref_state(&location).await?;
        Ok(Self {
            git,
            location,
            fingerprint,
            roots,
            labels,
            stream: None,
            pending: None,
            layout: Layout::default(),
            next: 0,
            high_water: 0,
            complete: false,
            last_read: Instant::now(),
            cache: VecDeque::new(),
            bytes: 0,
        })
    }
    async fn validate(&self) -> Result<()> {
        if self.git.fingerprint(&self.location).await? != self.fingerprint {
            return Err(AppError::new(ErrorCode::StaleSession,"HEAD, refs ou fronteiras mudaram. Atualize o repositório para reconstruir o grafo."));
        }
        Ok(())
    }
    pub fn commit(&self, oid: &str) -> Option<Commit> {
        self.cache
            .iter()
            .flat_map(|(p, _)| &p.commits)
            .find(|c| c.oid == oid)
            .cloned()
    }
    fn restart(&mut self) -> Result<()> {
        self.stream = if self.roots.is_empty() {
            None
        } else {
            Some(self.git.stream(&self.location.root, &self.roots)?)
        };
        self.pending = None;
        self.layout = Layout::default();
        self.next = 0;
        self.complete = self.roots.is_empty();
        Ok(())
    }
    async fn next_commit(&mut self) -> Result<Option<Commit>> {
        if let Some(commit) = self.pending.take() {
            return Ok(Some(commit));
        }
        let Some(stream) = &mut self.stream else {
            return Ok(None);
        };
        let Some(bytes) = stream.next().await? else {
            return Ok(None);
        };
        parse::commits(&bytes)?
            .pop()
            .map(Some)
            .ok_or_else(AppError::protocol)
    }
    async fn read_page(&mut self) -> Result<HistoryPage> {
        let start = Instant::now();
        let offset = self.next;
        let mut commits = Vec::with_capacity(COMMIT_LIMIT);
        let mut graph = Vec::with_capacity(COMMIT_LIMIT);
        let mut layout_time = Duration::ZERO;
        let mut size = 0;
        for _ in 0..COMMIT_LIMIT {
            let Some(commit) = self.next_commit().await? else {
                self.complete = true;
                break;
            };
            let t = Instant::now();
            let mut row = self.layout.push(&commit)?;
            row.refs = self.labels.get(&commit.oid).cloned().unwrap_or_default();
            layout_time += t.elapsed();
            size += row_size(&commit, &row);
            if size > 8 * 1024 * 1024 {
                return Err(AppError::new(
                    ErrorCode::OutputLimit,
                    "Uma página do grafo excedeu 8 MiB de dados estimados.",
                ));
            }
            commits.push(commit);
            graph.push(row);
        }
        if !self.complete {
            self.pending = self.next_commit().await?;
            self.complete = self.pending.is_none();
        }
        if self.complete {
            self.stream = None;
        }
        self.next = offset
            .checked_add(commits.len() as u32)
            .ok_or_else(AppError::protocol)?;
        self.high_water = self.high_water.max(self.next);
        self.last_read = Instant::now();
        Ok(HistoryPage {
            offset,
            commits,
            graph,
            next_offset: if self.complete { None } else { Some(self.next) },
            complete: self.complete,
            cache_bytes: 0,
            read_elapsed_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
            layout_elapsed_us: layout_time.as_micros().min(u32::MAX as u128) as u32,
        })
    }
    pub async fn page(&mut self, offset: u32) -> Result<HistoryPage> {
        if !offset.is_multiple_of(COMMIT_LIMIT as u32) || offset > self.high_water {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "Cursor de histórico inválido.",
            ));
        }
        self.validate().await?;
        if let Some(i) = self.cache.iter().position(|(p, _)| p.offset == offset) {
            let (mut page, size) = self.cache.remove(i).ok_or_else(AppError::protocol)?;
            page.cache_bytes = self.bytes as u32;
            self.cache.push_back((page.clone(), size));
            return Ok(page);
        }
        let operation = async {
            if self.stream.is_none()
                || self.next != offset
                || self.last_read.elapsed() > Duration::from_secs(55)
            {
                self.restart()?;
            }
            loop {
                let page = self.read_page().await?;
                let found = page.offset == offset;
                let size: usize = page
                    .commits
                    .iter()
                    .zip(&page.graph)
                    .map(|(c, g)| row_size(c, g))
                    .sum();
                if let Some(i) = self.cache.iter().position(|(p, _)| p.offset == page.offset) {
                    if let Some((_, old_size)) = self.cache.remove(i) {
                        self.bytes -= old_size;
                    }
                }
                self.bytes += size;
                self.cache.push_back((page.clone(), size));
                while self.cache.len() > CACHE_PAGES || self.bytes > CACHE_BYTES {
                    if let Some((_, size)) = self.cache.pop_front() {
                        self.bytes -= size;
                    }
                }
                if found {
                    return Ok(page);
                }
                if self.complete {
                    return Err(AppError::new(
                        ErrorCode::InvalidInput,
                        "Fim do histórico alcançado.",
                    ));
                }
            }
        };
        let mut page = tokio::time::timeout(Duration::from_secs(15), operation)
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::Timeout,
                    "A página demorou mais que 15 segundos. Atualize o histórico.",
                )
            })??;
        self.validate().await?;
        page.cache_bytes = self.bytes as u32;
        Ok(page)
    }
}
// Conservative accounting includes owned strings and allocation headers, not only commit count.
fn row_size(c: &Commit, g: &GraphRow) -> usize {
    std::mem::size_of::<Commit>()
        + std::mem::size_of::<GraphRow>()
        + [
            &c.oid,
            &c.tree_oid,
            &c.author_name,
            &c.author_email,
            &c.authored_at,
            &c.committer_name,
            &c.committer_email,
            &c.committed_at,
            &c.summary,
        ]
        .iter()
        .map(|s| s.capacity())
        .sum::<usize>()
        + c.parents.capacity() * std::mem::size_of::<String>()
        + c.parents.iter().map(String::capacity).sum::<usize>()
        + g.oid.capacity()
        + g.segments.capacity() * std::mem::size_of::<GraphSegment>()
        + g.segments
            .iter()
            .map(|s| s.child.capacity() + s.parent.capacity())
            .sum::<usize>()
        + g.refs.capacity() * std::mem::size_of::<CommitRef>()
        + g.refs
            .iter()
            .map(|r| r.name.capacity() + r.kind.capacity())
            .sum::<usize>()
}
