<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import * as api from '../../lib/ipc/client';
  import type { StashEntry, CommitFiles } from '../../lib/ipc/bindings';
  import DiffViewer from '../diff/DiffViewer.svelte';
  import FileList from '../diff/FileList.svelte';
  import BranchMenu from '../branches/BranchMenu.svelte';
  let {
    sessionId,
    generation,
    disabled,
  }: {
    sessionId: number;
    generation: number;
    disabled: boolean;
  } = $props();
  let entries = $state.raw<StashEntry[]>([]);
  let expanded = $state(true),
    loading = $state(false),
    busy = $state(false);
  let failure = $state(''),
    hasMore = $state(false);
  let selected = $state.raw<StashEntry | null>(null);
  let intent = $state<'details' | null>(null);
  const policy =
    'Somente leitura: criar, aplicar e excluir stash estão desabilitados nesta versão por segurança.';
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
        if (intent)
          failure =
            'A lista mudou externamente. Feche e selecione novamente antes de inspecionar.';
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
    failure = '';
    layers = [];
    fileIndex = null;
    layerIndex = 0;
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
    void tick().then(() => {
      if (focus?.isConnected) focus.focus();
      else
        (
          document.querySelector<HTMLButtonElement>(
            '.stash-entry:not(:disabled)',
          ) ??
          document.querySelector<HTMLButtonElement>('.stash-section .expand')
        )?.focus();
    });
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
  function choose(kind: 'details') {
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
    <button class="icon-button" aria-label="Criar stash" title={policy} disabled
      >+</button
    >
  </div>
  <p class="stash-policy">{policy}</p>
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
    items={[{ label: 'Visualizar stash', pick: () => choose('details') }]}
  />{/if}
{#if intent}
  <dialog
    bind:this={dialog}
    class="index-dialog stash-dialog details"
    aria-labelledby="stash-title"
    oncancel={(e) => {
      e.preventDefault();
      if (fileIndex !== null) void closeFile();
      else close();
    }}
  >
    <h2 id="stash-title">Detalhes do stash</h2>
    <div class="index-dialog-body">
      {#if selected}<p>
          <strong>{selected.reference} · {selected.message}</strong>
        </p>
        <p class="muted">
          {selected.timestamp} · {selected.branch ?? 'Branch não informada'}
        </p>
        <code class="oid">{selected.selection.oid}</code>{/if}
      <p>{policy}</p>
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
      {#if busy}<p role="status">Lendo stash…</p>{/if}
      {#if failure}<p role="alert">{failure}</p>{/if}
    </div>
    <div class="index-dialog-actions">
      <button class="toolbar-button" disabled={busy} onclick={close}
        >Fechar</button
      >
      <button class="toolbar-button" disabled title={policy}
        >Aplicar stash</button
      >
      <button class="toolbar-button" disabled title={policy}
        >Excluir stash</button
      >
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
  .stash-policy {
    color: var(--muted);
    font-size: 11px;
    margin: 6px 10px;
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
