#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use poseidon_core::{
    domain::{
        AppError, CommitFilesResponse, CommitPlanResponse, CreateCommitResponse, DetailResponse,
        FileDiffResponse, GitInstallation, HistoryResponse, IndexChangeResponse, SnapshotResponse,
        WorkingArea, WorkingChangesResponse,
    },
    RepositoryService, TrustStore,
};
use tauri::{Manager, State};

#[tauri::command]
async fn check_git(service: State<'_, RepositoryService>) -> Result<GitInstallation, AppError> {
    service.check_git().await
}

#[tauri::command]
async fn open_repository(
    service: State<'_, RepositoryService>,
    path: String,
) -> Result<SnapshotResponse, AppError> {
    service.open(std::path::Path::new(&path)).await
}

#[tauri::command]
async fn refresh_repository(
    service: State<'_, RepositoryService>,
    session_id: u32,
) -> Result<SnapshotResponse, AppError> {
    service.refresh(session_id).await
}

#[tauri::command]
async fn get_commit(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    oid: String,
    offset: Option<u32>,
) -> Result<DetailResponse, AppError> {
    service
        .commit_at(session_id, generation, &oid, offset)
        .await
}

#[tauri::command]
async fn get_history(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    offset: u32,
) -> Result<HistoryResponse, AppError> {
    service.history_page(session_id, generation, offset).await
}

#[tauri::command]
async fn get_commit_files(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    oid: String,
) -> Result<CommitFilesResponse, AppError> {
    service.commit_files(session_id, generation, &oid).await
}

/// Files are addressed by index into `get_commit_files`; no path crosses IPC inbound.
#[tauri::command]
async fn get_file_diff(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    oid: String,
    index: u32,
    offset: u32,
    allow_large: bool,
) -> Result<FileDiffResponse, AppError> {
    service
        .file_diff(session_id, generation, &oid, index, offset, allow_large)
        .await
}

#[tauri::command]
async fn get_working_changes(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
) -> Result<WorkingChangesResponse, AppError> {
    service.working_changes(session_id, generation).await
}

/// Local files are addressed by listing revision, group and index; no path crosses IPC.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn get_working_diff(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    revision: u32,
    area: WorkingArea,
    index: u32,
    offset: u32,
    allow_large: bool,
) -> Result<FileDiffResponse, AppError> {
    service
        .working_diff(
            session_id,
            generation,
            revision,
            area,
            index,
            offset,
            allow_large,
        )
        .await
}

/// Grants or revokes trust in the open repository. Called only from the explicit trust
/// dialog or the revoke action; opening a repository never grants it.
#[tauri::command]
async fn set_repository_trust(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    trusted: bool,
) -> Result<(), AppError> {
    service.set_trust(session_id, generation, trusted).await
}

/// Stages one listed file; no path or Git argument crosses IPC.
#[tauri::command]
async fn stage_file(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    revision: u32,
    area: WorkingArea,
    index: u32,
) -> Result<IndexChangeResponse, AppError> {
    service
        .stage_file(session_id, generation, revision, area, index)
        .await
}

#[tauri::command]
async fn unstage_file(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    revision: u32,
    index: u32,
) -> Result<IndexChangeResponse, AppError> {
    service
        .unstage_file(session_id, generation, revision, index)
        .await
}

#[tauri::command]
async fn prepare_commit(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    revision: u32,
) -> Result<CommitPlanResponse, AppError> {
    service
        .prepare_commit(session_id, generation, revision)
        .await
}
#[tauri::command]
async fn create_commit(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    token: u32,
    message: String,
) -> Result<CreateCommitResponse, AppError> {
    service
        .create_commit(session_id, generation, token, &message)
        .await
}
#[tauri::command]
async fn set_commit_hook_trust(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    trusted: bool,
) -> Result<(), AppError> {
    service
        .set_commit_hook_trust(session_id, generation, trusted)
        .await
}

#[tauri::command]
async fn prepare_branch(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    action: poseidon_core::domain::BranchAction,
) -> Result<poseidon_core::domain::BranchPlanResponse, AppError> {
    service.prepare_branch(session_id, generation, action).await
}
#[tauri::command]
async fn execute_branch(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    token: u32,
) -> Result<poseidon_core::domain::BranchChangeResponse, AppError> {
    service.execute_branch(session_id, generation, token).await
}
#[tauri::command]
async fn set_branch_hook_trust(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    trusted: bool,
) -> Result<(), AppError> {
    service
        .set_branch_hook_trust(session_id, generation, trusted)
        .await
}

#[tauri::command]
async fn list_stashes(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
) -> Result<poseidon_core::domain::StashListResponse, AppError> {
    service.stashes(session_id, generation).await
}
#[tauri::command]
async fn get_stash_details(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    selection: poseidon_core::domain::StashSelection,
) -> Result<poseidon_core::domain::StashDetailsResponse, AppError> {
    service
        .stash_details(session_id, generation, selection)
        .await
}
#[tauri::command]
async fn prepare_stash(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    action: poseidon_core::domain::StashAction,
) -> Result<poseidon_core::domain::StashPlanResponse, AppError> {
    service.prepare_stash(session_id, generation, action).await
}
#[tauri::command]
async fn execute_stash(
    service: State<'_, RepositoryService>,
    session_id: u32,
    generation: u32,
    token: u32,
) -> Result<poseidon_core::domain::BranchChangeResponse, AppError> {
    service.execute_stash(session_id, generation, token).await
}

#[cfg(feature = "runtime-bench")]
struct BenchClock {
    start: std::time::Instant,
    ready: std::sync::atomic::AtomicBool,
}

/// Opt-in diagnostic only. No arbitrary payload, path or Git command.
#[cfg(feature = "runtime-bench")]
#[tauri::command]
fn runtime_probe(clock: State<'_, BenchClock>, sequence: u32, ready: bool) -> (u32, u64) {
    let micros = clock.start.elapsed().as_micros().min(u64::MAX as u128) as u64;
    if ready && !clock.ready.swap(true, std::sync::atomic::Ordering::Relaxed) {
        println!("POSEIDON_BENCH_READY_US={micros}");
    }
    (sequence, micros)
}

fn main() {
    #[cfg(feature = "runtime-bench")]
    let clock = BenchClock {
        start: std::time::Instant::now(),
        ready: std::sync::atomic::AtomicBool::new(false),
    };
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Trust lives in the app's own data directory (XDG_DATA_HOME on Linux).
        .setup(|app| {
            let store = TrustStore::file(app.path().app_data_dir()?.join("trust.json"));
            app.manage(RepositoryService::with_trust(store));
            Ok(())
        });
    #[cfg(not(feature = "runtime-bench"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        check_git,
        open_repository,
        refresh_repository,
        get_commit,
        get_history,
        get_commit_files,
        get_file_diff,
        get_working_changes,
        get_working_diff,
        set_repository_trust,
        stage_file,
        unstage_file,
        prepare_commit,
        create_commit,
        set_commit_hook_trust,
        prepare_branch,
        execute_branch,
        set_branch_hook_trust,
        list_stashes,
        get_stash_details,
        prepare_stash,
        execute_stash
    ]);
    #[cfg(feature = "runtime-bench")]
    let builder = builder.manage(clock)
        .on_page_load(|webview, event| {
            if event.event() == tauri::webview::PageLoadEvent::Finished {
                let _ = webview.eval("requestAnimationFrame(() => requestAnimationFrame(() => window.__TAURI_INTERNALS__.invoke('runtime_probe', {sequence:0,ready:true})))");
            }
        })
        .invoke_handler(tauri::generate_handler![check_git, open_repository, refresh_repository, get_commit, get_history, get_commit_files, get_file_diff, get_working_changes, get_working_diff, set_repository_trust, stage_file, unstage_file, prepare_commit, create_commit, set_commit_hook_trust, prepare_branch, execute_branch, set_branch_hook_trust, list_stashes, get_stash_details, prepare_stash, execute_stash, runtime_probe]);
    builder
        .run(tauri::generate_context!())
        .expect("failed to start Git-Poseidon");
}
