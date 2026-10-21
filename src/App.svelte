<script lang="ts">
  import StashPanel from './features/stash/StashPanel.svelte';
  import { onMount, tick } from 'svelte';
  import Icon from './lib/ui/Icon.svelte';
  import BranchDialog from './features/branches/BranchDialog.svelte';
  import BranchMenu from './features/branches/BranchMenu.svelte';
  import type { BranchAction, Branch, BranchChange } from './lib/ipc/bindings';
  import RepositoryPathDialog from './features/repository/RepositoryPathDialog.svelte';
  import HistoryPanel from './features/history/HistoryPanel.svelte';
  import CommitDetails from './features/history/CommitDetails.svelte';
  import CommitComposer from './features/changes/CommitComposer.svelte';
  import ChangesPanel from './features/changes/ChangesPanel.svelte';
  import IndexDialog from './features/changes/IndexDialog.svelte';
  import DiffViewer from './features/diff/DiffViewer.svelte';
  import { areaTitle, type DiffSource } from './features/diff/source';
  import * as api from './lib/ipc/client';
  import type {
    AppError,
    Commit,
    CommitCreation,
    CommitDetail,
    CommitFiles,
    CommitRef,
    GitInstallation,
    HistoryPage,
    SnapshotResponse,
    WorkingArea,
    WorkingChanges,
  } from './lib/ipc/bindings';

  let git = $state<GitInstallation | null>(null);
  let gitChecking = $state(true);
  let snapshot = $state.raw<SnapshotResponse | null>(null);
  let selected = $state.raw<Commit | null>(null);
  let selectedOffset = $state(0);
  let selectedRefs = $state.raw<CommitRef[]>([]);
  let detail = $state.raw<CommitDetail | null>(null);
  let detailError = $state<string | null>(null);
  let detailLoading = $state(false);
  let loading = $state(false);
  let dialogOpen = $state(false);
  let pathDialogOpen = $state(false);
  let historyCount = $state(0);
  let historyComplete = $state(false);
  let error = $state<AppError | null>(null);
  let repoEpoch = 0;
  let selectionEpoch = 0;
  let detailQueue: Promise<void> = Promise.resolve();
  const desktop = api.desktopAvailable();

  // Files of the selected commit and the open diff (file index, or null = history).
  let files = $state.raw<CommitFiles | null>(null);
  let filesLoading = $state(false);
  let filesError = $state<string | null>(null);
  let filesEpoch = 0;
  let diffIndex = $state<number | null>(null);
  // Last file viewed in this commit: stays highlighted and receives focus on Esc.
  let viewedIndex = $state<number | null>(null);
  const diffFile = $derived(
    diffIndex === null
      ? null
      : (files?.files.find((f) => f.index === diffIndex) ?? null),
  );
  const diffPosition = $derived(
    files && diffIndex !== null
      ? files.files.findIndex((f) => f.index === diffIndex)
      : -1,
  );

  async function loadFiles(commit: Commit, attempt = 0) {
    const context = snapshot;
    if (!context) return;
    const epoch = ++filesEpoch;
    if (attempt === 0) {
      files = null;
      filesError = null;
    }
    filesLoading = true;
    try {
      const response = await api.getCommitFiles(
        context.sessionId,
        context.generation,
        commit.oid,
      );
      if (
        epoch !== filesEpoch ||
        response.sessionId !== snapshot?.sessionId ||
        response.generation !== snapshot.generation ||
        response.data.oid !== commit.oid
      )
        return;
      files = response.data;
    } catch (cause) {
      if (epoch !== filesEpoch) return;
      const failure = api.readableError(cause);
      // The backend cancels a request superseded by a newer one; if this is
      // still the latest selection, ask again shortly.
      if (failure.code === 'Cancelled' && attempt < 3) {
        setTimeout(() => {
          if (epoch === filesEpoch) void loadFiles(commit, attempt + 1);
        }, 60);
        return;
      }
      filesError = failure.message;
    } finally {
      if (epoch === filesEpoch) filesLoading = false;
    }
  }
  function openDiff(index: number) {
    workingOpen = null;
    diffIndex = viewedIndex = index;
  }
  async function closeDiff() {
    const panel = workingOpen ? 'inspector-changes' : 'inspector-commit';
    diffIndex = null;
    workingOpen = null;
    await tick();
    document
      .querySelector<HTMLElement>(`#${panel} .file-item[aria-current="true"]`)
      ?.focus();
  }
  function stepDiff(delta: number) {
    if (workingOpen && working) {
      const list = working[workingOpen.area];
      const position = list.findIndex((f) => f.index === workingOpen!.index);
      const next = list[position + delta];
      if (next) openWorking(workingOpen.area, next.index);
      return;
    }
    if (!files || diffPosition < 0) return;
    const next = files.files[diffPosition + delta];
    if (next) diffIndex = viewedIndex = next.index;
  }

  // Local changes: listed on demand; files are addressed by listing revision.
  type WorkingFile = { area: WorkingArea; index: number };
  let working = $state.raw<WorkingChanges | null>(null);
  let workingLoading = $state(false);
  let workingError = $state<string | null>(null);
  let workingEpoch = 0;
  let workingOpen = $state.raw<WorkingFile | null>(null);
  let workingViewed = $state.raw<WorkingFile | null>(null);
  // Remounts the viewer when a relisting keeps the revision after a change error.
  let workingNonce = $state(0);
  // Consecutive "changed while reading" reloads for the open file; bounded.
  let changedStreak = 0;
  let workingNotice = $state('');

  type IndexIntent = WorkingFile & {
    sessionId: number;
    generation: number;
    revision: number;
    path: string;
  };
  let commitBusy = $state(false);
  let historyPanel = $state<{ revealSelected: () => Promise<void> }>();
  let indexBusy = $state.raw<WorkingFile | null>(null);
  let indexMessage = $state<{ kind: 'ok' | 'error'; text: string } | null>(
    null,
  );
  let moved = $state<{ area: WorkingArea; path: string } | null>(null);
  let indexDialog = $state.raw<{
    kind: 'trust' | 'partial';
    intent: IndexIntent;
  } | null>(null);
  let actionFocus: HTMLElement | null = null;

  function requestIndex(area: WorkingArea, index: number) {
    if (
      !snapshot ||
      !working ||
      loading ||
      workingLoading ||
      indexBusy ||
      commitBusy ||
      branchBusy ||
      !!branchIntent ||
      !!branchMenu ||
      indexDialog ||
      working.writeBlock
    )
      return;
    const file = working[area].find((f) => f.index === index);
    if (!file) return;
    actionFocus =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const intent: IndexIntent = {
      area,
      index,
      path: file.path,
      revision: working.revision,
      sessionId: snapshot.sessionId,
      generation: snapshot.generation,
    };
    if (!working.trusted) indexDialog = { kind: 'trust', intent };
    else if (
      area !== 'staged' &&
      working.staged.some((f) => f.path === file.path)
    )
      indexDialog = { kind: 'partial', intent };
    else void changeIndex(intent);
  }
  async function restoreActionFocus() {
    await tick();
    if (actionFocus?.isConnected) actionFocus.focus();
    else
      document
        .querySelector<HTMLElement>(
          '#inspector-changes .file-row.moved .file-item, #inspector-changes .file-item[aria-current="true"]',
        )
        ?.focus();
  }
  function cancelIndex() {
    indexDialog = null;
    void restoreActionFocus();
  }
  async function confirmIndex() {
    const pending = indexDialog;
    if (!pending) return;
    indexDialog = null;
    if (pending.kind === 'partial') await changeIndex(pending.intent);
    else await changeTrust(true);
  }
  async function changeTrust(trusted: boolean) {
    const context = snapshot;
    if (!context || loading || indexBusy || commitBusy) return;
    indexBusy = { area: 'staged', index: -1 };
    ++workingEpoch;
    workingLoading = false;
    try {
      await api.setRepositoryTrust(
        context.sessionId,
        context.generation,
        trusted,
      );
      indexMessage = {
        kind: 'ok',
        text: trusted
          ? 'Confiança concedida. Confira a lista atualizada e acione o arquivo novamente.'
          : 'Confiança revogada. Operações de index e filtros estão bloqueados.',
      };
    } catch (cause) {
      indexMessage = { kind: 'error', text: api.readableError(cause).message };
    } finally {
      indexBusy = null;
      await loadWorking();
      await restoreActionFocus();
    }
  }
  async function changeIndex(intent: IndexIntent) {
    if (indexBusy || commitBusy || loading) return;
    indexBusy = { area: intent.area, index: intent.index };
    indexMessage = null;
    moved = null;
    ++workingEpoch; // Ignore any read started before this write.
    workingLoading = false;
    const previous = working;
    const actedOn = (file: WorkingFile | null) =>
      file?.area === intent.area && file.index === intent.index;
    const followOpen = actedOn(workingOpen);
    const followViewed = actedOn(workingViewed);
    try {
      const response =
        intent.area === 'staged'
          ? await api.unstageFile(
              intent.sessionId,
              intent.generation,
              intent.revision,
              intent.index,
            )
          : await api.stageFile(
              intent.sessionId,
              intent.generation,
              intent.revision,
              intent.area,
              intent.index,
            );
      if (
        response.schemaVersion !== 2 ||
        response.sessionId !== snapshot?.sessionId ||
        response.generation !== snapshot.generation
      )
        throw new Error('Snapshot changed');
      working = response.data.working;
      workingError = null;
      workingNotice = '';
      if (previous) followFiles(previous, working);
      const areas: WorkingArea[] =
        intent.area === 'staged'
          ? ['unstaged', 'untracked', 'staged']
          : ['staged', 'unstaged', 'untracked'];
      for (const area of areas) {
        const file = working[area].find((f) => f.path === response.data.path);
        if (!file) continue;
        moved = { area, path: file.path };
        if (followOpen) workingOpen = { area, index: file.index };
        if (followViewed || !workingViewed)
          workingViewed = { area, index: file.index };
        break;
      }
      workingNotice = '';
      indexMessage = {
        kind: 'ok',
        text: `${intent.path}: ${intent.area === 'staged' ? 'retirado do index' : 'preparado'}. Working tree preservado.`,
      };
    } catch (cause) {
      const failure = api.readableError(cause);
      indexMessage = {
        kind: 'error',
        text: failure.message + ' Nenhuma tentativa automática será feita.',
      };
      // Always reconcile after a failed write, including an unknown outcome.
      indexBusy = null;
      await loadWorking();
    } finally {
      indexBusy = null;
      await restoreActionFocus();
    }
  }

  async function committed(result: CommitCreation) {
    if (result.snapshot)
      await reconcileSnapshot(
        result.snapshot,
        result.outcome === 'created' ? result.oid : (selected?.oid ?? null),
        result.outcome === 'created',
      );
  }
  async function reconcileSnapshot(
    response: SnapshotResponse,
    targetOid: string | null,
    locate: boolean,
  ) {
    {
      ++repoEpoch;
      ++selectionEpoch;
      ++workingEpoch;
      ++filesEpoch;
      snapshot = response;
      historyCount = response.data.commits.length;
      historyComplete = !response.data.hasMoreCommits;
      working = null;
      workingError = null;
      workingLoading = false;
      workingOpen = workingViewed = null;
      diffIndex = viewedIndex = null;
      files = null;
      indexMessage = null;
      moved = null;
      workingNotice = '';
      selected = null;
      selectedRefs = [];
      detail = null;
      const epoch = repoEpoch;
      let commits = response.data.commits;
      let graph = response.data.graph;
      let offset = 0;
      let nextOffset: number | null = response.data.hasMoreCommits
        ? commits.length
        : null;
      let index = commits.findIndex((c) => c.oid === targetOid);
      try {
        while (
          locate &&
          index < 0 &&
          nextOffset !== null &&
          epoch === repoEpoch
        ) {
          workingNotice = 'Localizando o HEAD no histórico…';
          const page = await fetchHistory(nextOffset);
          commits = page.commits;
          graph = page.graph;
          offset = page.offset;
          nextOffset = page.nextOffset;
          index = commits.findIndex((c) => c.oid === targetOid);
        }
        if (epoch !== repoEpoch) return;
        if (index >= 0) {
          selectCommit(commits[index], offset, graph[index]?.refs ?? []);
          workingNotice = '';
          if (offset > 0) {
            await tick();
            await historyPanel?.revealSelected();
          }
        } else if (locate)
          workingNotice =
            'O nó não está nas refs atuais do histórico. O repositório mudou novamente; confira o HEAD antes de repetir qualquer operação.';
      } catch (cause) {
        workingNotice =
          'Não foi possível localizar o nó: ' +
          api.readableError(cause).message;
      }
    }
  }
  function setCommitBusy(value: boolean) {
    commitBusy = value;
    if (!value) void loadWorking();
  }

  async function loadWorking(attempt = 0) {
    const context = snapshot;
    if (
      !context ||
      !context.data.status.available ||
      indexBusy ||
      commitBusy ||
      branchBusy ||
      !!branchIntent ||
      !!branchMenu ||
      indexDialog
    )
      return;
    const epoch = ++workingEpoch;
    workingLoading = true;
    try {
      const response = await api.getWorkingChanges(
        context.sessionId,
        context.generation,
      );
      if (
        epoch !== workingEpoch ||
        response.sessionId !== snapshot?.sessionId ||
        response.generation !== snapshot.generation
      )
        return;
      const previous = working;
      working = response.data;
      workingError = null;
      if (previous && previous.revision !== response.data.revision)
        followFiles(previous, response.data);
      else if (previous && workingOpen) workingNonce++;
    } catch (cause) {
      if (epoch !== workingEpoch) return;
      const failure = api.readableError(cause);
      if (failure.code === 'Cancelled' && attempt < 3) {
        setTimeout(() => {
          if (epoch === workingEpoch) void loadWorking(attempt + 1);
        }, 60);
        return;
      }
      workingError = failure.message;
    } finally {
      if (epoch === workingEpoch) workingLoading = false;
    }
  }
  // A new listing renumbers files: keep the open/viewed file by group and path.
  function followFiles(previous: WorkingChanges, next: WorkingChanges) {
    const follow = (file: WorkingFile | null) => {
      if (!file) return null;
      const old = previous[file.area].find((f) => f.index === file.index);
      const match =
        old &&
        next[file.area].find(
          (f) => f.path === old.path && f.oldPath === old.oldPath,
        );
      return match ? { area: file.area, index: match.index } : null;
    };
    const open = workingOpen;
    workingViewed = follow(workingViewed);
    workingOpen = follow(open);
    if (open && !workingOpen) {
      const old = previous[open.area].find((f) => f.index === open.index);
      workingNotice = `${old?.path ?? 'O arquivo'} não está mais em “${areaTitle[open.area]}”. Diff fechado.`;
    }
  }
  function openWorking(area: WorkingArea, index: number) {
    diffIndex = null;
    changedStreak = 0;
    workingNotice = '';
    workingOpen = workingViewed = { area, index };
  }
  // The viewer reports an outdated listing; reload unless the file keeps changing.
  function workingChanged() {
    if (++changedStreak > 3) return false;
    void loadWorking();
    return true;
  }
  function showChanges() {
    inspectorTab = 'changes';
    void loadWorking();
  }

  // One open diff at a time: a local file or a file of the selected commit.
  const viewer = $derived.by(() => {
    if (!snapshot) return null;
    const scope = `${snapshot.sessionId}:${snapshot.generation}`;
    if (workingOpen && working) {
      const list = working[workingOpen.area];
      const position = list.findIndex((f) => f.index === workingOpen!.index);
      if (position < 0) return null;
      return {
        key: `w:${scope}:${working.revision}:${workingOpen.area}:${workingOpen.index}:${workingNonce}`,
        source: {
          kind: 'working',
          revision: working.revision,
          area: workingOpen.area,
          headOid: working.headOid,
        } satisfies DiffSource,
        file: list[position],
        position,
        count: list.length,
      };
    }
    if (diffFile && files)
      return {
        key: `c:${scope}:${files.oid}:${diffFile.index}`,
        source: {
          kind: 'commit',
          oid: files.oid,
          base: files.base,
        } satisfies DiffSource,
        file: diffFile,
        position: diffPosition,
        count: files.files.length,
      };
    return null;
  });

  // Inspector geometry is a per-viewer preference; storage may be unavailable.
  const INSPECTOR_MIN = 300,
    INSPECTOR_MAX = 560,
    INSPECTOR_DEFAULT = 360,
    HISTORY_MIN = 460;
  function readPreference(key: string) {
    try {
      return localStorage.getItem(`poseidon.inspector.${key}`);
    } catch {
      return null;
    }
  }
  function savePreference(key: string, value: string) {
    try {
      localStorage.setItem(`poseidon.inspector.${key}`, value);
    } catch {
      // Preference only; the layout still works without persistence.
    }
  }
  let inspectorTab = $state<'commit' | 'changes'>('commit');
  let inspectorOpen = $state(readPreference('open') !== 'false');
  let inspectorPreference = $state(
    Number(readPreference('width')) || INSPECTOR_DEFAULT,
  );
  let workspaceWidth = $state(0);
  const inspectorMax = $derived(
    Math.max(
      INSPECTOR_MIN,
      Math.min(INSPECTOR_MAX, workspaceWidth - HISTORY_MIN - 8),
    ),
  );
  const inspectorWidth = $derived(
    Math.round(
      Math.min(inspectorMax, Math.max(INSPECTOR_MIN, inspectorPreference)),
    ),
  );
  function setInspectorWidth(value: number) {
    inspectorPreference = Math.min(
      inspectorMax,
      Math.max(INSPECTOR_MIN, value),
    );
  }
  function toggleInspector(tab?: 'commit' | 'changes') {
    if (tab && (!inspectorOpen || inspectorTab !== tab)) {
      inspectorTab = tab;
      inspectorOpen = true;
    } else inspectorOpen = !inspectorOpen;
    savePreference('open', String(inspectorOpen));
    if (inspectorOpen && inspectorTab === 'changes') void loadWorking();
  }
  function startResize(event: PointerEvent) {
    if (event.button !== 0) return;
    const handle = event.currentTarget as HTMLElement;
    const startX = event.clientX,
      startWidth = inspectorWidth;
    handle.setPointerCapture(event.pointerId);
    const move = (e: PointerEvent) =>
      setInspectorWidth(startWidth - (e.clientX - startX));
    const done = () => {
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', done);
      handle.removeEventListener('pointercancel', done);
      savePreference('width', String(inspectorWidth));
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', done);
    handle.addEventListener('pointercancel', done);
  }
  function resizeKey(event: KeyboardEvent) {
    const next =
      event.key === 'ArrowLeft'
        ? inspectorWidth + 16
        : event.key === 'ArrowRight'
          ? inspectorWidth - 16
          : event.key === 'Home'
            ? INSPECTOR_MAX
            : event.key === 'End'
              ? INSPECTOR_MIN
              : event.key === 'Enter'
                ? INSPECTOR_DEFAULT
                : null;
    if (next === null) return;
    event.preventDefault();
    setInspectorWidth(next);
    savePreference('width', String(inspectorWidth));
  }

  const repository = $derived(snapshot?.data ?? null);
  const localCount = $derived(
    working
      ? `${working.hasMoreFiles ? '≥' : ''}${new Set([...working.staged, ...working.unstaged, ...working.untracked].map((f) => f.path)).size}`
      : String(repository?.status.totalFiles ?? 0),
  );
  const headLabel = $derived(
    repository?.head.kind === 'detached'
      ? 'HEAD destacado'
      : (repository?.head.name ?? 'Sem repositório'),
  );
  const localBranches = $derived(
    repository?.branches.filter((b) => !b.remote) ?? [],
  );
  const remoteBranches = $derived(
    repository?.branches.filter((b) => b.remote) ?? [],
  );
  let branchIntent = $state.raw<BranchAction | null>(null);
  let branchBusy = $state(false);
  let branchMenu = $state.raw<{
    x: number;
    y: number;
    oid: string | null;
    branches: Branch[];
  } | null>(null);
  let branchFocus: HTMLElement | null = null;
  function closeBranch() {
    branchIntent = null;
    branchMenu = null;
    if (inspectorTab === 'changes') void loadWorking();
    void tick().then(() => {
      if (branchFocus?.isConnected) branchFocus.focus();
      else
        document
          .querySelector<HTMLButtonElement>('[aria-label="Criar branch"]')
          ?.focus();
    });
  }
  function branchAction(action: BranchAction) {
    branchMenu = null;
    branchIntent = action;
  }
  function branchContext(
    event: MouseEvent | KeyboardEvent,
    oid: string | null,
    branches: Branch[],
  ) {
    if (
      loading ||
      indexBusy ||
      commitBusy ||
      branchBusy ||
      indexDialog ||
      branchIntent
    )
      return;
    event.preventDefault();
    branchFocus = event.currentTarget as HTMLElement;
    const r = branchFocus.getBoundingClientRect();
    branchMenu = {
      x:
        event instanceof MouseEvent && (event.clientX || event.clientY)
          ? event.clientX
          : r.left,
      y:
        event instanceof MouseEvent && (event.clientX || event.clientY)
          ? event.clientY
          : r.bottom,
      oid,
      branches,
    };
  }
  const branchItems = $derived.by(() => {
    const menu = branchMenu;
    if (!menu) return [];
    const items: { label: string; disabled?: boolean; pick: () => void }[] = [];
    if (menu.oid)
      items.push({
        label: 'Criar branch neste commit…',
        pick: () =>
          branchAction({ kind: 'create', name: '', startOid: menu.oid! }),
      });
    for (const branch of menu.branches) {
      items.push({
        label: 'Trocar para ' + branch.name,
        disabled: branch.current,
        pick: () =>
          branchAction({
            kind: 'switch',
            name: branch.name,
            expectedOid: branch.oid,
          }),
      });
      items.push({
        label: 'Renomear ' + branch.name + '…',
        pick: () =>
          branchAction({
            kind: 'rename',
            name: branch.name,
            expectedOid: branch.oid,
            newName: branch.name,
          }),
      });
      items.push({
        label: 'Excluir ' + branch.name + '…',
        disabled: branch.current,
        pick: () =>
          branchAction({
            kind: 'delete',
            name: branch.name,
            expectedOid: branch.oid,
          }),
      });
    }
    return items;
  });
  async function branchChanged(result: BranchChange) {
    if (result.snapshot) {
      const h = result.snapshot.data.head;
      await reconcileSnapshot(
        result.snapshot,
        h.kind === 'unborn' ? null : h.oid,
        true,
      );
    }
  }
  async function verifyGit() {
    gitChecking = true;
    error = null;
    try {
      git = await api.checkGit();
    } catch (cause) {
      git = null;
      error = api.readableError(cause);
    } finally {
      gitChecking = false;
    }
  }

  onMount(() => {
    if (desktop) void verifyGit();
    else gitChecking = false;
    // No watcher in this phase: returning to the window re-reads local changes
    // while they are on screen.
    const focus = () => {
      if (workingOpen || (inspectorOpen && inspectorTab === 'changes'))
        void loadWorking();
    };
    window.addEventListener('focus', focus);
    return () => window.removeEventListener('focus', focus);
  });

  async function open() {
    if (
      loading ||
      dialogOpen ||
      pathDialogOpen ||
      indexBusy ||
      commitBusy ||
      branchBusy ||
      !!branchIntent ||
      !!branchMenu ||
      indexDialog ||
      !git
    )
      return;
    dialogOpen = true;
    try {
      const path = await api.chooseRepository();
      if (path) await load(path);
    } catch (cause) {
      error = api.readableError(cause);
    } finally {
      dialogOpen = false;
    }
  }

  async function load(path?: string) {
    if (
      indexBusy ||
      commitBusy ||
      indexDialog ||
      branchBusy ||
      branchIntent ||
      branchMenu
    )
      return;
    const epoch = ++repoEpoch;
    ++selectionEpoch;
    loading = true;
    detailLoading = false;
    error = null;
    await detailQueue;
    try {
      const response = path
        ? await api.openRepository(path)
        : await api.refreshRepository(snapshot!.sessionId);
      if (epoch !== repoEpoch) return;
      if (response.schemaVersion !== 2) throw new Error('Unsupported schema');
      const previous = selected?.oid;
      snapshot = response;
      historyCount = response.data.commits.length;
      historyComplete = !response.data.hasMoreCommits;
      selected = null;
      selectedRefs = [];
      diffIndex = null;
      files = null;
      // Listings belong to the previous generation.
      ++workingEpoch;
      working = null;
      workingError = null;
      workingLoading = false;
      workingOpen = workingViewed = null;
      indexMessage = moved = indexDialog = null;
      if (inspectorOpen && inspectorTab === 'changes') void loadWorking();
      detail = null;
      detailError = null;
      const index = Math.max(
        0,
        response.data.commits.findIndex((c) => c.oid === previous),
      );
      const next = response.data.commits[index];
      if (next) selectCommit(next, 0, response.data.graph[index]?.refs ?? []);
    } catch (cause) {
      if (epoch === repoEpoch) error = api.readableError(cause);
    } finally {
      if (epoch === repoEpoch) loading = false;
    }
  }

  function fetchHistory(offset: number): Promise<HistoryPage> {
    const context = snapshot!;
    const work = detailQueue.then(async () => {
      const response = await api.getHistory(
        context.sessionId,
        context.generation,
        offset,
      );
      if (
        response.schemaVersion !== 2 ||
        response.sessionId !== snapshot?.sessionId ||
        response.generation !== snapshot?.generation
      ) {
        throw new Error('Snapshot changed');
      }
      historyCount = Math.max(
        historyCount,
        response.data.offset + response.data.commits.length,
      );
      if (response.data.complete) historyComplete = true;
      return response.data;
    });
    detailQueue = work.then(
      () => {},
      () => {},
    );
    return work;
  }

  function selectCommit(commit: Commit, offset = 0, refs: CommitRef[] = []) {
    const context = snapshot;
    if (!context) return;
    const epoch = ++selectionEpoch;
    selected = commit;
    selectedOffset = offset;
    selectedRefs = refs;
    diffIndex = viewedIndex = null;
    void loadFiles(commit);
    detail = null;
    detailError = null;
    detailLoading = true;
    detailQueue = detailQueue.then(async () => {
      if (epoch !== selectionEpoch) return;
      try {
        const response = await api.getCommit(
          context.sessionId,
          context.generation,
          commit.oid,
          offset,
        );
        if (
          epoch === selectionEpoch &&
          response.sessionId === snapshot?.sessionId &&
          response.generation === snapshot.generation
        )
          detail = response.data;
      } catch (cause) {
        if (epoch === selectionEpoch)
          detailError = api.readableError(cause).message;
      } finally {
        if (epoch === selectionEpoch) detailLoading = false;
      }
    });
  }
</script>

<svelte:window
  onkeydown={(event) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'o') {
      event.preventDefault();
      void open();
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r') {
      event.preventDefault();
      if (snapshot && !loading && !dialogOpen) void load();
    }
  }}
/>

<div class="app-shell">
  <aside class="sidebar" aria-label="Repositórios e branches">
    <div class="brand">
      <div class="brand-mark" aria-hidden="true">
        <img src="/poseidon.svg" alt="" width="44" height="44" />
      </div>
      <div>
        <span class="brand-name"
          >Git<span class="brand-dot">Poseidon</span></span
        ><span class="brand-caption">GIT SOB SEU COMANDO</span>
      </div>
    </div>

    <div class="sidebar-section">
      <div class="section-label">
        REPOSITÓRIOS <span class="count">{repository ? '1' : '0'}</span>
      </div>
      {#if repository}
        <div class="repository-card" aria-current="true">
          <div class="repo-icon"><Icon name="folder" size={20} /></div>
          <div class="truncate">
            <strong>{repository.name}</strong><span title={repository.path}
              >{repository.path}</span
            >
          </div>
          <span class="active-dot"></span>
        </div>
      {:else}<p class="sidebar-hint">
          Seu próximo projeto<br />começa por aqui.
        </p>{/if}
      <button
        class="add-repo"
        title="Abrir repositório (Ctrl/Cmd+O)"
        onclick={open}
        disabled={!desktop ||
          !git ||
          loading ||
          dialogOpen ||
          !!indexBusy ||
          commitBusy ||
          branchBusy ||
          !!branchIntent ||
          !!branchMenu ||
          !!indexDialog}
        ><span>+</span> Abrir repositório <kbd>⌘ / Ctrl O</kbd></button
      >
      <button
        class="toolbar-button"
        onclick={() => (pathDialogOpen = true)}
        disabled={!desktop ||
          !git ||
          loading ||
          dialogOpen ||
          !!indexBusy ||
          commitBusy ||
          branchBusy ||
          !!branchIntent ||
          !!branchMenu ||
          !!indexDialog}>Abrir por caminho…</button
      >
    </div>

    <div class="sidebar-rule"></div>
    {#if snapshot}
      {#key snapshot.sessionId}
        <StashPanel
          sessionId={snapshot.sessionId}
          generation={snapshot.generation}
          disabled={loading ||
            branchBusy ||
            commitBusy ||
            !!indexBusy ||
            !!branchIntent ||
            !!indexDialog}
        />
      {/key}
    {/if}
    <div class="sidebar-section branches-section">
      <div class="section-label">
        <span class="label-with-icon"
          ><Icon name="branch" size={14} /> BRANCHES LOCAIS</span
        ><span class="count">{localBranches.length}</span>
        <button
          class="icon-button"
          aria-label="Criar branch"
          title="Criar branch a partir do HEAD"
          disabled={!repository ||
            repository.head.kind === 'unborn' ||
            loading ||
            branchBusy ||
            commitBusy ||
            !!indexBusy}
          onclick={(event) => {
            if (repository && repository.head.kind !== 'unborn') {
              branchFocus = event.currentTarget;
              branchAction({
                kind: 'create',
                name: '',
                startOid: repository.head.oid,
              });
            }
          }}><Icon name="plus" size={14} /></button
        >
      </div>
      {#if repository?.head.kind === 'unborn'}<div class="branch-item">
          <Icon name="branch" size={15} /><span class="truncate"
            >{repository.head.name}</span
          ><span class="small-badge">nova</span>
        </div>{/if}
      {#each localBranches as branch (branch.fullRef)}
        <button
          type="button"
          class:current={branch.current}
          class="branch-item branch-button"
          aria-label={`Branch ${branch.name}${branch.current ? ', atual' : ''}`}
          aria-current={branch.current ? 'true' : undefined}
          disabled={loading || branchBusy || commitBusy || !!indexBusy}
          onclick={(event) => branchContext(event, branch.oid, [branch])}
          oncontextmenu={(event) => branchContext(event, branch.oid, [branch])}
          onkeydown={(event) => {
            if (
              event.key === 'ContextMenu' ||
              (event.shiftKey && event.key === 'F10')
            )
              branchContext(event, branch.oid, [branch]);
          }}
          title={`${branch.fullRef}\n${branch.oid}${branch.upstream ? `\nUpstream: ${branch.upstream}` : ''}`}
        >
          <Icon name="branch" size={15} /><span class="truncate"
            >{branch.name}</span
          >{#if branch.upstream}<span
              class="upstream-indicator"
              title={`Upstream: ${branch.upstream}`}
              aria-label={`Upstream: ${branch.upstream}`}>↑</span
            >{/if}{#if branch.current}<span class="branch-current-dot"
            ></span>{/if}
        </button>
      {:else}{#if repository?.head.kind !== 'unborn'}<p
            class="muted sidebar-empty"
          >
            {repository
              ? 'Nenhuma branch local.'
              : 'Abra um repositório para explorar.'}
          </p>{/if}{/each}
      <div class="section-label remote-label">
        <span class="label-with-icon"
          ><Icon name="cloud" size={14} /> BRANCHES REMOTAS</span
        ><span class="count">{remoteBranches.length}</span>
      </div>
      {#each remoteBranches as branch (branch.fullRef)}<div
          class="branch-item remote"
          title={`${branch.fullRef}\n${branch.oid}${branch.symbolicTarget ? `\n→ ${branch.symbolicTarget}` : ''}`}
        >
          <Icon name="branch" size={15} /><span class="truncate"
            >{branch.name}</span
          >{#if branch.symbolicTarget}<Icon name="arrow" size={12} />{/if}
        </div>
      {:else}<p class="muted sidebar-empty">
          {repository
            ? 'Nenhuma referência remota local.'
            : 'As referências aparecerão aqui.'}
        </p>{/each}
      {#if repository?.hasMoreBranches}<p class="limit-note">
          Exibindo as primeiras 5.000 branches.
        </p>{/if}
    </div>
    <div class="sidebar-bottom">
      <Icon name="shield" size={17} />
      <div>
        <strong>Controle local do index</strong><span
          >Confiança explícita e revogável</span
        >
      </div>
    </div>
  </aside>

  <div class="workspace">
    <header class="topbar">
      <div class="breadcrumbs">
        <Icon name="folder" size={15} /><span class="breadcrumb-root"
          >Repositórios</span
        ><span class="breadcrumb-separator">/</span>
        {#if repository}<h1 title={repository.path}>{repository.name}</h1>
          <span
            class="head-chip"
            class:detached={repository.head.kind !== 'attached'}
            title={repository.head.kind === 'unborn'
              ? 'Branch ainda sem commits'
              : `HEAD · ${repository.head.oid}`}
            ><Icon name="branch" size={13} /><span>{headLabel}</span
            >{#if repository.head.kind === 'unborn'}<small>sem commits</small
              >{/if}</span
          >{:else}<strong>Visão geral</strong>{/if}
      </div>
      {#if repository}<div
          class="repo-stats"
          aria-label="Resumo do repositório"
        >
          <span title="Commits carregados no histórico"
            ><Icon name="commit" size={14} /><b
              >{historyCount}{historyComplete ? '' : '+'}</b
            > commits</span
          ><span title="Branches locais"
            ><Icon name="branch" size={14} /><b>{localBranches.length}</b> branches</span
          ><button
            class="stat-button"
            class:dirty={localCount !== '0'}
            title="Ver alterações locais da working tree"
            aria-pressed={inspectorOpen && inspectorTab === 'changes'}
            onclick={() => toggleInspector('changes')}
            ><Icon name="file" size={14} /><b>{localCount}</b> alterações locais</button
          >
        </div>{/if}
      <div class="header-actions">
        {#if repository}<button
            class="icon-button"
            title={inspectorOpen
              ? 'Recolher painel de detalhes'
              : 'Mostrar painel de detalhes'}
            aria-label="Painel de detalhes"
            aria-pressed={inspectorOpen}
            aria-controls="inspector"
            onclick={() => toggleInspector()}
            ><Icon name="panel" size={16} /></button
          >{/if}<button
          class="icon-button"
          title="Atualizar repositório (Ctrl/Cmd+R)"
          aria-label="Atualizar repositório"
          disabled={!snapshot ||
            loading ||
            dialogOpen ||
            !!indexBusy ||
            commitBusy ||
            branchBusy ||
            !!branchIntent ||
            !!branchMenu ||
            !!indexDialog}
          onclick={() => load()}
          ><span class:spinning={loading}
            ><Icon name="refresh" size={16} /></span
          ></button
        >
      </div>
    </header>

    {#if !desktop}<div class="notice" role="status">
        <Icon name="terminal" />Esta interface precisa do aplicativo desktop.
        Execute <code>npm run desktop</code> para acessar o Git real.
      </div>{/if}
    {#if error}<div class="error-banner" role="alert">
        <Icon name="alert" />
        <div>
          <strong>Não foi possível concluir a leitura</strong>
          <p>{error.message}</p>
        </div>
        {#if !git && desktop}<button
            class="text-button"
            onclick={verifyGit}
            disabled={gitChecking}>Verificar Git</button
          >{/if}<button
          class="icon-button"
          aria-label="Fechar mensagem de erro"
          onclick={() => (error = null)}><Icon name="close" /></button
        >
      </div>{/if}

    <main aria-busy={loading} class:repo-open={!!repository}>
      {#if repository}
        <div
          class="repo-workspace"
          class:inspector-closed={!inspectorOpen}
          style={`--inspector-width:${inspectorWidth}px`}
          bind:clientWidth={workspaceWidth}
        >
          <div class="center-pane">
            <!-- The graph stays mounted (scroll, pages) under an open diff. -->
            <div class="history-host" inert={viewer !== null}>
              {#key `${snapshot?.sessionId}:${snapshot?.generation}`}
                <HistoryPanel
                  bind:this={historyPanel}
                  allCommits={repository.commits}
                  graph={repository.graph}
                  fetchpage={fetchHistory}
                  onrefresh={() => load()}
                  hasMoreCommits={repository.hasMoreCommits}
                  selectedOid={selected?.oid ?? null}
                  {selectedOffset}
                  {loading}
                  onselect={selectCommit}
                  oncontext={(event, commit) =>
                    branchContext(
                      event,
                      commit.oid,
                      localBranches.filter((b) => b.oid === commit.oid),
                    )}
                />
              {/key}
            </div>
            {#if viewer && snapshot}
              {#key viewer.key}
                <DiffViewer
                  sessionId={snapshot.sessionId}
                  generation={snapshot.generation}
                  source={viewer.source}
                  file={viewer.file}
                  position={viewer.position}
                  count={viewer.count}
                  onclose={closeDiff}
                  onstep={stepDiff}
                  onchanged={viewer.source.kind === 'working'
                    ? workingChanged
                    : undefined}
                />
              {/key}
            {/if}
          </div>
          <!-- A focusable separator is the WAI-ARIA window-splitter widget. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
          {#if inspectorOpen}<div
              class="pane-resizer"
              role="separator"
              tabindex="0"
              aria-orientation="vertical"
              aria-label="Largura do painel de detalhes"
              aria-controls="inspector"
              aria-valuemin={INSPECTOR_MIN}
              aria-valuemax={inspectorMax}
              aria-valuenow={inspectorWidth}
              title="Arraste ou use ←/→ para redimensionar; Enter restaura"
              onpointerdown={startResize}
              onkeydown={resizeKey}
              ondblclick={() => {
                setInspectorWidth(INSPECTOR_DEFAULT);
                savePreference('width', String(inspectorWidth));
              }}
            ></div>{/if}
          <aside
            id="inspector"
            class="panel inspector"
            hidden={!inspectorOpen}
            aria-label="Painel de detalhes"
          >
            <div class="inspector-tabs" role="tablist">
              <button
                id="tab-commit"
                role="tab"
                aria-selected={inspectorTab === 'commit'}
                aria-controls="inspector-commit"
                onclick={() => (inspectorTab = 'commit')}
                ><Icon name="commit" size={15} />Commit</button
              ><button
                id="tab-changes"
                role="tab"
                aria-selected={inspectorTab === 'changes'}
                aria-controls="inspector-changes"
                onclick={showChanges}
                ><Icon name="file" size={15} />Alterações locais<span
                  class="small-badge">{localCount}</span
                ></button
              >
            </div>
            <div
              id="inspector-commit"
              class="inspector-body"
              role="tabpanel"
              aria-labelledby="tab-commit"
              hidden={inspectorTab !== 'commit'}
            >
              <CommitDetails
                {selected}
                refs={selectedRefs}
                {detail}
                loading={detailLoading}
                error={detailError}
                onretry={() => {
                  if (selected)
                    selectCommit(selected, selectedOffset, selectedRefs);
                }}
                {files}
                {filesLoading}
                {filesError}
                selectedFile={viewedIndex}
                onopenfile={openDiff}
                onretryfiles={() => {
                  if (selected) void loadFiles(selected);
                }}
              />
            </div>
            <div
              id="inspector-changes"
              class="inspector-body"
              role="tabpanel"
              aria-labelledby="tab-changes"
              hidden={inspectorTab !== 'changes'}
            >
              {#if snapshot && repository.status.available}
                {#key snapshot.sessionId}<CommitComposer
                    sessionId={snapshot.sessionId}
                    generation={snapshot.generation}
                    {working}
                    busy={!!indexBusy ||
                      commitBusy ||
                      branchBusy ||
                      !!branchIntent ||
                      !!branchMenu ||
                      loading ||
                      workingLoading}
                    onbusy={setCommitBusy}
                    onresult={committed}
                  />{/key}
              {/if}
              <ChangesPanel
                status={repository.status}
                {working}
                loading={workingLoading}
                error={workingError}
                selected={workingViewed}
                busy={indexBusy ??
                  (commitBusy ? { area: 'staged', index: -1 } : null)}
                message={indexMessage}
                {moved}
                onindex={requestIndex}
                onrevoke={() => void changeTrust(false)}
                onopen={openWorking}
                onretry={() => void loadWorking()}
              />
              {#if workingNotice}<p class="limit-note" role="status">
                  {workingNotice}
                </p>{/if}
            </div>
          </aside>
        </div>
      {:else}
        <section class="workspace-heading">
          <div>
            <div class="eyebrow">ESPAÇO DE TRABALHO</div>
            <h1>Explore seu próximo horizonte</h1>
            <p>
              História, branches e alterações. Tudo começa no seu repositório.
            </p>
          </div>
          <span class="local-label"
            ><span class="active-dot"></span> LOCAL POR NATUREZA</span
          >
        </section>
        <section class="welcome-panel">
          <div class="welcome-art" aria-hidden="true">
            <div class="orbit orbit-one"></div>
            <div class="orbit orbit-two"></div>
            <div class="orbit orbit-three"></div>
            <div class="welcome-symbol">
              <img src="/poseidon.svg" alt="" width="132" height="132" />
            </div>
            <span class="orbit-dot dot-one"></span><span
              class="orbit-dot dot-two"
            ></span>
          </div>
          <div class="welcome-content">
            <span class="welcome-kicker">ASSUMA O COMANDO DO SEU CÓDIGO</span>
            <h2>Um oceano de branches.<br />Uma visão clara.</h2>
            <p>
              Abra um projeto local para explorar o histórico, conhecer suas
              branches e acompanhar o que está mudando.
            </p>
            <button
              class="primary-button"
              onclick={open}
              disabled={!desktop ||
                !git ||
                loading ||
                dialogOpen ||
                !!indexBusy ||
                commitBusy ||
                branchBusy ||
                !!branchIntent ||
                !!branchMenu ||
                !!indexDialog}
              ><Icon name="folder" size={18} />{loading
                ? 'Lendo repositório…'
                : dialogOpen
                  ? 'Selecionando pasta…'
                  : 'Abrir repositório local'}<Icon
                name="arrow"
                size={17}
              /></button
            ><span class="welcome-footnote"
              ><Icon name="shield" size={13} /> Local. Open source. Sob seu controle.</span
            >
          </div>
        </section>
        <div class="welcome-features">
          <div>
            <span class="feature-number">01</span><Icon
              name="history"
              size={23}
            />
            <h3>Encontre o contexto</h3>
            <p>
              Commits reais, autores e mensagens em uma visão direta do seu
              projeto.
            </p>
          </div>
          <div>
            <span class="feature-number">02</span><Icon
              name="branch"
              size={23}
            />
            <h3>Explore as direções</h3>
            <p>
              Branches locais e referências remotas, com o HEAD sempre à vista.
            </p>
          </div>
          <div>
            <span class="feature-number">03</span><Icon name="file" size={23} />
            <h3>Entenda o presente</h3>
            <p>
              Veja o que está preparado, modificado ou ainda não foi rastreado.
            </p>
          </div>
        </div>
      {/if}
    </main>
    <footer class="statusbar">
      <div>
        <span class="status-indicator" class:offline={!git}></span><span
          title={git?.executable}
          >{gitChecking
            ? 'Verificando Git…'
            : (git?.version ?? 'Git indisponível')}</span
        >{#if repository}<span class="status-divider"></span><Icon
            name="branch"
            size={13}
          /><span>{headLabel}</span><span class="status-divider"></span><span
            title={repository.path}
            >{repository.bare
              ? 'Bare'
              : repository.shallow
                ? 'Histórico raso'
                : 'Repositório local'} · {repository.objectFormat.toUpperCase()}</span
          >{/if}
      </div>
      <div aria-live="polite">
        {#if loading}<span class="spinning"
            ><Icon name="refresh" size={13} /></span
          ><span>Lendo repositório…</span>{:else if repository}<span
            >{repository.operation}</span
          ><span class="status-divider"></span><span
            title="Tempo de leitura do Git e montagem do snapshot no Rust; não inclui a pintura da interface"
            >Leitura {repository.readElapsedMs} ms</span
          >{:else}<span>Pronto para explorar</span>{/if}<span
          class="status-divider"
        ></span><Icon name="shield" size={13} /><span
          >{indexBusy
            ? 'Alterando index…'
            : working?.trusted
              ? 'Index autorizado'
              : 'Inspeção protegida'}</span
        >
      </div>
    </footer>
  </div>
</div>

{#if indexDialog}
  <IndexDialog
    kind={indexDialog.kind}
    path={indexDialog.intent.path}
    drivers={working?.filterDrivers ?? []}
    onconfirm={() => void confirmIndex()}
    oncancel={cancelIndex}
  />
{/if}

{#if pathDialogOpen}
  <RepositoryPathDialog
    onopen={(path) => {
      pathDialogOpen = false;
      void load(path);
    }}
    oncancel={() => (pathDialogOpen = false)}
  />
{/if}

{#if branchMenu}<BranchMenu
    x={branchMenu.x}
    y={branchMenu.y}
    items={branchItems}
    onclose={closeBranch}
  />{/if}
{#if branchIntent && snapshot}<BranchDialog
    sessionId={snapshot.sessionId}
    generation={snapshot.generation}
    intent={branchIntent}
    onclose={closeBranch}
    onchanged={branchChanged}
    onbusy={(v) => (branchBusy = v)}
  />{/if}
