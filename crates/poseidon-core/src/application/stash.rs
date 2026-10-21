use super::branches::ensure_location;
use super::*;

pub(super) struct Listing {
    session: (u32, u32),
    revision: u32,
    raw: Vec<u8>,
    stamp: Option<FileStamp>,
}
fn log_stamp(l: &RepositoryLocation) -> Option<FileStamp> {
    file_stamp(&l.common_dir.join("logs/refs/stash"))
}
fn entries(raw: &[u8], revision: u32) -> Result<Vec<StashEntry>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let fields: Vec<_> = raw
        .strip_suffix(&[0])
        .unwrap_or(raw)
        .split(|b| *b == 0)
        .collect();
    if !fields.len().is_multiple_of(5) {
        return Err(unsupported("Reflog de stash inválido."));
    }
    fields
        .as_chunks::<5>()
        .0
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let oid = parse::text(f[0]);
            if !parse::valid_oid(&oid) {
                return Err(unsupported("OID de stash inválido."));
            }
            let message = parse::text(f[1]);
            let branch = message
                .strip_prefix("On ")
                .or_else(|| message.strip_prefix("WIP on "))
                .and_then(|v| v.split_once(':'))
                .map(|v| v.0.to_owned())
                .filter(|branch| branch != "(no branch)");
            Ok(StashEntry {
                selection: StashSelection {
                    revision,
                    index: i as u32,
                    oid,
                },
                reference: format!("stash@{{{i}}}"),
                message,
                timestamp: parse::text(f[2])
                    .rsplit_once("@{")
                    .and_then(|(_, date)| date.strip_suffix('}'))
                    .ok_or_else(|| unsupported("Data do reflog inválida."))?
                    .to_owned(),
                branch,
            })
        })
        .collect()
}
impl RepositoryService {
    pub async fn stashes(&self, session_id: u32, generation: u32) -> Result<StashListResponse> {
        let start = Instant::now();
        let _permit = self.stash_queries.try_acquire().map_err(|_| busy())?;
        let l = self.session_location(session_id, generation)?;
        let git = Git::discover()?;
        ensure_location(&git, &l).await?;
        let stamp = log_stamp(&l);
        let raw = git.stash_log(&l).await?;
        if stamp != log_stamp(&l) {
            return Err(changed());
        }
        self.session_location(session_id, generation)?;
        let mut listing = self.stash_listing.lock().map_err(|_| busy())?;
        let revision = match listing
            .as_ref()
            .filter(|v| v.session == (session_id, generation) && v.raw == raw && v.stamp == stamp)
        {
            Some(v) => v.revision,
            None => self.next_plan.fetch_add(1, Ordering::Relaxed),
        };
        let mut data = entries(&raw, revision)?;
        let has_more = data.len() > 1000;
        data.truncate(1000);
        *listing = Some(Listing {
            session: (session_id, generation),
            revision,
            raw,
            stamp,
        });
        Ok(StashListResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data: StashList {
                entries: data,
                has_more,
                elapsed_ms: elapsed_ms(start),
            },
        })
    }
    async fn resolve_stash(
        &self,
        git: &Git,
        l: &RepositoryLocation,
        session: (u32, u32),
        selection: &StashSelection,
    ) -> Result<()> {
        let (raw, stamp) = {
            let listing = self.stash_listing.lock().map_err(|_| busy())?;
            let v = listing
                .as_ref()
                .filter(|v| v.session == session && v.revision == selection.revision)
                .ok_or_else(stale)?;
            let e = entries(&v.raw, v.revision)?;
            if e.get(selection.index as usize)
                .is_none_or(|e| e.selection != *selection)
            {
                return Err(stale());
            }
            (v.raw.clone(), v.stamp.clone())
        };
        if log_stamp(l) != stamp || git.stash_log(l).await? != raw {
            return Err(stale());
        }
        Ok(())
    }
    pub async fn stash_details(
        &self,
        session_id: u32,
        generation: u32,
        selection: StashSelection,
    ) -> Result<StashDetailsResponse> {
        // Serialize metadata reads with mutations; immutable patches use the existing diff gate.
        let _permit = self.mutation.try_acquire().map_err(|_| writing())?;
        let l = self.session_location(session_id, generation)?;
        let git = Git::discover()?;
        ensure_location(&git, &l).await?;
        self.resolve_stash(&git, &l, (session_id, generation), &selection)
            .await?;
        let parents = git.stash_parents(&l, &selection.oid).await?;
        let mut data = Vec::new();
        for oid in std::iter::once(&selection.oid).chain(parents.iter().skip(1)) {
            data.push(self.commit_files(session_id, generation, oid).await?.data);
        }
        self.resolve_stash(&git, &l, (session_id, generation), &selection)
            .await?;
        Ok(StashDetailsResponse {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            data,
        })
    }
    // Keep the IPC signatures to fail closed for stale/malicious clients. No
    // plan is issued and no Git process (including inspection) is started here.
    // Snapshot/revalidation cannot seal external configuration or positional
    // reflog deletion. Restore writes only with a proven execution policy/CAS.
    pub async fn prepare_stash(
        &self,
        session_id: u32,
        generation: u32,
        _action: StashAction,
    ) -> Result<StashPlanResponse> {
        self.session_location(session_id, generation)?;
        Err(mutations_unavailable())
    }
    pub async fn execute_stash(
        &self,
        session_id: u32,
        generation: u32,
        _token: u32,
    ) -> Result<BranchChangeResponse> {
        self.session_location(session_id, generation)?;
        Err(mutations_unavailable())
    }
}
fn mutations_unavailable() -> AppError {
    unsupported("Stash está disponível somente para leitura nesta versão. Criar, aplicar e excluir estão desabilitados: a execução de programas externos e a exclusão concorrente de entradas não possuem garantias suficientes. Nenhum comando Git foi iniciado por esta solicitação.")
}

#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
mod test_support;
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn background_listing_does_not_contend_with_mutation_queue() {
        let f = super::test_support::Fixture::new();
        f.write("a", "base\n");
        f.commit("base");
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        let _held = s.mutation.acquire().await.unwrap();
        assert!(s
            .stashes(r.session_id, r.generation)
            .await
            .unwrap()
            .data
            .entries
            .is_empty());
        let _read = s.stash_queries.acquire().await.unwrap();
        assert!(matches!(
            s.prepare_stash(
                r.session_id,
                r.generation,
                StashAction::Create {
                    message: String::new(),
                    include_untracked: true
                }
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::UnsupportedState
        ));
    }
    #[tokio::test]
    async fn unavailable_mutations_never_issue_or_accept_confirmation_tokens() {
        let f = super::test_support::Fixture::new();
        f.write("a", "base\n");
        f.commit("base");
        f.write("a", "precious\n");
        let s = RepositoryService::default();
        let r = s.open(&f.root).await.unwrap();
        s.set_trust(r.session_id, r.generation, true).await.unwrap();
        let error = s
            .prepare_stash(
                r.session_id,
                r.generation,
                StashAction::Create {
                    message: "no plan".into(),
                    include_untracked: true,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::UnsupportedState));
        for token in [0, 1, u32::MAX, 1] {
            assert!(matches!(
                s.execute_stash(r.session_id, r.generation, token)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::UnsupportedState
            ));
        }
        assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"precious\n");
        assert!(!f
            .output(&["show-ref", "--verify", "refs/stash"])
            .status
            .success());
    }
}
