use crate::domain::{AppError, Branch, Commit, FileChange, Result, WorkingStatus, CHANGE_LIMIT};

pub(crate) fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Keep Unicode legible, but distinguish control characters and invalid UTF-8 bytes.
pub(crate) fn display_path(mut bytes: &[u8]) -> String {
    let mut result = String::new();
    while !bytes.is_empty() {
        let (valid, invalid) = match std::str::from_utf8(bytes) {
            Ok(value) => (value, 0),
            Err(error) => (
                std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default(),
                error
                    .error_len()
                    .unwrap_or(bytes.len() - error.valid_up_to()),
            ),
        };
        for ch in valid.chars() {
            if ch.is_control() || ch == '\\' {
                result.extend(ch.escape_default());
            } else {
                result.push(ch);
            }
        }
        bytes = &bytes[valid.len()..];
        for byte in &bytes[..invalid] {
            result.push_str(&format!("\\x{byte:02x}"));
        }
        bytes = &bytes[invalid..];
    }
    result
}

pub(crate) fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn commits(bytes: &[u8]) -> Result<Vec<Commit>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut fields: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    if fields.last() == Some(&&b""[..]) {
        fields.pop();
    }
    let (records, remainder) = fields.as_chunks::<10>();
    if !remainder.is_empty() {
        return Err(AppError::protocol());
    }
    records
        .iter()
        .map(|f| {
            let oid = text(f[0]);
            let tree_oid = text(f[1]);
            let parents: Vec<String> = text(f[2]).split_whitespace().map(str::to_owned).collect();
            if !valid_oid(&oid) || !valid_oid(&tree_oid) || parents.iter().any(|p| !valid_oid(p)) {
                return Err(AppError::protocol());
            }
            Ok(Commit {
                oid,
                tree_oid,
                parents,
                author_name: text(f[3]),
                author_email: text(f[4]),
                authored_at: text(f[5]),
                committer_name: text(f[6]),
                committer_email: text(f[7]),
                committed_at: text(f[8]),
                summary: text(f[9]),
            })
        })
        .collect()
}

pub(crate) fn branches(bytes: &[u8]) -> Result<Vec<Branch>> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|r| !r.is_empty())
        .map(|record| {
            let f: Vec<&[u8]> = record.split(|b| *b == 0).collect();
            if f.len() != 6 {
                return Err(AppError::protocol());
            }
            let full_ref = text(f[0]);
            let oid = text(f[1]);
            if !valid_oid(&oid) {
                return Err(AppError::protocol());
            }
            let remote = full_ref.starts_with("refs/remotes/");
            let name = full_ref
                .strip_prefix(if remote {
                    "refs/remotes/"
                } else {
                    "refs/heads/"
                })
                .ok_or_else(AppError::protocol)?
                .to_owned();
            Ok(Branch {
                full_ref,
                name,
                oid,
                remote,
                current: f[3] == b"*",
                upstream: (!f[2].is_empty()).then(|| text(f[2])),
                symbolic_target: (!f[4].is_empty()).then(|| text(f[4])),
            })
        })
        .collect()
}

pub(crate) fn status(bytes: &[u8]) -> Result<WorkingStatus> {
    let mut result = WorkingStatus {
        available: true,
        total_files: 0,
        has_more_changes: false,
        staged: 0,
        modified: 0,
        untracked: 0,
        conflicts: 0,
        changes: Vec::new(),
    };
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let kind = record[0];
        if kind == b'#' {
            continue;
        }
        let change = match kind {
            b'?' => FileChange {
                path: display_path(record.get(2..).ok_or_else(AppError::protocol)?),
                original_path: None,
                index_status: "?".into(),
                worktree_status: "?".into(),
                untracked: true,
                conflicted: false,
                submodule: false,
            },
            b'1' | b'2' | b'u' => {
                // Porcelain v2 has a fixed number of space-delimited metadata fields;
                // the remaining bytes are one path, including spaces/newlines.
                let fields: Vec<&[u8]> = record
                    .splitn(
                        match kind {
                            b'1' => 9,
                            b'2' => 10,
                            _ => 11,
                        },
                        |b| *b == b' ',
                    )
                    .collect();
                let expected = match kind {
                    b'1' => 9,
                    b'2' => 10,
                    _ => 11,
                };
                if fields.len() != expected || fields[1].len() != 2 {
                    return Err(AppError::protocol());
                }
                FileChange {
                    path: display_path(fields[expected - 1]),
                    original_path: if kind == b'2' {
                        Some(display_path(records.next().ok_or_else(AppError::protocol)?))
                    } else {
                        None
                    },
                    index_status: text(&fields[1][..1]),
                    worktree_status: text(&fields[1][1..]),
                    untracked: false,
                    conflicted: kind == b'u',
                    submodule: fields[2].starts_with(b"S"),
                }
            }
            b'!' => continue,
            _ => return Err(AppError::protocol()),
        };
        result.total_files += 1;
        if change.untracked {
            result.untracked += 1;
        } else if change.conflicted {
            result.conflicts += 1;
        } else {
            if change.index_status != "." {
                result.staged += 1;
            }
            if change.worktree_status != "." {
                result.modified += 1;
            }
        }
        if result.changes.len() < CHANGE_LIMIT {
            result.changes.push(change);
        }
    }
    result.has_more_changes = result.total_files as usize > CHANGE_LIMIT;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_protocol_fails_closed() {
        assert!(commits(b"bad\0record\0").is_err());
        assert!(status(b"2 too short\0").is_err());
        assert!(branches(b"invalid\0").is_err());
    }

    #[test]
    fn paths_are_displayed_without_losing_byte_distinctions() {
        assert_eq!(display_path(b"a\n\\\xff"), "a\\n\\\\\\xff");
        assert_eq!(display_path("ação.txt".as_bytes()), "ação.txt");
    }
}
