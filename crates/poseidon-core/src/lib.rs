//! Git inspection, whole-file index operations and guarded commit creation.
//! No desktop runtime or network API.
pub mod application;
mod diff;
pub mod domain;
mod git;
pub mod graph;
mod history;
mod parse;
pub mod process;
pub mod trust;

pub use application::RepositoryService;
pub use git::Git;
pub use trust::TrustStore;

/// Instrumentation available only in explicitly enabled benchmark builds.
#[cfg(feature = "bench")]
pub mod benchmark {
    pub fn parse_commits(bytes: &[u8]) -> crate::domain::Result<Vec<crate::domain::Commit>> {
        crate::parse::commits(bytes)
    }
    /// Parses a unified patch; returns the number of display lines.
    pub fn parse_patch(bytes: &[u8], line_limit: usize) -> crate::domain::Result<usize> {
        crate::diff::patch(bytes, line_limit).map(|p| p.lines.len())
    }
}
