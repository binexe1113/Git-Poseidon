<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import * as api from '../../lib/ipc/client';
  import type {
    AppError,
    Worktree,
    WorktreeAvailability,
    WorktreeList,
    WorktreeStatus,
  } from '../../lib/ipc/bindings';
  import Icon from '../../lib/ui/Icon.svelte';
  let {
    sessionId,
    generation,
    disabled,
    onopen,
  }: {
    sessionId: number;
    generation: number;
    disabled: boolean;
    onopen: (
      revision: number,
      key: number,
      onFailure: (failure: AppError) => void,
    ) => Promise<boolean>;
  } = $props();
  let list = $state.raw<WorktreeList | null>(null);
  let expanded = $state(true),
    loading = $state(false),
    stale = $state(false);
  let failure = $state('');
  let selected = $state.raw<Worktree | null>(null);
  // The listing the dialog was opened from. It is never re-targeted.
  let selectedRevision = $state(0);
  let status = $state.raw<WorktreeStatus | null>(null);
  let statusLoading = $state(false),
    opening = $state(false);
  let statusError = $state(''),
    notice = $state('');
  let dialog: HTMLDialogElement | undefined = $state();
  let focus: HTMLElement | null = null;
  let active = $state(0);
  let disposed = false;
  const states: Record<WorktreeAvailability, string> = {
    available: 'disponível',
    missing: 'ausente',
    initializing: 'inicializando',
    inconsistent: 'inconsistente',
    inaccessible: 'inacessível',
  };
  const worktrees = $derived(list?.worktrees ?? []);
  const outdated = $derived(
    !!selected && !!list && list.revision !== selectedRevision,
  );

  function kind(w: Worktree) {
    return w.kind === 'main'
      ? 'principal'
      : w.kind === 'bare'
        ? 'repositório bare'
        : 'vinculado';
  }
  function position(w: Worktree) {
    if (w.branch) return w.branch;
    if (w.kind === 'bare') return 'sem working tree';
    if (w.head) return `HEAD destacado · ${w.head.slice(0, 8)}`;
    return 'HEAD não legível';
  }
  function describe(w: Worktree) {
    return [
      w.name,
      kind(w),
      position(w),
      w.current ? 'aberto agora' : '',
      w.locked ? 'bloqueado' : '',
      w.availability === 'available' ? '' : states[w.availability],
    ]
      .filter(Boolean)
      .join(', ');
  }

  async function refresh() {
    if (loading || disabled || disposed) return;
    const g = generation;
    loading = true;
    try {
      const r = await api.listWorktrees(sessionId, g);
      if (disposed || g !== generation) return;
      list = r.data;
      failure = '';
      stale = false;
    } catch (e) {
      const error = api.readableError(e);
      if (!disposed && g === generation && error.code !== 'Busy') {
        // Entries of a listing that could not be read again are not offered.
        failure = error.message;
        list = null;
      }
    } finally {
      loading = false;
    }
  }
  $effect(() => {
    generation;
    if (!disabled) untrack(() => void refresh());
  });
  onMount(() => {
    const update = () => {
      if (!document.hidden) void refresh();
    };
    window.addEventListener('focus', update);
    return () => {
      disposed = true;
      window.removeEventListener('focus', update);
    };
  });

  function refused(error: AppError) {
    notice = error.message;
    if (error.code === 'StaleSession') {
      stale = true;
      void refresh();
    }
  }
  async function show(w: Worktree, target: HTMLElement) {
    if (disabled || !list) return;
    focus = target;
    selected = w;
    selectedRevision = list.revision;
    status = null;
    statusError = notice = '';
    await tick();
    dialog?.showModal();
    (
      dialog?.querySelector<HTMLElement>('.worktree-open:not(:disabled)') ??
      dialog?.querySelector<HTMLElement>('.index-dialog-actions button')
    )?.focus();
    if (w.availability !== 'available' || w.kind === 'bare') return;
    const revision = list.revision;
    statusLoading = true;
    try {
      const r = await api.getWorktreeStatus(
        sessionId,
        generation,
        revision,
        w.key,
      );
      if (selected?.key === w.key && selectedRevision === revision)
        status = r.data;
    } catch (e) {
      const error = api.readableError(e);
      statusError = error.message;
      if (error.code === 'StaleSession') refused(error);
    } finally {
      statusLoading = false;
    }
  }
  async function open() {
    if (!selected || outdated || opening || disabled) return;
    opening = true;
    notice = '';
    const done = await onopen(selectedRevision, selected.key, refused);
    if (!disposed) opening = false;
    if (done && !disposed) close();
  }
  function close() {
    if (opening) return;
    selected = null;
    dialog?.close();
    void tick().then(() => {
      if (focus?.isConnected) focus.focus();
    });
  }
  function move(event: KeyboardEvent, index: number) {
    const last = worktrees.length - 1;
    const next =
      event.key === 'ArrowDown'
        ? index + 1
        : event.key === 'ArrowUp'
          ? index - 1
          : event.key === 'Home'
            ? 0
            : event.key === 'End'
              ? last
              : null;
    if (next === null) return;
    event.preventDefault();
    active = Math.max(0, Math.min(last, next));
    document
      .querySelectorAll<HTMLButtonElement>('.worktree-entry')
      [active]?.focus();
  }
</script>

<section class="sidebar-section worktree-section" aria-label="Worktrees">
  <div class="section-label">
    <button
      class="expand"
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
      >{expanded ? '▾' : '▸'} WORKTREES
      <span class="count">{worktrees.length}{list?.truncated ? '+' : ''}</span
      ></button
    >
    <button
      class="icon-button"
      aria-label="Atualizar worktrees"
      title="Atualizar a lista de worktrees"
      disabled={disabled || loading}
      onclick={() => void refresh()}><Icon name="refresh" size={14} /></button
    >
  </div>
  {#if expanded}
    {#if stale}<p class="worktree-note" role="status">
        A lista de worktrees mudou fora do aplicativo. {loading
          ? 'Atualizando…'
          : 'Selecione novamente.'}
      </p>{/if}
    {#if failure}<div class="worktree-note" role="alert">
        {failure}
        <button class="text-button" onclick={() => void refresh()}
          >Tentar novamente</button
        >
      </div>{/if}
    <div class="worktree-list" aria-busy={loading}>
      {#each worktrees as w, index (`${list?.revision}:${w.key}`)}
        <button
          class="worktree-entry"
          class:current={w.current}
          class:unavailable={w.availability !== 'available'}
          tabindex={index === Math.min(active, worktrees.length - 1) ? 0 : -1}
          aria-current={w.current ? 'true' : undefined}
          aria-label={describe(w)}
          title={w.path}
          {disabled}
          onfocus={() => (active = index)}
          onclick={(e) => show(w, e.currentTarget)}
          onkeydown={(e) => move(e, index)}
        >
          <span class="worktree-line"
            ><Icon name={w.kind === 'linked' ? 'branch' : 'folder'} size={14} />
            <span class="truncate name">{w.name}</span>
            {#if w.current}<span class="small-badge">aberto</span>{/if}</span
          ><span class="worktree-line"
            >{#if w.availability !== 'available'}<span class="small-badge warn"
                >{states[w.availability]}</span
              >{:else if w.locked}<span class="small-badge">bloqueado</span
              >{/if}<small class="truncate">{kind(w)} · {position(w)}</small
            ></span
          >
        </button>
      {:else}<p class="muted sidebar-empty">
          {loading ? 'Carregando…' : failure ? '' : 'Nenhum worktree listado.'}
        </p>{/each}
      {#if list && worktrees.length === 1}<p class="muted worktree-note">
          Somente o worktree principal.
        </p>{/if}
      {#if list?.truncated}<p class="muted worktree-note">
          Exibindo os primeiros 256 worktrees.
        </p>{/if}
    </div>
  {/if}
</section>

{#if selected}
  <dialog
    bind:this={dialog}
    class="index-dialog worktree-dialog"
    aria-labelledby="worktree-title"
    oncancel={(e) => {
      e.preventDefault();
      close();
    }}
  >
    <h2 id="worktree-title">Worktree {selected.name}</h2>
    <div class="index-dialog-body">
      <dl class="worktree-facts">
        <dt>Caminho</dt>
        <dd><code>{selected.path}</code></dd>
        <dt>Tipo</dt>
        <dd>{kind(selected)}{selected.current ? ' · aberto agora' : ''}</dd>
        <dt>Posição</dt>
        <dd>{position(selected)}</dd>
        {#if selected.head}<dt>Commit</dt>
          <dd><code>{selected.head}</code></dd>{/if}
        <dt>Estado</dt>
        <dd>
          {states[selected.availability]}{#if selected.reason}<span
              class="worktree-reason">{selected.reason}</span
            >{/if}
        </dd>
        {#if selected.locked}<dt>Bloqueio</dt>
          <dd>
            {selected.lockReason ?? 'sem motivo informado'}<span
              class="worktree-reason"
              >Estado administrativo do <code>git worktree lock</code>: impede
              que o Git remova, mova ou pode este registro. Não bloqueia as
              operações do Poseidon neste worktree.</span
            >
          </dd>{/if}
        {#if selected.prunable}<dt>Podável</dt>
          <dd>{selected.prunable}</dd>{/if}
      </dl>

      {#if selected.availability === 'available' && selected.kind !== 'bare'}
        <h3>Alterações locais</h3>
        {#if statusLoading}<p role="status">Lendo status protegido…</p>
        {:else if status}<p>
            {status.staged} preparadas · {status.modified} modificadas · {status.untracked}
            não rastreadas{status.conflicts
              ? ` · ${status.conflicts} em conflito`
              : ''}{status.hasMoreChanges ? ' (lista limitada)' : ''}
          </p>
          <h3>Permissões</h3>
          <p>
            Confiança do repositório: <strong
              >{status.trusted ? 'concedida' : 'não concedida'}</strong
            > (compartilhada por todos os worktrees).
          </p>
          <p>
            Filtros neste worktree: <strong
              >{status.filters === 'none'
                ? 'nenhum configurado'
                : status.filters === 'authorized'
                  ? 'autorizados'
                  : 'exigem revisão'}</strong
            >{status.filters === 'reviewRequired'
              ? '. A configuração efetiva deste worktree difere da autorizada: escritas aqui pedirão nova revisão.'
              : '.'}
          </p>
        {:else if statusError}<p role="alert">{statusError}</p>{/if}
        <p class="muted">
          Status lido com filtros neutralizados; nenhum programa do repositório
          é executado.
        </p>
      {/if}
      <p class="muted">
        Abrir troca apenas a sessão exibida no Poseidon: nenhuma branch é
        trocada e nenhum arquivo é alterado. Criar, remover, mover e podar
        worktrees não estão disponíveis nesta versão.
      </p>
      {#if outdated && !notice}<p role="status">
          A lista de worktrees mudou depois desta seleção. Feche e selecione o
          worktree novamente.
        </p>{/if}
      {#if notice}<p role="alert">{notice}</p>{/if}
    </div>
    <div class="index-dialog-actions">
      {#if stale}<button
          class="toolbar-button"
          disabled={loading}
          onclick={() => void refresh()}>Atualizar lista</button
        >{/if}
      <button class="toolbar-button" disabled={opening} onclick={close}
        >Fechar</button
      >
      <button
        class="primary-button worktree-open"
        disabled={disabled ||
          opening ||
          stale ||
          outdated ||
          selected.current ||
          selected.availability !== 'available'}
        title={selected.current
          ? 'Este worktree já está aberto.'
          : (selected.reason ?? 'Abrir este worktree no Poseidon')}
        onclick={() => void open()}
        >{opening ? 'Abrindo…' : 'Abrir worktree'}</button
      >
    </div>
  </dialog>
{/if}

<style>
  .worktree-section {
    flex-shrink: 0;
    max-height: 30vh;
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
  .worktree-entry {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    min-width: 0;
    border: 0;
    background: transparent;
    padding: 7px 10px;
    text-align: left;
  }
  .worktree-entry:hover:not(:disabled),
  .worktree-entry:focus-visible {
    background: #304665;
  }
  .worktree-entry.current {
    box-shadow: inset 2px 0 0 var(--teal);
  }
  .worktree-entry.unavailable .truncate {
    color: var(--muted);
  }
  .worktree-line {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    width: 100%;
  }
  .worktree-entry small {
    color: var(--muted);
    flex: 1 1 auto;
    min-width: 0;
  }
  .worktree-entry .name {
    flex: 1 1 auto;
    min-width: 0;
  }
  .worktree-entry .small-badge {
    flex-shrink: 0;
  }
  .warn {
    color: #f2c46d;
  }
  .worktree-note {
    color: var(--muted);
    font-size: 11px;
    margin: 6px 10px;
  }
  .worktree-dialog {
    width: min(640px, 92vw);
    max-width: 92vw;
    max-height: 90vh;
    overflow: auto;
  }
  .worktree-facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 6px 14px;
    margin: 0 0 12px;
  }
  .worktree-facts dt {
    color: var(--muted);
  }
  .worktree-facts dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .worktree-facts code {
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .worktree-reason {
    display: block;
    color: var(--muted);
    font-size: 12px;
    margin-top: 2px;
  }
  .worktree-dialog h3 {
    font-size: 13px;
    margin: 14px 0 6px;
  }
</style>
