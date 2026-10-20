<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import * as api from '../../lib/ipc/client';
  import type {
    StashEntry,
    StashAction,
    StashPlan,
    CommitFiles,
    BranchChange,
    WorkingStatus,
  } from '../../lib/ipc/bindings';
  import DiffViewer from '../diff/DiffViewer.svelte';
  import FileList from '../diff/FileList.svelte';
  import BranchMenu from '../branches/BranchMenu.svelte';
  let {
    sessionId,
    generation,
    disabled,
    onbusy,
    onchanged,
  }: {
    sessionId: number;
    generation: number;
    disabled: boolean;
    onbusy: (busy: boolean) => void;
    onchanged: (result: BranchChange) => Promise<void>;
  } = $props();
  let entries = $state.raw<StashEntry[]>([]);
  let expanded = $state(true),
    loading = $state(false),
    busy = $state(false);
  let observed = $state.raw<WorkingStatus | null>(null);
  let failure = $state(''),
    result = $state(''),
    hasMore = $state(false);
  let selected = $state.raw<StashEntry | null>(null);
  let intent = $state<'details' | 'create' | 'apply' | 'drop' | null>(null);
  let plan = $state.raw<StashPlan | null>(null);
  let message = $state(''),
    includeUntracked = $state(false),
    restoreIndex = $state(false);
  let layers = $state.raw<CommitFiles[]>([]),
    layerIndex = $state(0),
    fileIndex = $state<number | null>(null);
  let dialog: HTMLDialogElement | undefined = $state();
  let focus: HTMLElement | null = null;
  let disposed = false;
  let menu = $state<{ x: number; y: number; entry: StashEntry } | null>(null);
  let shown = $state(50);
  let active = $state(0);
  const layer = $derived(layers[layerIndex]);
  const file = $derived(layer?.files.find((f) => f.index === fileIndex));
  const position = $derived(
    layer?.files.findIndex((f) => f.index === fileIndex) ?? 0,
  );
  async function refresh() {
    if (busy || loading || disabled || disposed) return;
    const g = generation;
    loading = true;
    try {
      const r = await api.listStashes(sessionId, g);
      if (disposed || g !== generation) return;
      if (
        entries[0]?.selection.revision !==
          r.data.entries[0]?.selection.revision ||
        entries.length !== r.data.entries.length
      ) {
        entries = r.data.entries;
      }
      hasMore = r.data.hasMore;
      if (!intent) failure = '';
      if (
        selected &&
        !entries.some(
          (e) =>
            e.selection.revision === selected!.selection.revision &&
            e.selection.index === selected!.selection.index &&
            e.selection.oid === selected!.selection.oid,
        )
      ) {
        plan = null;
        if (intent && !result)
          failure =
            'A lista mudou externamente. Feche e selecione novamente antes de operar.';
      }
    } catch (e) {
      if (!disposed && g === generation && api.readableError(e).code !== 'Busy')
        failure = api.readableError(e).message;
    } finally {
      loading = false;
    }
  }
  $effect(() => {
    generation;
    if (!disabled) untrack(() => void refresh());
  });
  onMount(() => {
    const timer = setInterval(() => {
      if (!document.hidden) void refresh();
    }, 3000);
    const update = () => void refresh();
    window.addEventListener('focus', update);
    return () => {
      disposed = true;
      clearInterval(timer);
      window.removeEventListener('focus', update);
    };
  });
  async function open(
    kind: typeof intent,
    entry: StashEntry | null,
    target?: HTMLElement,
  ) {
    if (disabled || busy) return;
    focus = target ?? (document.activeElement as HTMLElement);
    selected = entry;
    intent = kind;
    plan = null;
    failure = '';
    result = '';
    observed = null;
    layers = [];
    fileIndex = null;
    layerIndex = 0;
    message = '';
    includeUntracked = false;
    restoreIndex = false;
    await tick();
    dialog?.showModal();
    dialog?.querySelector<HTMLElement>('input,button')?.focus();
    if (kind === 'details' && entry) {
      busy = true;
      try {
        layers = (
          await api.getStashDetails(sessionId, generation, entry.selection)
        ).data;
      } catch (e) {
        failure = api.readableError(e).message;
      } finally {
        busy = false;
      }
    }
  }
  async function closeFile() {
    const previous = position;
    fileIndex = null;
    await tick();
    dialog
      ?.querySelectorAll<HTMLButtonElement>('.file-item')
      [previous]?.focus();
  }
  function entryKey(event: KeyboardEvent, entry: StashEntry, index: number) {
    if (
      event.key === 'ContextMenu' ||
      (event.shiftKey && event.key === 'F10')
    ) {
      context(event, entry);
      return;
    }
    const next =
      event.key === 'ArrowDown'
        ? index + 1
        : event.key === 'ArrowUp'
          ? index - 1
          : event.key === 'Home'
            ? 0
            : event.key === 'End'
              ? Math.min(shown, entries.length) - 1
              : null;
    if (next === null) return;
    event.preventDefault();
    active = Math.max(0, Math.min(Math.min(shown, entries.length) - 1, next));
    document
      .querySelectorAll<HTMLButtonElement>('.stash-entry')
      [active]?.focus();
  }
  function close() {
    if (busy) return;
    intent = null;
    plan = null;
    void tick().then(() => {
      if (focus?.isConnected) focus.focus();
      else
        document
          .querySelector<HTMLButtonElement>('[aria-label="Criar stash"]')
          ?.focus();
    });
  }
  function action(): StashAction {
    if (intent === 'create')
      return { kind: 'create', message, includeUntracked };
    if (!selected) throw Error('Seleção ausente');
    return intent === 'drop'
      ? { kind: 'drop', selection: selected.selection }
      : { kind: 'apply', selection: selected.selection, restoreIndex };
  }
  async function review() {
    busy = true;
    failure = '';
    plan = null;
    try {
      plan = (await api.prepareStash(sessionId, generation, action())).data;
      await tick();
      dialog?.querySelector<HTMLButtonElement>('[data-cancel]')?.focus();
    } catch (e) {
      failure = api.readableError(e).message;
    } finally {
      busy = false;
    }
  }
  async function trust() {
    busy = true;
    try {
      await api.setRepositoryTrust(sessionId, generation, true);
      await review();
    } catch (e) {
      failure = api.readableError(e).message;
    } finally {
      busy = false;
    }
  }
  async function execute() {
    if (!plan || plan.block || busy) return;
    const token = plan.token;
    plan = null;
    busy = true;
    onbusy(true);
    try {
      const r = (await api.executeStash(sessionId, generation, token)).data;
      result = r.message;
      observed = r.snapshot?.data.status ?? null;
      await onchanged(r);
    } catch (e) {
      result =
        api.readableError(e).message +
        ' Nenhuma repetição automática; feche e revise novamente.';
    } finally {
      busy = false;
      onbusy(false);
      void refresh();
    }
  }
  function context(event: MouseEvent | KeyboardEvent, entry: StashEntry) {
    if (disabled || busy) return;
    event.preventDefault();
    focus = event.currentTarget as HTMLElement;
    const r = focus.getBoundingClientRect();
    menu = {
      x: event instanceof MouseEvent ? event.clientX : r.left,
      y: event instanceof MouseEvent ? event.clientY : r.bottom,
      entry,
    };
  }
  function choose(kind: 'details' | 'apply' | 'drop') {
    const entry = menu!.entry;
    menu = null;
    void open(kind, entry, focus ?? undefined);
  }
</script>

<section class="sidebar-section stash-section" aria-label="Stashes">
  <div class="section-label">
    <button
      class="expand"
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
      >{expanded ? '▾' : '▸'} STASHES
      <span class="count">{entries.length}{hasMore ? '+' : ''}</span></button
    >
    <button
      class="icon-button"
      aria-label="Criar stash"
      title="Criar stash"
      disabled={disabled || busy}
      onclick={(e) => open('create', null, e.currentTarget)}>+</button
    >
  </div>
  {#if expanded}
    <div class="stash-list" aria-busy={loading}>
      {#each entries.slice(0, shown) as entry, index (`${entry.selection.revision}:${entry.selection.index}`)}
        <button
          class="stash-entry"
          tabindex={index === Math.min(active, entries.length - 1) ? 0 : -1}
          onfocus={() => (active = index)}
          aria-label={`${entry.reference}: ${entry.message}`}
          title={`${entry.message}\n${entry.timestamp}\n${entry.branch ?? 'Branch não informada'}\n${entry.selection.oid}`}
          disabled={disabled || busy}
          onclick={(e) => open('details', entry, e.currentTarget)}
          oncontextmenu={(e) => context(e, entry)}
          onkeydown={(e) => entryKey(e, entry, index)}
          ><span class="truncate">{entry.reference} · {entry.message}</span
          ><small>{entry.timestamp.slice(0, 16)}</small></button
        >
      {:else}<p class="muted sidebar-empty">
          {loading ? 'Carregando…' : 'Nenhum stash'}
        </p>{/each}
      {#if entries.length > shown}<button
          class="text-button"
          onclick={() => (shown += 50)}>Mostrar mais</button
        >{/if}
      {#if hasMore}<p class="muted">Limite: primeiras 1.000 entradas.</p>{/if}
    </div>
  {/if}
  {#if failure && !intent}<p role="status">{failure}</p>{/if}
</section>
{#if menu}<BranchMenu
    label="Stashes"
    x={menu.x}
    y={menu.y}
    onclose={() => {
      menu = null;
      focus?.focus();
    }}
    items={[
      { label: 'Visualizar stash', pick: () => choose('details') },
      { label: 'Aplicar stash', pick: () => choose('apply') },
      { label: 'Excluir stash', pick: () => choose('drop') },
    ]}
  />{/if}
{#if intent}
  <dialog
    bind:this={dialog}
    class="index-dialog stash-dialog"
    class:details={intent === 'details'}
    aria-labelledby="stash-title"
    oncancel={(e) => {
      e.preventDefault();
      if (fileIndex !== null) void closeFile();
      else close();
    }}
  >
    <h2 id="stash-title">
      {intent === 'create'
        ? 'Criar stash'
        : intent === 'apply'
          ? 'Aplicar stash'
          : intent === 'drop'
            ? 'Excluir stash'
            : 'Detalhes do stash'}
    </h2>
    <div class="index-dialog-body">
      {#if selected}<p>
          <strong>{selected.reference} · {selected.message}</strong>
        </p>
        <p class="muted">
          {selected.timestamp} · {selected.branch ?? 'Branch não informada'}
        </p>
        <code class="oid">{selected.selection.oid}</code>{/if}
      {#if intent === 'create'}
        <label for="stash-message">Mensagem personalizada</label><input
          id="stash-message"
          bind:value={message}
          maxlength="4096"
          disabled={busy || !!result}
          oninput={() => (plan = null)}
        />
        <label
          ><input
            type="checkbox"
            bind:checked={includeUntracked}
            disabled={busy || !!result}
            onchange={() => (plan = null)}
          /> Incluir untracked (ignored são preservados)</label
        >
        <p>
          Inclui alterações tracked staged e unstaged. Após o armazenamento
          bem-sucedido, os arquivos incluídos serão retirados do working tree.
        </p>
      {:else if intent === 'apply'}
        <label
          ><input
            type="checkbox"
            bind:checked={restoreIndex}
            disabled={busy || !!result}
            onchange={() => (plan = null)}
          /> Restaurar também o index (--index)</label
        >
        <p>
          O stash será mantido. A aplicação pode produzir conflitos; revise o
          estado real após a tentativa.
        </p>
      {:else if intent === 'drop'}<p>
          Esta entrada será excluída do reflog. Confirme a referência e o OID
          acima; a ação não possui recuperação automática.
        </p>{/if}
      {#if intent === 'details' && layers.length}
        <div class="layer-tabs" aria-label="Camada do stash">
          {#each layers as l, i}<button
              class="toolbar-button"
              aria-pressed={layerIndex === i}
              onclick={() => {
                layerIndex = i;
                fileIndex = null;
              }}
              >{[
                'Tracked · base → working tree',
                'Index · base → staged',
                'Untracked',
              ][i]}</button
            >{/each}
        </div>
        {#if layer}<p>
            {layer.totalFiles} arquivos · +{layer.additions} −{layer.deletions}
          </p>
          <FileList
            files={layer.files}
            label="Arquivos do stash"
            selectedIndex={fileIndex}
            onopen={(i) => (fileIndex = i)}
          />
          {#if layer.hasMoreFiles}<p>
              Lista limitada a {layer.files.length} arquivos.
            </p>{/if}
        {/if}
      {/if}
      {#if plan}<p>Arquivos abrangidos ({plan.paths.length}):</p>
        <ul>
          {#each plan.paths as path}<li><code>{path}</code></li>{/each}
        </ul>
        {#if plan.block}<p role="status">
            {plan.block}
          </p>{/if}{#if !plan.trusted}<button
            class="toolbar-button"
            disabled={busy}
            onclick={trust}>Confiar neste repositório</button
          >{/if}{/if}
      {#if busy}<p role="status">Verificando estado do Git…</p>{/if}
      {#if failure}<p role="alert">{failure}</p>{/if}
      {#if result}<p class="stash-result" role="status">{result}</p>{/if}
      {#if observed}
        <div class="stash-observed" role="status">
          Estado observado: {observed.staged} preparadas · {observed.modified} modificadas
          · {observed.untracked} untracked · {observed.conflicts} conflitos.
          {#if observed.changes.length}<ul>
              {#each observed.changes as change}<li>
                  <code>{change.path}</code>
                </li>{/each}
            </ul>{/if}
          {#if observed.hasMoreChanges}<p>
              Lista de alterações limitada; confira o painel de alterações
              locais.
            </p>{/if}
        </div>
      {/if}
    </div>
    <div class="index-dialog-actions">
      <button class="toolbar-button" data-cancel disabled={busy} onclick={close}
        >{result || intent === 'details' ? 'Fechar' : 'Cancelar'}</button
      >
      {#if intent === 'details' && selected && !busy && !failure}<button
          class="toolbar-button"
          onclick={() => {
            intent = 'apply';
            plan = null;
          }}>Aplicar stash</button
        ><button
          class="toolbar-button"
          onclick={() => {
            intent = 'drop';
            plan = null;
          }}>Excluir stash</button
        >
      {:else if intent !== 'details' && !result}{#if plan && !plan.block}<button
            class="primary-button"
            disabled={busy}
            onclick={execute}
            >Confirmar {intent === 'create'
              ? 'criação'
              : intent === 'apply'
                ? 'aplicação'
                : 'exclusão'}</button
          >{:else}<button
            class="primary-button"
            disabled={busy}
            onclick={review}>Revisar operação</button
          >{/if}{/if}
    </div>
    {#if file && layer && intent === 'details'}<div class="stash-diff">
        {#key `${layer.oid}:${file.index}`}<DiffViewer
            {sessionId}
            {generation}
            source={{ kind: 'commit', oid: layer.oid, base: layer.base }}
            {file}
            {position}
            count={layer.files.length}
            onclose={closeFile}
            onstep={(d) => {
              fileIndex =
                layer.files[
                  Math.max(0, Math.min(layer.files.length - 1, position + d))
                ].index;
            }}
          />{/key}
      </div>{/if}
  </dialog>
{/if}

<style>
  .stash-section {
    flex-shrink: 0;
    max-height: 32vh;
    overflow: auto;
  }
  .expand {
    display: flex;
    gap: 6px;
    align-items: center;
    flex: 1;
    border: 0;
    background: transparent;
    text-align: left;
    font-size: 11px;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .stash-entry {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    border: 0;
    background: transparent;
    padding: 8px 10px;
    text-align: left;
  }
  .stash-entry:hover,
  .stash-entry:focus-visible {
    background: #304665;
  }
  .stash-entry small {
    color: var(--muted);
  }
  .stash-entry span {
    width: 100%;
  }
  .stash-dialog {
    max-height: 90vh;
    overflow: auto;
  }
  .stash-dialog.details {
    width: min(900px, 92vw);
    max-width: 92vw;
  }
  .oid {
    display: block;
    overflow-wrap: anywhere;
    font-size: 11px;
    margin: 8px 0;
  }
  input:not([type='checkbox']) {
    display: block;
    width: 100%;
    padding: 8px;
    margin: 8px 0;
    background: var(--bg);
    border: 1px solid var(--line);
    color: var(--text-strong);
  }
  label {
    display: block;
    margin: 10px 0;
  }
  ul {
    max-height: 150px;
    overflow: auto;
    overflow-wrap: anywhere;
  }
  .layer-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 12px 0;
  }
  .layer-tabs [aria-pressed='true'] {
    border-color: var(--teal);
  }
  .stash-diff {
    position: absolute;
    inset: 0;
    background: var(--bg);
    min-height: 0;
  }
  .stash-dialog:has(.stash-diff) {
    height: 85vh;
  }
</style>
