//! Bounded parsers for `git diff-tree -z --raw --numstat` and single-file patches.
use crate::{
    domain::{
        AppError, ChangeKind, ChangedFile, DiffContent, DiffLine, DiffLineKind, ErrorCode, Result,
        DIFF_LINE_CHARS,
    },
    parse,
};
use std::collections::{HashMap, HashSet, VecDeque};

/// One changed path with the raw bytes needed to request its patch later.
#[derive(Clone)]
pub(crate) struct RawChange {
    pub old_mode: String,
    pub new_mode: String,
    pub old_oid: String,
    pub new_oid: String,
    pub status: u8,
    pub similarity: Option<u8>,
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
}

impl RawChange {
    pub fn kind(&self) -> ChangeKind {
        match self.status {
            b'A' => ChangeKind::Added,
            b'M' => ChangeKind::Modified,
            b'D' => ChangeKind::Deleted,
            b'R' => ChangeKind::Renamed,
            b'C' => ChangeKind::Copied,
            b'T' => ChangeKind::TypeChanged,
            b'U' => ChangeKind::Unmerged,
            b'?' => ChangeKind::Untracked,
            _ => ChangeKind::Unknown,
        }
    }

    /// Paths for the pathspec: both sides of a rename/copy, otherwise one.
    pub fn pathspec(&self) -> Vec<&[u8]> {
        if self.old_path == self.new_path {
            vec![&self.new_path]
        } else {
            vec![&self.old_path, &self.new_path]
        }
    }

    pub fn is_gitlink(&self) -> bool {
        self.old_mode == "160000" || self.new_mode == "160000"
    }
}

pub(crate) struct ParsedChanges {
    pub entries: Vec<RawChange>,
    pub files: Vec<ChangedFile>,
    pub additions: u32,
    pub deletions: u32,
}

fn number(field: &[u8]) -> Result<Option<u32>> {
    if field == b"-" {
        return Ok(None);
    }
    std::str::from_utf8(field)
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Some)
        .ok_or_else(AppError::protocol)
}

type Stat = (Option<u32>, Option<u32>);

/// Raw records come first, then numstat records; both describe the same paths. The stat
/// is `None` when Git printed no numstat record for the entry.
fn records(bytes: &[u8]) -> Result<Vec<(RawChange, Option<Stat>)>> {
    let mut tokens = bytes.split(|b| *b == 0).peekable();
    let mut entries = Vec::new();
    while tokens.peek().is_some_and(|t| t.first() == Some(&b':')) {
        let header = tokens.next().ok_or_else(AppError::protocol)?;
        let fields: Vec<&[u8]> = header[1..].split(|b| *b == b' ').collect();
        if fields.len() != 5 || fields[4].is_empty() {
            return Err(AppError::protocol());
        }
        let status = fields[4][0];
        let similarity = std::str::from_utf8(&fields[4][1..])
            .ok()
            .and_then(|s| s.parse::<u8>().ok());
        let first = tokens.next().ok_or_else(AppError::protocol)?.to_vec();
        let second = if matches!(status, b'R' | b'C') {
            tokens.next().ok_or_else(AppError::protocol)?.to_vec()
        } else {
            first.clone()
        };
        let (old_oid, new_oid) = (parse::text(fields[2]), parse::text(fields[3]));
        if !parse::valid_oid(&old_oid) || !parse::valid_oid(&new_oid) {
            return Err(AppError::protocol());
        }
        entries.push(RawChange {
            old_mode: parse::text(fields[0]),
            new_mode: parse::text(fields[1]),
            old_oid,
            new_oid,
            status,
            similarity,
            old_path: first,
            new_path: second,
        });
    }
    // numstat: "added\tdeleted\tpath" or, for renames, "added\tdeleted\t" old new.
    // A path can repeat (an unmerged entry and its worktree side); records keep the raw order.
    let mut stats: HashMap<Vec<u8>, VecDeque<Stat>> = HashMap::new();
    while let Some(token) = tokens.next() {
        if token.is_empty() {
            continue;
        }
        let mut parts = token.splitn(3, |b| *b == b'\t');
        let (added, deleted, path) = (
            parts.next().ok_or_else(AppError::protocol)?,
            parts.next().ok_or_else(AppError::protocol)?,
            parts.next().ok_or_else(AppError::protocol)?,
        );
        let path = if path.is_empty() {
            tokens.next().ok_or_else(AppError::protocol)?;
            tokens.next().ok_or_else(AppError::protocol)?.to_vec()
        } else {
            path.to_vec()
        };
        stats
            .entry(path)
            .or_default()
            .push_back((number(added)?, number(deleted)?));
    }
    Ok(entries
        .into_iter()
        .map(|entry| {
            let stat = stats.get_mut(&entry.new_path).and_then(VecDeque::pop_front);
            (entry, stat)
        })
        .collect())
}

/// Changed files of a commit (`diff-tree`) or of the index (`diff-index --cached`).
pub(crate) fn changes(bytes: &[u8], limit: usize) -> Result<ParsedChanges> {
    Ok(build(records(bytes)?, limit))
}

/// `diff-files` output. Entries whose only difference is stat data (no numstat record,
/// same mode) are dropped, as `git status` does after refreshing. An unmerged path is
/// listed once, as `Unmerged`.
pub(crate) fn worktree_changes(bytes: &[u8], limit: usize) -> Result<ParsedChanges> {
    let records = records(bytes)?;
    let unmerged: HashSet<Vec<u8>> = records
        .iter()
        .filter(|(e, _)| e.status == b'U')
        .map(|(e, _)| e.new_path.clone())
        .collect();
    let kept = records
        .into_iter()
        .filter(|(e, stat)| {
            let stat_only =
                stat.is_none() && e.status == b'M' && e.old_mode == e.new_mode && !e.is_gitlink();
            !stat_only && (e.status == b'U' || !unmerged.contains(&e.new_path))
        })
        .collect();
    Ok(build(kept, limit))
}

/// `ls-files -z --others` output: new files without index entries.
pub(crate) fn untracked(bytes: &[u8], limit: usize) -> ParsedChanges {
    let records = bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|path| {
            let entry = RawChange {
                old_mode: "000000".into(),
                new_mode: String::new(),
                old_oid: String::new(),
                new_oid: String::new(),
                status: b'?',
                similarity: None,
                old_path: path.to_vec(),
                new_path: path.to_vec(),
            };
            (entry, Some((None, None)))
        })
        .collect();
    let mut parsed = build(records, limit);
    for file in &mut parsed.files {
        file.binary = false; // Unknown until the content is read.
    }
    parsed
}

fn build(records: Vec<(RawChange, Option<Stat>)>, limit: usize) -> ParsedChanges {
    let (mut additions, mut deletions) = (0u32, 0u32);
    let mut files = Vec::with_capacity(records.len().min(limit));
    let mut entries = Vec::with_capacity(records.len());
    for (index, (entry, stat)) in records.into_iter().enumerate() {
        let (added, deleted) = stat.unwrap_or((None, None));
        additions = additions.saturating_add(added.unwrap_or(0));
        deletions = deletions.saturating_add(deleted.unwrap_or(0));
        if index >= limit {
            entries.push(entry);
            continue;
        }
        let binary = added.is_none() && deleted.is_none() && !entry.is_gitlink();
        files.push(ChangedFile {
            index: index as u32,
            path: parse::display_path(&entry.new_path),
            old_path: (entry.old_path != entry.new_path)
                .then(|| parse::display_path(&entry.old_path)),
            kind: entry.kind(),
            similarity: entry
                .similarity
                .filter(|_| matches!(entry.status, b'R' | b'C')),
            additions: added,
            deletions: deleted,
            binary,
            old_mode: entry.old_mode.clone(),
            new_mode: entry.new_mode.clone(),
        });
        entries.push(entry);
    }
    ParsedChanges {
        entries,
        files,
        additions,
        deletions,
    }
}

pub(crate) struct ParsedPatch {
    pub content: DiffContent,
    pub lines: Vec<DiffLine>,
    pub hunk_starts: Vec<u32>,
    pub max_columns: u32,
    pub truncated: bool,
    pub lossy: bool,
    /// Approximate heap size, for the byte-bounded cache.
    pub bytes: usize,
    /// Blob sizes, cached with the patch so later pages spawn no process.
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
}

impl ParsedPatch {
    /// Outcome without text lines: binary, gated or too large.
    pub fn empty(content: DiffContent) -> Self {
        Self {
            content,
            lines: Vec::new(),
            hunk_starts: Vec::new(),
            max_columns: 0,
            truncated: false,
            lossy: false,
            bytes: 0,
            old_size: None,
            new_size: None,
        }
    }
}

fn hunk_start(header: &[u8], marker: u8) -> Option<u32> {
    let text = std::str::from_utf8(header).ok()?;
    let field = text
        .split(' ')
        .find(|f| f.as_bytes().first() == Some(&marker))?;
    field[1..].split(',').next()?.parse().ok()
}

/// Parses one file's unified patch. Headers before the first hunk are skipped.
pub(crate) fn patch(bytes: &[u8], line_limit: usize) -> Result<ParsedPatch> {
    let mut lines = Vec::new();
    let mut hunk_starts = Vec::new();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let (mut in_hunk, mut binary, mut truncated, mut lossy) = (false, false, false, false);
    let (mut max_columns, mut bytes_used) = (0u32, 0usize);
    let mut raw_lines = bytes.split(|b| *b == b'\n').peekable();
    while let Some(raw) = raw_lines.next() {
        if raw.is_empty() && raw_lines.peek().is_none() {
            break; // Trailing newline of the last line.
        }
        if raw.starts_with(b"diff --git ") {
            in_hunk = false;
            continue;
        }
        if !in_hunk && !raw.starts_with(b"@@ ") {
            if raw.starts_with(b"Binary files ") || raw.starts_with(b"GIT binary patch") {
                binary = true;
            }
            continue;
        }
        if lines.len() >= line_limit {
            truncated = true;
            break;
        }
        let (kind, body) = if raw.starts_with(b"@@ ") {
            in_hunk = true;
            old_no = hunk_start(raw, b'-').ok_or_else(AppError::protocol)?;
            new_no = hunk_start(raw, b'+').ok_or_else(AppError::protocol)?;
            hunk_starts.push(lines.len() as u32);
            (DiffLineKind::Hunk, raw)
        } else {
            match raw.first() {
                Some(b'+') => (DiffLineKind::Added, &raw[1..]),
                Some(b'-') => (DiffLineKind::Deleted, &raw[1..]),
                Some(b' ') => (DiffLineKind::Context, &raw[1..]),
                Some(b'\\') => (DiffLineKind::Note, &raw[1..]),
                // diff.suppressBlankEmpty prints an empty context line without the space.
                None => (DiffLineKind::Context, raw),
                _ => return Err(AppError::protocol()),
            }
        };
        let (old_line, new_line) = match kind {
            DiffLineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            DiffLineKind::Added => {
                new_no += 1;
                (None, Some(new_no - 1))
            }
            DiffLineKind::Deleted => {
                old_no += 1;
                (Some(old_no - 1), None)
            }
            _ => (None, None),
        };
        let cr = kind != DiffLineKind::Hunk && body.last() == Some(&b'\r');
        let body = if cr { &body[..body.len() - 1] } else { body };
        let decoded = match std::str::from_utf8(body) {
            Ok(text) => std::borrow::Cow::Borrowed(text),
            Err(_) => {
                lossy = true;
                String::from_utf8_lossy(body)
            }
        };
        let decoded = if kind == DiffLineKind::Note {
            decoded.trim_start()
        } else {
            &decoded
        };
        let mut text = String::with_capacity(decoded.len().min(DIFF_LINE_CHARS * 4));
        let (mut columns, mut clipped) = (0u32, false);
        for (count, ch) in decoded.chars().enumerate() {
            if count >= DIFF_LINE_CHARS {
                clipped = true;
                break;
            }
            columns += if ch == '\t' { 4 } else { 1 };
            text.push(ch);
        }
        max_columns = max_columns.max(columns);
        bytes_used += text.capacity() + std::mem::size_of::<DiffLine>();
        lines.push(DiffLine {
            kind,
            old_line,
            new_line,
            text,
            cr,
            clipped,
        });
    }
    let content = if binary {
        DiffContent::Binary
    } else if lines.is_empty() {
        DiffContent::NoContent
    } else {
        DiffContent::Text
    };
    Ok(ParsedPatch {
        content,
        lines,
        hunk_starts,
        max_columns,
        truncated,
        lossy,
        bytes: bytes_used,
        old_size: None,
        new_size: None,
    })
}

/// Content of an untracked file as an all-added patch, in the shape Git prints for a new
/// file. Binary uses Git's heuristic: a NUL byte within the first 8000 bytes.
pub(crate) fn untracked_patch(content: &[u8], line_limit: usize) -> Result<ParsedPatch> {
    if content[..content.len().min(8000)].contains(&0) {
        return Ok(ParsedPatch::empty(DiffContent::Binary));
    }
    if content.is_empty() {
        return Ok(ParsedPatch::empty(DiffContent::NoContent));
    }
    let complete = content.ends_with(b"\n");
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    let count = body.iter().filter(|b| **b == b'\n').count() + 1;
    let mut text = Vec::with_capacity(content.len() + count + 64);
    if count == 1 {
        text.extend_from_slice(b"@@ -0,0 +1 @@\n");
    } else {
        text.extend_from_slice(format!("@@ -0,0 +1,{count} @@\n").as_bytes());
    }
    for line in body.split(|b| *b == b'\n') {
        text.push(b'+');
        text.extend_from_slice(line);
        text.push(b'\n');
    }
    if !complete {
        text.extend_from_slice(b"\\ No newline at end of file\n");
    }
    patch(&text, line_limit)
}

pub(crate) fn too_large() -> AppError {
    AppError::new(
        ErrorCode::OutputLimit,
        "O diff deste arquivo excede o limite desta fase. O repositório não foi alterado.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const Z: &str = "0000000000000000000000000000000000000000";

    #[test]
    fn raw_and_numstat_pair_by_path_including_renames_and_binary() {
        let input = format!(
            ":100644 100644 {A} {B} M\0b.bin\0:000000 100644 {Z} {B} A\0esp aço 日本\t.txt\0:100644 100644 {A} {B} R097\0old\0new name\0-\t-\tb.bin\0003\t0\tesp aço 日本\t.txt\0001\t2\t\0old\0new name\0"
        );
        let parsed = changes(input.as_bytes(), 10).unwrap();
        assert_eq!(parsed.files.len(), 3);
        assert!(parsed.files[0].binary && parsed.files[0].additions.is_none());
        assert_eq!(parsed.files[1].kind, ChangeKind::Added);
        assert_eq!(parsed.files[1].path, "esp aço 日本\\t.txt");
        assert_eq!(parsed.files[1].additions, Some(3));
        let renamed = &parsed.files[2];
        assert_eq!(renamed.kind, ChangeKind::Renamed);
        assert_eq!(renamed.similarity, Some(97));
        assert_eq!(renamed.old_path.as_deref(), Some("old"));
        assert_eq!((renamed.additions, renamed.deletions), (Some(1), Some(2)));
        assert_eq!((parsed.additions, parsed.deletions), (4, 2));
        assert_eq!(
            parsed.entries[2].pathspec(),
            vec![&b"old"[..], &b"new name"[..]]
        );
    }

    #[test]
    fn list_limit_keeps_totals() {
        let mut input = String::new();
        for i in 0..5 {
            input.push_str(&format!(":000000 100644 {Z} {B} A\0f{i}\0"));
        }
        for i in 0..5 {
            input.push_str(&format!("2\t0\tf{i}\0"));
        }
        let parsed = changes(input.as_bytes(), 2).unwrap();
        assert_eq!((parsed.files.len(), parsed.entries.len()), (2, 5));
        assert_eq!(parsed.additions, 10);
    }

    #[test]
    fn malformed_raw_records_are_rejected() {
        assert!(changes(b":100644 M\0x\0", 10).is_err());
        assert!(changes(format!(":100644 100644 {A} zz M\0x\0").as_bytes(), 10).is_err());
    }

    #[test]
    fn patch_numbers_lines_and_keeps_cr_and_missing_newline() {
        let input = b"diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@ fn main\n keep\r\n-old\n+new\n \n\\ No newline at end of file\n@@ -10 +10,2 @@\n ten\n+eleven\n";
        let p = patch(input, 100).unwrap();
        assert_eq!(p.content, DiffContent::Text);
        assert_eq!(p.hunk_starts, vec![0, 6]);
        let l = &p.lines;
        assert_eq!(
            (l[1].old_line, l[1].new_line, l[1].cr),
            (Some(1), Some(1), true)
        );
        assert_eq!(l[1].text, "keep");
        assert_eq!((l[2].kind, l[2].old_line), (DiffLineKind::Deleted, Some(2)));
        assert_eq!((l[3].kind, l[3].new_line), (DiffLineKind::Added, Some(2)));
        assert_eq!((l[4].kind, l[4].text.as_str()), (DiffLineKind::Context, ""));
        assert_eq!(
            (l[5].kind, l[5].text.as_str()),
            (DiffLineKind::Note, "No newline at end of file")
        );
        assert_eq!((l[7].old_line, l[8].new_line), (Some(10), Some(11)));
    }

    #[test]
    fn patch_limits_lines_columns_and_flags_binary_and_invalid_utf8() {
        let mut input = b"@@ -1,3 +1,3 @@\n".to_vec();
        input.extend_from_slice(b"+\xff\xfe\n");
        input.push(b'+');
        input.extend(std::iter::repeat_n(b'x', DIFF_LINE_CHARS + 10));
        input.extend_from_slice(b"\n+\tz\n");
        let p = patch(&input, 3).unwrap();
        assert!(p.truncated && p.lossy);
        assert_eq!(p.lines.len(), 3);
        assert!(p.lines[2].clipped);
        assert_eq!(p.max_columns as usize, DIFF_LINE_CHARS);
        let binary = patch(b"diff --git a/b b/b\nBinary files a/b and b/b differ\n", 10).unwrap();
        assert_eq!(binary.content, DiffContent::Binary);
        let mode = patch(
            b"diff --git a/s b/s\nold mode 100644\nnew mode 100755\n",
            10,
        )
        .unwrap();
        assert_eq!(mode.content, DiffContent::NoContent);
        assert!(patch(b"@@ -x +1 @@\n", 10).is_err());
    }

    #[test]
    fn worktree_listing_drops_stat_only_entries_and_collapses_conflicts() {
        // `diff-files`: a stat-dirty file has no numstat record; an unmerged path appears
        // twice (U, then its worktree side) with one numstat record each.
        let input = format!(
            ":100644 100644 {A} {Z} M\0touched\0:000000 000000 {Z} {Z} U\0conflict\0:100644 100644 {A} {Z} M\0conflict\0:100644 100644 {A} {Z} M\0real\0000\t0\tconflict\0004\t0\tconflict\0001\t1\treal\0"
        );
        let parsed = worktree_changes(input.as_bytes(), 10).unwrap();
        let listed: Vec<(&str, ChangeKind, Option<u32>)> = parsed
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.kind, f.additions))
            .collect();
        assert_eq!(
            listed,
            [
                ("conflict", ChangeKind::Unmerged, Some(0)),
                ("real", ChangeKind::Modified, Some(1))
            ]
        );
        assert_eq!(parsed.files[1].index, 1);
        assert_eq!(parsed.entries.len(), 2);
    }

    #[test]
    fn untracked_content_becomes_an_all_added_patch() {
        let one = untracked_patch(b"solo", 10).unwrap();
        let kinds: Vec<DiffLineKind> = one.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [DiffLineKind::Hunk, DiffLineKind::Added, DiffLineKind::Note]
        );
        assert_eq!(one.lines[0].text, "@@ -0,0 +1 @@");
        let crlf = untracked_patch(b"a\r\nb\r\n", 10).unwrap();
        assert_eq!(crlf.lines[0].text, "@@ -0,0 +1,2 @@");
        assert!(crlf.lines[2].cr && crlf.lines[2].new_line == Some(2));
        let binary = untracked_patch(b"text\0more", 10).unwrap();
        assert_eq!(binary.content, DiffContent::Binary);
        let empty = untracked_patch(b"", 10).unwrap();
        assert_eq!(empty.content, DiffContent::NoContent);
        let listed = untracked(b"a.txt\0dir/b.txt\0", 1);
        assert_eq!(listed.entries.len(), 2);
        assert_eq!(listed.files.len(), 1);
        assert_eq!(listed.files[0].kind, ChangeKind::Untracked);
        assert!(!listed.files[0].binary);
    }
}
