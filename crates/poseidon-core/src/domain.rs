use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const SCHEMA_VERSION: u32 = 2;
pub const COMMIT_LIMIT: usize = 200;
pub const BRANCH_LIMIT: usize = 5_000;
pub const CHANGE_LIMIT: usize = 500;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitInstallation {
    pub version: String,
    pub executable: String,
}

#[derive(Debug, Clone, Serialize, TS)]
pub enum ErrorCode {
    GitUnavailable,
    UnsupportedGit,
    InvalidRepository,
    InvalidInput,
    CommandFailed,
    Timeout,
    Cancelled,
    OutputLimit,
    InvalidOutput,
    StaleSession,
    Busy,
    /// Index or working tree changed after the listing: reload the working changes.
    WorkingTreeChanged,
    /// Writes need the user's explicit trust in this repository (docs/17).
    TrustRequired,
    /// Another Git process holds `index.lock`; it is never removed by the app.
    IndexLocked,
    /// Merge/rebase in progress, conflict, submodule, sparse checkout or bare repository.
    UnsupportedState,
    /// A write was interrupted (timeout or signal): the result is not known until the
    /// repository is read again.
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, TS)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn protocol() -> Self {
        Self::new(
            ErrorCode::InvalidOutput,
            "O Git retornou dados em um formato inesperado.",
        )
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Head {
    Attached { name: String, oid: String },
    Unborn { name: String },
    Detached { oid: String },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub oid: String,
    pub tree_oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committed_at: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitDetail {
    pub commit: Commit,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Branch {
    pub full_ref: String,
    pub name: String,
    pub oid: String,
    pub remote: bool,
    pub current: bool,
    pub upstream: Option<String>,
    pub symbolic_target: Option<String>,
}

/// Display-only escaped paths. No API accepts these strings for a file operation.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub original_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
    pub untracked: bool,
    pub conflicted: bool,
    pub submodule: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkingStatus {
    pub available: bool,
    pub total_files: u32,
    pub has_more_changes: bool,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicts: u32,
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySnapshot {
    pub name: String,
    pub path: String,
    pub git_dir: String,
    pub common_dir: String,
    pub bare: bool,
    pub shallow: bool,
    pub object_format: String,
    pub head: Head,
    pub operation: String,
    pub status: WorkingStatus,
    pub commits: Vec<Commit>,
    pub branches: Vec<Branch>,
    pub graph: Vec<GraphRow>,
    pub has_more_commits: bool,
    pub has_more_branches: bool,
    pub read_elapsed_ms: u32,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: RepositorySnapshot,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DetailResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: CommitDetail,
}

#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommitRef {
    pub name: String,
    pub kind: String,
    pub current: bool,
}

/// A segment is always part of the real child -> parent edge, even through other rows.
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GraphSegment {
    pub child: String,
    pub parent: String,
    pub from_lane: u16,
    pub to_lane: u16,
    /// Half-row coordinates: top=0, node=1, bottom=2.
    pub from_y: u8,
    pub to_y: u8,
    pub color: u16,
}

#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GraphRow {
    pub oid: String,
    pub index: u32,
    pub lane: u16,
    pub width: u16,
    pub segments: Vec<GraphSegment>,
    pub refs: Vec<CommitRef>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub offset: u32,
    pub commits: Vec<Commit>,
    pub graph: Vec<GraphRow>,
    pub next_offset: Option<u32>,
    pub complete: bool,
    pub cache_bytes: u32,
    pub read_elapsed_ms: u32,
    pub layout_elapsed_us: u32,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: HistoryPage,
}

/// Files listed per commit; the rest is counted but not sent.
pub const DIFF_FILE_LIMIT: usize = 3_000;
/// Lines per IPC page of a file diff.
pub const DIFF_PAGE_LINES: usize = 1_000;
/// Default line budget per file (docs/04); `allow_large` raises it to the hard cap.
pub const DIFF_LINE_LIMIT: usize = 20_000;
pub const DIFF_LINE_HARD_LIMIT: usize = 200_000;
/// Blobs above this need an explicit "open anyway" (docs/04: 2 MiB).
pub const DIFF_SOFT_BLOB_BYTES: u64 = 2 * 1024 * 1024;
/// Blobs above this are never diffed in this phase.
pub const DIFF_HARD_BLOB_BYTES: u64 = 32 * 1024 * 1024;
/// Patch text read from Git for one file.
pub const DIFF_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Characters per displayed line; longer lines are clipped and flagged.
pub const DIFF_LINE_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    /// Untracked file, compared with nothing (content preview).
    Untracked,
    Unknown,
}

/// What the commit is compared with. Merges use the first parent (explicit in the UI).
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffBase {
    /// Root commit, or the boundary of a shallow clone: compared with the empty tree.
    EmptyTree,
    FirstParent {
        oid: String,
        #[serde(rename = "parentCount")]
        parent_count: u32,
    },
}

/// Display-only paths; diffs are requested by `index`, never by path string.
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub index: u32,
    pub path: String,
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub similarity: Option<u8>,
    /// `None` when Git reports the file as binary.
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub binary: bool,
    pub old_mode: String,
    pub new_mode: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitFiles {
    pub oid: String,
    pub base: DiffBase,
    pub files: Vec<ChangedFile>,
    pub total_files: u32,
    pub has_more_files: bool,
    /// Sums over every changed file, including those beyond the list limit.
    pub additions: u32,
    pub deletions: u32,
    pub elapsed_ms: u32,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitFilesResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: CommitFiles,
}

/// Local change groups: `HEAD → index`, `index → working tree`, untracked against nothing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum WorkingArea {
    Staged,
    Unstaged,
    Untracked,
}

/// One listing of local changes. `revision` addresses it in `get_working_diff`; a
/// listing identical to the previous one keeps its revision.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkingChanges {
    pub revision: u32,
    /// `None` on an unborn branch: staged files are compared with the empty tree.
    pub head_oid: Option<String>,
    pub staged: Vec<ChangedFile>,
    pub unstaged: Vec<ChangedFile>,
    pub untracked: Vec<ChangedFile>,
    /// Counts before the per-group list limit.
    pub total_staged: u32,
    pub total_unstaged: u32,
    pub total_untracked: u32,
    pub has_more_files: bool,
    /// Trust granted for this repository's identity: writes allowed, filters run.
    pub trusted: bool,
    /// Why stage/unstage is unavailable for the whole repository, if it is.
    pub write_block: Option<String>,
    /// Configured filter drivers (names only; nothing was run to find them).
    pub filter_drivers: Vec<String>,
    pub elapsed_ms: u32,
}

/// Result of a whole-file stage or unstage: the index as Git now reports it.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IndexChange {
    /// Display path of the file that was changed.
    pub path: String,
    /// The listing after the write (same contract as `get_working_changes`).
    pub working: WorkingChanges,
    /// Time spent in the Git write process alone.
    pub git_ms: u32,
    /// Re-listing after the write; `incremental` when only the touched paths were read.
    pub relist_ms: u32,
    pub incremental: bool,
    pub elapsed_ms: u32,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IndexChangeResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: IndexChange,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkingChangesResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: WorkingChanges,
}

/// What a file diff belongs to.
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffTarget {
    Commit { oid: String },
    Working { revision: u32, area: WorkingArea },
}

#[derive(Debug, Clone, Copy, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    Hunk,
    Context,
    Added,
    Deleted,
    /// `\ No newline at end of file`.
    Note,
}

#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// Without the +/-/space marker and without a trailing CR (see `cr`).
    pub text: String,
    pub cr: bool,
    pub clipped: bool,
}

#[derive(Debug, Clone, Copy, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DiffContent {
    Text,
    Binary,
    /// Mode-only change, pure rename or empty file: no hunks.
    NoContent,
    /// Above the soft limit; can be opened with `allow_large`.
    Large,
    /// Above the hard limit; not diffed in this phase.
    TooLarge,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub target: DiffTarget,
    pub file: ChangedFile,
    pub content: DiffContent,
    pub offset: u32,
    /// Page size used by the backend; request offsets in multiples of it.
    pub page_lines: u32,
    pub lines: Vec<DiffLine>,
    pub total_lines: u32,
    /// Line indices of every hunk header, for hunk navigation.
    pub hunk_starts: Vec<u32>,
    /// Widest line (characters, tabs as 4) for horizontal sizing.
    pub max_columns: u32,
    pub truncated: bool,
    pub lossy: bool,
    /// Blob sizes in bytes; JSON numbers (exact up to 2^53).
    #[ts(type = "number | null")]
    pub old_size: Option<u64>,
    #[ts(type = "number | null")]
    pub new_size: Option<u64>,
    pub elapsed_ms: u32,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileDiffResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: FileDiff,
}

/// A one-use, expiring confirmation of the staged index and effective Git policy.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitPlan {
    pub token: u32,
    pub head_oid: Option<String>,
    pub author: Option<String>,
    pub committer: Option<String>,
    pub staged_count: u32,
    pub additions: u32,
    pub deletions: u32,
    pub hooks: Vec<String>,
    pub hooks_path: String,
    pub hooks_trusted: bool,
    pub signing: Option<String>,
    pub block: Option<String>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitPlanResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: CommitPlan,
}
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CommitOutcome {
    Created,
    Rejected,
    Unknown,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitCreation {
    pub outcome: CommitOutcome,
    pub oid: Option<String>,
    pub message: String,
    /// Git operation including guard setup/trace parsing, excluding hook wrapper time.
    pub git_ms: u32,
    /// Hook wrappers plus delegated hooks; Trace2, not a wall-clock subtraction estimate.
    pub hooks_ms: u32,
    pub refresh_ms: u32,
    pub invalidation_us: u32,
    pub elapsed_ms: u32,
    pub snapshot: Option<SnapshotResponse>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CreateCommitResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: CommitCreation,
}

/// Local-only branch intent. Names are validated by Git; OIDs are full immutable IDs.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BranchAction {
    Create {
        name: String,
        start_oid: String,
    },
    Switch {
        name: String,
        expected_oid: String,
    },
    Rename {
        name: String,
        expected_oid: String,
        new_name: String,
    },
    Delete {
        name: String,
        expected_oid: String,
    },
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchPlan {
    pub token: u32,
    pub action: BranchAction,
    pub hooks: Vec<String>,
    pub filters: Vec<String>,
    pub trusted: bool,
    pub hooks_trusted: bool,
    pub dirty_paths: Vec<String>,
    pub block: Option<String>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchPlanResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: BranchPlan,
}
#[derive(Debug, Clone, Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BranchOutcome {
    Applied,
    Rejected,
    Unknown,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchChange {
    pub outcome: BranchOutcome,
    pub message: String,
    pub git_ms: u32,
    pub refresh_ms: u32,
    pub elapsed_ms: u32,
    pub snapshot: Option<SnapshotResponse>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BranchChangeResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: BranchChange,
}

/// Position is only a hint; revision and OID are resolved and checked in Rust.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StashSelection {
    pub revision: u32,
    pub index: u32,
    pub oid: String,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StashEntry {
    pub selection: StashSelection,
    pub reference: String,
    pub message: String,
    pub timestamp: String,
    pub branch: Option<String>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StashList {
    pub entries: Vec<StashEntry>,
    pub has_more: bool,
    pub elapsed_ms: u32,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StashListResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: StashList,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum StashAction {
    Create {
        message: String,
        include_untracked: bool,
    },
    Apply {
        selection: StashSelection,
        restore_index: bool,
    },
    Drop {
        selection: StashSelection,
    },
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StashPlan {
    pub token: u32,
    pub action: StashAction,
    pub paths: Vec<String>,
    pub trusted: bool,
    pub block: Option<String>,
}
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StashPlanResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: StashPlan,
}
/// Each layer reuses the bounded, immutable commit diff contract.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StashDetailsResponse {
    pub schema_version: u32,
    pub session_id: u32,
    pub generation: u32,
    pub data: Vec<CommitFiles>,
}
