<script lang="ts">
  import Icon from '../../lib/ui/Icon.svelte';
  import FileList from '../diff/FileList.svelte';
  import { areaTitle, type FileAction } from '../diff/source';
  import type {
    WorkingArea,
    WorkingChanges,
    WorkingStatus,
  } from '../../lib/ipc/bindings';
  let {
    status,
    working,
    loading,
    error,
    selected,
    busy,
    message,
    moved,
    onopen,
    onretry,
    onindex,
    onrevoke,
  }: {
    status: WorkingStatus;
    working: WorkingChanges | null;
    loading: boolean;
    error: string | null;
    selected: { area: WorkingArea; index: number } | null;
    /** File whose stage/unstage is running. */
    busy: { area: WorkingArea; index: number } | null;
    /** Result or error of the last index action. */
    message: { kind: 'ok' | 'error'; text: string } | null;
    /** File that just changed group, briefly marked in its new group. */
    moved: { area: WorkingArea; path: string } | null;
    onopen: (area: WorkingArea, index: number) => void;
    onretry: () => void;
    onindex: (area: WorkingArea, index: number) => void;
    onrevoke: () => void;
  } = $props();
  // A path in both index groups: staging it whole replaces the staged version.
  const partial = $derived(
    new Set(
      working
        ? working.staged
            .map((f) => f.path)
            .filter((p) => working.unstaged.some((u) => u.path === p))
        : [],
    ),
  );
  const action = (area: WorkingArea): FileAction => ({
    label: area === 'staged' ? 'Retirar do index' : 'Preparar arquivo',
    key: area === 'staged' ? 'u' : 's',
    icon: area === 'staged' ? 'minus' : 'plus',
    busy: busy?.area === area ? busy.index : busy ? -1 : null,
    disabled: working?.writeBlock ?? (loading ? 'Atualizando lista…' : null),
    run: (index) => onindex(area, index),
  });
  let changeFilter = $state<'all' | 'staged' | 'working'>('all');
  const groups = $derived(
    working
      ? (
          [
            ['staged', working.staged, working.totalStaged],
            ['unstaged', working.unstaged, working.totalUnstaged],
            ['untracked', working.untracked, working.totalUntracked],
          ] as const
        ).filter(
          ([area, files]) =>
            files.length > 0 &&
            (changeFilter === 'all' ||
              (changeFilter === 'staged') === (area === 'staged')),
        )
      : [],
  );
  const counts = $derived({
    all: working
      ? working.totalStaged + working.totalUnstaged + working.totalUntracked
      : status.totalFiles,
    staged: working?.totalStaged ?? status.staged,
    working: working
      ? working.totalUnstaged + working.totalUntracked
      : status.modified + status.untracked,
  });
  const range: Record<WorkingArea, string> = $derived({
    staged: working?.headOid
      ? `HEAD ${working.headOid.slice(0, 8)} → index`
      : 'árvore vazia → index',
    unstaged: 'index → working tree',
    untracked: 'conteúdo novo',
  });
</script>

<section
  class="changes-panel"
  aria-labelledby="changes-title"
  aria-busy={loading || !!busy}
  data-revision={working?.revision}
>
  <div class="changes-heading">
    <h3 id="changes-title">Working tree atual</h3>
    <p>
      Estado do repositório agora, não do commit selecionado. Relido ao abrir
      esta aba, ao voltar à janela e com Ctrl+R. Selecione um arquivo para ver o
      diff.
    </p>
    {#if working}<p
        class="trust-bar"
        class:trusted={working.trusted}
        class:blocked={!!working.writeBlock}
      >
        <Icon
          name={working.writeBlock
            ? 'alert'
            : working.trusted
              ? 'shield'
              : 'unlock'}
          size={14}
        /><span
          >{working.writeBlock ??
            (working.trusted
              ? 'Repositório confiável: preparar e retirar liberados; filtros do Git são executados.'
              : 'Sem confiança: inspeção protegida. O primeiro preparo pede sua confirmação.')}</span
        >{#if working.trusted}<button
            class="text-button"
            disabled={!!busy || loading}
            onclick={onrevoke}>Revogar</button
          >{/if}
      </p>{/if}
    <div class="filter-tabs" aria-label="Filtrar alterações">
      <button
        class:active={changeFilter === 'all'}
        aria-pressed={changeFilter === 'all'}
        onclick={() => (changeFilter = 'all')}
        >Todas <span>{counts.all}</span></button
      ><button
        class:active={changeFilter === 'staged'}
        aria-pressed={changeFilter === 'staged'}
        onclick={() => (changeFilter = 'staged')}
        >Index <span>{counts.staged}</span></button
      ><button
        class:active={changeFilter === 'working'}
        aria-pressed={changeFilter === 'working'}
        onclick={() => (changeFilter = 'working')}
        >Working tree <span>{counts.working}</span></button
      >
    </div>
  </div>
  {#if message}<p
      class="index-message"
      class:error={message.kind === 'error'}
      role={message.kind === 'error' ? 'alert' : 'status'}
    >
      {message.text}
    </p>{/if}
  {#if !status.available}<p class="changes-empty muted">
      Repositórios bare não possuem working tree.
    </p>{:else if error}<div class="changes-empty" role="alert">
      <span class="detail-error">{error}</span>
      <button class="text-button" onclick={onretry}>Tentar novamente</button>
    </div>{:else if !working}<p class="changes-empty muted" role="status">
      Lendo alterações locais…
    </p>{:else if groups.length}
    {#each groups as [area, files, total] (area)}
      <section class="working-group" aria-labelledby={`group-${area}`}>
        <h4 class="message-heading files-heading" id={`group-${area}`}>
          <span>{areaTitle[area]}</span><span class="files-summary"
            >{total}</span
          >
        </h4>
        <p class="diff-base">{range[area]}</p>
        <FileList
          {files}
          label={`${areaTitle[area]}: ${range[area]}`}
          selectedIndex={selected?.area === area ? selected.index : null}
          onopen={(index) => onopen(area, index)}
          action={action(area)}
          {partial}
          highlight={moved?.area === area ? moved.path : null}
        />
      </section>
    {/each}
  {:else}<div class="changes-empty">
      <span class="clean-check"><Icon name="check" size={18} /></span><span
        >{counts.all
          ? 'Nenhuma alteração neste filtro.'
          : 'Tudo em ordem. Nenhuma alteração local.'}</span
      >
    </div>{/if}
  {#if working?.hasMoreFiles}<p class="limit-note">
      Cada grupo lista até 3.000 arquivos; os totais abrangem todos.
    </p>{/if}
</section>
