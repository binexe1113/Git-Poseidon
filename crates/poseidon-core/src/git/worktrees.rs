//! Read-only view of the worktree registry (T23-B1). Listing never prunes, repairs
//! or rewrites anything: an entry Git or the file system cannot vouch for is shown
//! as it is, never fixed. Runs in the real repository context: the private
//! execution context hides the main worktree's HEAD (docs/23, P1).
use super::{os_path, Git, RepositoryLocation};
use crate::domain::{AppError, ErrorCode, Result, WorktreeAvailability, WorktreeKind};
use crate::process::{Cancellation, Limits};
use std::path::{Path, PathBuf};

/// Same ceiling as `OtherHeadGuards`.
pub(crate) const LIMIT: usize = 256;
const OUTPUT_BYTES: usize = 1024 * 1024;
const LINK_BYTES: u64 = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Record {
    pub path: Vec<u8>,
    pub head: Option<String>,
    pub branch: Option<Vec<u8>>,
    pub detached: bool,
    pub bare: bool,
    /// `Some` when locked; the reason may be empty.
    pub locked: Option<Vec<u8>>,
    pub prunable: Option<Vec<u8>>,
}

fn malformed() -> AppError {
    AppError::new(
        ErrorCode::InvalidOutput,
        "O Git devolveu uma lista de worktrees inesperada. Nada foi interpretado.",
    )
}

fn set_once<T>(slot: &mut Option<T>, value: Option<T>) -> Result<()> {
    if slot.is_some() {
        return Err(malformed());
    }
    *slot = Some(value.ok_or_else(malformed)?);
    Ok(())
}

/// `git worktree list --porcelain -z`: each attribute ends in NUL, each entry in an
/// extra NUL. A malformed entry rejects the whole listing, so no partially parsed
/// identity is ever used. Attributes from newer Git versions are ignored.
pub(crate) fn parse(bytes: &[u8], oid_len: usize) -> Result<(Vec<Record>, bool)> {
    let body = bytes
        .strip_suffix(b"\0")
        .filter(|b| b.ends_with(b"\0"))
        .ok_or_else(malformed)?;
    let mut records = Vec::new();
    let mut truncated = false;
    let mut current: Option<Record> = None;
    for field in body.split(|b| *b == 0) {
        if field.is_empty() {
            let record = current.take().ok_or_else(malformed)?;
            if (record.detached && record.branch.is_some())
                || (record.bare && (record.head.is_some() || record.branch.is_some()))
            {
                return Err(malformed());
            }
            if records.len() < LIMIT {
                records.push(record);
            } else {
                truncated = true;
            }
            continue;
        }
        let (name, value) = match field.iter().position(|b| *b == b' ') {
            Some(i) => (&field[..i], Some(&field[i + 1..])),
            None => (field, None),
        };
        if name == b"worktree" {
            let path = value.ok_or_else(malformed)?;
            if current.is_some() || !Path::new(&os_path(path)?).is_absolute() {
                return Err(malformed());
            }
            current = Some(Record {
                path: path.to_vec(),
                ..Record::default()
            });
            continue;
        }
        let record = current.as_mut().ok_or_else(malformed)?;
        match name {
            b"HEAD" => set_once(
                &mut record.head,
                value
                    .filter(|v| v.len() == oid_len && v.iter().all(u8::is_ascii_hexdigit))
                    .map(|v| String::from_utf8_lossy(v).into_owned()),
            )?,
            b"branch" => set_once(
                &mut record.branch,
                value
                    .filter(|v| v.len() > 5 && v.starts_with(b"refs/"))
                    .map(<[u8]>::to_vec),
            )?,
            b"detached" | b"bare" => {
                let flag = if name == b"bare" {
                    &mut record.bare
                } else {
                    &mut record.detached
                };
                if *flag || value.is_some() {
                    return Err(malformed());
                }
                *flag = true;
            }
            b"locked" => set_once(&mut record.locked, Some(value.unwrap_or_default().to_vec()))?,
            b"prunable" => set_once(
                &mut record.prunable,
                Some(value.unwrap_or_default().to_vec()),
            )?,
            _ => {}
        }
    }
    if current.is_some() || records.is_empty() {
        return Err(malformed());
    }
    Ok((records, truncated))
}

/// One resolved entry. `git_dir` and `stamp` exist only for available entries.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub record: Record,
    pub kind: WorktreeKind,
    pub path: PathBuf,
    pub availability: WorktreeAvailability,
    pub reason: Option<&'static str>,
    pub git_dir: Option<PathBuf>,
    /// dev/inode of the checkout root and of its Git directory.
    pub stamp: Option<[u64; 4]>,
}

pub(crate) struct Registry {
    pub entries: Vec<Entry>,
    pub truncated: bool,
    /// Identity of the registry: paths, links, lock/prune state and inodes. HEAD
    /// and branch are left out: they change with ordinary work in any worktree.
    pub digest: [u8; 32],
}

/// A small regular file, without following a final symlink or blocking on a FIFO.
fn read_link(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(LINK_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LINK_BYTES {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    Ok(bytes)
}

/// Link target as Git reads it: absolute, or relative to the link's directory.
fn target(base: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let path = PathBuf::from(os_path(bytes).ok()?);
    base.join(path).canonicalize().ok()
}

pub(crate) fn stamp(root: &Path, git_dir: &Path) -> Option<[u64; 4]> {
    let (root, git) = (
        std::fs::symlink_metadata(root).ok()?,
        std::fs::symlink_metadata(git_dir).ok()?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some([root.dev(), root.ino(), git.dev(), git.ino()])
    }
    #[cfg(not(unix))]
    {
        let _ = (root, git);
        None
    }
}

fn resolve(record: Record, main: bool, common: &Path) -> Result<Entry> {
    if !main && record.bare {
        return Err(malformed());
    }
    let path = PathBuf::from(os_path(&record.path)?);
    let kind = match (main, record.bare) {
        (true, true) => WorktreeKind::Bare,
        (true, false) => WorktreeKind::Main,
        _ => WorktreeKind::Linked,
    };
    let mut entry = Entry {
        record,
        kind,
        path,
        availability: WorktreeAvailability::Available,
        reason: None,
        git_dir: None,
        stamp: None,
    };
    let unavailable = |mut entry: Entry, availability, reason| {
        entry.availability = availability;
        entry.reason = Some(reason);
        Ok(entry)
    };
    use WorktreeAvailability::*;
    if !main && entry.record.locked.as_deref() == Some(b"initializing") {
        return unavailable(entry, Initializing, "Criação em andamento ou interrompida (locked: initializing). O Poseidon não altera este registro.");
    }
    if entry.record.prunable.is_some() {
        // Git also calls an unreadable but existing checkout prunable: pruning
        // it would drop the metadata of a live worktree, so say what it is.
        return if std::fs::symlink_metadata(&entry.path).is_ok() {
            unavailable(entry, Inaccessible, "O diretório existe, mas o Git não consegue lê-lo e o considera podável. Não remova o registro sem conferir permissões.")
        } else {
            unavailable(entry, Missing, "O Git informa que este registro pode ser podado: o diretório não existe mais nesse caminho.")
        };
    }
    let meta = match std::fs::symlink_metadata(&entry.path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return unavailable(entry, Missing, "O diretório registrado não existe.");
        }
        Err(_) => {
            return unavailable(
                entry,
                Inaccessible,
                "O diretório registrado não pode ser lido.",
            );
        }
    };
    // Git records the real path; a symlink there now means the path changed.
    if !meta.is_dir() || entry.path.canonicalize().ok().as_ref() != Some(&entry.path) {
        return unavailable(
            entry,
            Inconsistent,
            "O caminho registrado mudou: não é mais o mesmo diretório real.",
        );
    }
    let git_dir = match entry.kind {
        WorktreeKind::Bare => {
            if entry.path != common {
                return unavailable(
                    entry,
                    Inconsistent,
                    "O repositório bare não corresponde ao common-dir desta sessão.",
                );
            }
            common.to_path_buf()
        }
        WorktreeKind::Main => {
            let dot = entry.path.join(".git");
            if !std::fs::symlink_metadata(&dot).is_ok_and(|m| m.is_dir())
                || dot.canonicalize().ok().as_deref() != Some(common)
            {
                return unavailable(
                    entry,
                    Inconsistent,
                    "O diretório .git do worktree principal não corresponde ao common-dir.",
                );
            }
            common.to_path_buf()
        }
        WorktreeKind::Linked => {
            let link = match read_link(&entry.path.join(".git")) {
                Ok(link) => link,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    return unavailable(
                        entry,
                        Inaccessible,
                        "O arquivo .git do worktree não pode ser lido.",
                    );
                }
                Err(_) => {
                    return unavailable(
                        entry,
                        Inconsistent,
                        "O worktree não tem um arquivo .git válido.",
                    );
                }
            };
            let admin = link
                .strip_prefix(b"gitdir: ")
                .and_then(|t| target(&entry.path, t))
                .filter(|a| {
                    a.parent() == Some(common.join("worktrees").as_path())
                        && a.file_name().is_some()
                });
            let Some(admin) = admin else {
                return unavailable(
                    entry,
                    Inconsistent,
                    "O arquivo .git aponta para fora do registro deste repositório.",
                );
            };
            // The registry must point back at this checkout.
            let back = read_link(&admin.join("gitdir"))
                .ok()
                .and_then(|bytes| target(&admin, &bytes));
            if back.is_none() || back != entry.path.join(".git").canonicalize().ok() {
                return unavailable(
                    entry,
                    Inconsistent,
                    "O registro do Git aponta para outro diretório.",
                );
            }
            admin
        }
    };
    entry.stamp = stamp(&entry.path, &git_dir);
    entry.git_dir = Some(git_dir);
    Ok(entry)
}

fn digest(entries: &[Entry]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    for e in entries {
        field(&[e.kind as u8, e.availability as u8]);
        field(&e.record.path);
        for optional in [&e.record.locked, &e.record.prunable] {
            field(optional.as_deref().map_or(b"-".as_slice(), |v| v));
            field(&[optional.is_some() as u8]);
        }
        field(
            e.git_dir
                .as_ref()
                .map_or(b"".as_slice(), |p| p.as_os_str().as_encoded_bytes()),
        );
        for n in e.stamp.unwrap_or_default() {
            field(&n.to_le_bytes());
        }
    }
    hash.finalize().into()
}

impl Git {
    /// The registry as Git reports it now, plus read-only identity checks.
    pub(crate) async fn worktree_registry(
        &self,
        location: &RepositoryLocation,
    ) -> Result<Registry> {
        // Git runs from the session folder: if it vanished, say so, instead of
        // reporting a Git start-up failure.
        if !std::fs::metadata(&location.root).is_ok_and(|m| m.is_dir()) {
            return Err(AppError::new(
                ErrorCode::InvalidRepository,
                "A pasta da sessão aberta não existe mais: o worktree pode ter sido removido ou movido. Abra outro worktree ou repositório.",
            ));
        }
        let output = self
            .process
            .run_with(
                &location.root,
                &["worktree", "list", "--porcelain", "-z"],
                Limits {
                    stdout_bytes: OUTPUT_BYTES,
                    ..Limits::default()
                },
                &Cancellation::default(),
            )
            .await?;
        if !output.success {
            return Err(AppError::new(
                ErrorCode::CommandFailed,
                "O Git não conseguiu listar os worktrees. Verifique permissões e safe.directory no terminal.",
            ));
        }
        let oid_len = if location.object_format == "sha256" {
            64
        } else {
            40
        };
        let (records, truncated) = parse(&output.stdout, oid_len)?;
        let common = location.common_dir.canonicalize().map_err(|_| {
            AppError::new(
                ErrorCode::InvalidRepository,
                "O diretório comum do repositório não está mais acessível.",
            )
        })?;
        let entries = records
            .into_iter()
            .enumerate()
            .map(|(i, record)| resolve(record, i == 0, &common))
            .collect::<Result<Vec<_>>>()?;
        Ok(Registry {
            digest: digest(&entries),
            entries,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "0123456789abcdef0123456789abcdef01234567";

    fn ok(bytes: &[u8]) -> Vec<Record> {
        parse(bytes, 40).unwrap().0
    }
    fn bad(bytes: &[u8]) {
        assert!(
            parse(bytes, 40).is_err(),
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }

    #[test]
    fn parses_main_linked_detached_locked_prunable_and_raw_paths() {
        let mut bytes = format!("worktree /r\0HEAD {A}\0branch refs/heads/main\0\0").into_bytes();
        // Newline, Unicode and an invalid UTF-8 byte in one path.
        bytes.extend_from_slice(b"worktree /w x\n\xe6\x97\xa5\xff\0");
        bytes.extend_from_slice(format!("HEAD {A}\0detached\0locked\0\0").as_bytes());
        bytes.extend_from_slice(format!("worktree /gone\0HEAD {A}\0branch refs/heads/f\0locked usb disk\0prunable gitdir file points to non-existent location\0future-attribute value\0\0").as_bytes());
        let records = ok(&bytes);
        assert_eq!(records.len(), 3);
        assert_eq!(
            records[0].branch.as_deref(),
            Some(b"refs/heads/main".as_slice())
        );
        assert!(records[1].path.ends_with(b"x\n\xe6\x97\xa5\xff"));
        assert!(records[1].detached && records[1].locked == Some(Vec::new()));
        assert_eq!(records[2].locked.as_deref(), Some(b"usb disk".as_slice()));
        assert!(records[2].prunable.is_some());
    }

    #[test]
    fn parses_bare_main_and_sha256() {
        let b = "f".repeat(64);
        let r = parse(
            format!("worktree /b.git\0bare\0\0worktree /w\0HEAD {b}\0branch refs/heads/x\0\0")
                .as_bytes(),
            64,
        )
        .unwrap()
        .0;
        assert!(r[0].bare && r[0].head.is_none());
        assert_eq!(r[1].head.as_deref(), Some(b.as_str()));
        assert!(parse(format!("worktree /w\0HEAD {A}\0\0").as_bytes(), 64).is_err());
    }

    #[test]
    fn rejects_malformed_listings_entirely() {
        bad(b"");
        bad(b"worktree /r\0");
        bad(format!("worktree /r\0HEAD {A}\0").as_bytes());
        bad(format!("HEAD {A}\0\0").as_bytes());
        bad(b"worktree relative\0\0");
        bad(b"worktree \0\0");
        bad(format!("worktree /r\0HEAD {A}\0HEAD {A}\0\0").as_bytes());
        bad(b"worktree /r\0HEAD xyz\0\0");
        bad(format!("worktree /r\0HEAD {A}\0branch main\0\0").as_bytes());
        bad(format!("worktree /r\0HEAD {A}\0branch refs/heads/m\0detached\0\0").as_bytes());
        bad(b"worktree /r\0worktree /s\0\0");
        bad(b"worktree /r\0detached x\0\0");
        bad(format!("worktree /r\0HEAD {A}\0\0\0").as_bytes());
    }

    #[test]
    fn keeps_the_first_limit_entries_and_reports_truncation() {
        let mut raw = Vec::new();
        for i in 0..LIMIT + 3 {
            raw.extend_from_slice(format!("worktree /w{i}\0HEAD {A}\0detached\0\0").as_bytes());
        }
        let (records, truncated) = parse(&raw, 40).unwrap();
        assert_eq!(records.len(), LIMIT);
        assert!(truncated);
    }
}
