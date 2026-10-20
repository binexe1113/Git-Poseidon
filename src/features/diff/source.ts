import * as api from '../../lib/ipc/client';
import type {
  DiffBase,
  DiffTarget,
  FileDiffResponse,
  WorkingArea,
} from '../../lib/ipc/bindings';

/** What the open diff compares; mirrors the backend `DiffTarget` plus labels. */
export type DiffSource =
  | { kind: 'commit'; oid: string; base: DiffBase }
  | {
      kind: 'working';
      revision: number;
      area: WorkingArea;
      headOid: string | null;
    };

export function loadDiff(
  source: DiffSource,
  sessionId: number,
  generation: number,
  index: number,
  offset: number,
  allowLarge: boolean,
): Promise<FileDiffResponse> {
  return source.kind === 'commit'
    ? api.getFileDiff(
        sessionId,
        generation,
        source.oid,
        index,
        offset,
        allowLarge,
      )
    : api.getWorkingDiff(
        sessionId,
        generation,
        source.revision,
        source.area,
        index,
        offset,
        allowLarge,
      );
}

export function sameTarget(source: DiffSource, target: DiffTarget): boolean {
  return source.kind === 'commit'
    ? target.kind === 'commit' && target.oid === source.oid
    : target.kind === 'working' &&
        target.revision === source.revision &&
        target.area === source.area;
}

export const areaTitle: Record<WorkingArea, string> = {
  staged: 'Preparadas',
  unstaged: 'Não preparadas',
  untracked: 'Não rastreadas',
};

/** Optional per-file index action (stage or unstage) shown in file lists. */
export type FileAction = {
  label: string;
  /** Keyboard shortcut on a focused row ("s" or "u"). */
  key: string;
  icon: 'plus' | 'minus';
  /** Index of the file whose action is running, if any. */
  busy: number | null;
  /** Why the action is unavailable for every file, if it is. */
  disabled: string | null;
  run: (index: number) => void;
};
