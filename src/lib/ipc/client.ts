import { invoke, isTauri } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import type {
  AppError,
  CommitPlanResponse,
  CreateCommitResponse,
  CommitFilesResponse,
  DetailResponse,
  FileDiffResponse,
  GitInstallation,
  IndexChangeResponse,
  HistoryResponse,
  SnapshotResponse,
  WorkingArea,
  WorkingChangesResponse,
} from './bindings';

export function desktopAvailable(): boolean {
  return isTauri();
}

export async function chooseRepository(): Promise<string | null> {
  const path = await open({
    directory: true,
    multiple: false,
    title: 'Abrir repositório Git local',
  });
  return typeof path === 'string' ? path : null;
}

export function checkGit(): Promise<GitInstallation> {
  return invoke('check_git');
}

export function openRepository(path: string): Promise<SnapshotResponse> {
  return invoke('open_repository', { path });
}

export function refreshRepository(
  sessionId: number,
): Promise<SnapshotResponse> {
  return invoke('refresh_repository', { sessionId });
}

export function getCommit(
  sessionId: number,
  generation: number,
  oid: string,
  offset = 0,
): Promise<DetailResponse> {
  return invoke('get_commit', { sessionId, generation, oid, offset });
}

export function getHistory(
  sessionId: number,
  generation: number,
  offset: number,
): Promise<HistoryResponse> {
  return invoke('get_history', { sessionId, generation, offset });
}

export function getCommitFiles(
  sessionId: number,
  generation: number,
  oid: string,
): Promise<CommitFilesResponse> {
  return invoke('get_commit_files', { sessionId, generation, oid });
}

/** Files are addressed by their index in `getCommitFiles`, never by path. */
export function getFileDiff(
  sessionId: number,
  generation: number,
  oid: string,
  index: number,
  offset: number,
  allowLarge: boolean,
): Promise<FileDiffResponse> {
  return invoke('get_file_diff', {
    sessionId,
    generation,
    oid,
    index,
    offset,
    allowLarge,
  });
}

export function getWorkingChanges(
  sessionId: number,
  generation: number,
): Promise<WorkingChangesResponse> {
  return invoke('get_working_changes', { sessionId, generation });
}

/** Local files are addressed by listing revision, group and index, never by path. */
export function getWorkingDiff(
  sessionId: number,
  generation: number,
  revision: number,
  area: WorkingArea,
  index: number,
  offset: number,
  allowLarge: boolean,
): Promise<FileDiffResponse> {
  return invoke('get_working_diff', {
    sessionId,
    generation,
    revision,
    area,
    index,
    offset,
    allowLarge,
  });
}

/** Only from the explicit trust dialog or the revoke action. */
export function setRepositoryTrust(
  sessionId: number,
  generation: number,
  trusted: boolean,
): Promise<void> {
  return invoke('set_repository_trust', { sessionId, generation, trusted });
}

/** Stages one listed file, addressed like `getWorkingDiff`. */
export function stageFile(
  sessionId: number,
  generation: number,
  revision: number,
  area: WorkingArea,
  index: number,
): Promise<IndexChangeResponse> {
  return invoke('stage_file', { sessionId, generation, revision, area, index });
}

export function unstageFile(
  sessionId: number,
  generation: number,
  revision: number,
  index: number,
): Promise<IndexChangeResponse> {
  return invoke('unstage_file', { sessionId, generation, revision, index });
}

export function readableError(error: unknown): AppError {
  if (
    typeof error === 'object' &&
    error !== null &&
    'message' in error &&
    typeof error.message === 'string' &&
    'code' in error
  ) {
    return error as AppError;
  }
  return {
    code: 'CommandFailed',
    message:
      'Não foi possível comunicar com o aplicativo desktop. Tente novamente.',
  };
}

export function prepareCommit(
  sessionId: number,
  generation: number,
  revision: number,
): Promise<CommitPlanResponse> {
  return invoke('prepare_commit', { sessionId, generation, revision });
}
export function createCommit(
  sessionId: number,
  generation: number,
  token: number,
  message: string,
): Promise<CreateCommitResponse> {
  return invoke('create_commit', { sessionId, generation, token, message });
}
export function setCommitHookTrust(
  sessionId: number,
  generation: number,
  trusted: boolean,
): Promise<void> {
  return invoke('set_commit_hook_trust', { sessionId, generation, trusted });
}

export function prepareBranch(
  sessionId: number,
  generation: number,
  action: import('./bindings').BranchAction,
): Promise<import('./bindings').BranchPlanResponse> {
  return invoke('prepare_branch', { sessionId, generation, action });
}
export function executeBranch(
  sessionId: number,
  generation: number,
  token: number,
): Promise<import('./bindings').BranchChangeResponse> {
  return invoke('execute_branch', { sessionId, generation, token });
}
export function setBranchHookTrust(
  sessionId: number,
  generation: number,
  trusted: boolean,
): Promise<void> {
  return invoke('set_branch_hook_trust', { sessionId, generation, trusted });
}

export function listStashes(
  sessionId: number,
  generation: number,
): Promise<import('./bindings').StashListResponse> {
  return invoke('list_stashes', { sessionId, generation });
}
export function getStashDetails(
  sessionId: number,
  generation: number,
  selection: import('./bindings').StashSelection,
): Promise<import('./bindings').StashDetailsResponse> {
  return invoke('get_stash_details', { sessionId, generation, selection });
}
export function prepareStash(
  sessionId: number,
  generation: number,
  action: import('./bindings').StashAction,
): Promise<import('./bindings').StashPlanResponse> {
  return invoke('prepare_stash', { sessionId, generation, action });
}
export function executeStash(
  sessionId: number,
  generation: number,
  token: number,
): Promise<import('./bindings').BranchChangeResponse> {
  return invoke('execute_stash', { sessionId, generation, token });
}
