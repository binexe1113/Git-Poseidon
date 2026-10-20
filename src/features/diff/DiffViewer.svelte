<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../../lib/ui/Icon.svelte';
  import ChangeBadge from './ChangeBadge.svelte';
  import { familyFor, highlight } from './highlight';
  import { loadDiff, sameTarget, type DiffSource } from './source';
  import * as api from '../../lib/ipc/client';
  import type { ChangedFile, DiffLine, FileDiff } from '../../lib/ipc/bindings';
  let {
    sessionId,
    generation,
    source,
    file,
    position,
    count,
    onclose,
    onstep,
    onchanged,
  }: {
    sessionId: number;
    generation: number;
    source: DiffSource;
    file: ChangedFile;
    position: number;
    count: number;
    onclose: () => void;
    onstep: (delta: number) => void;
    /** Local files only: the listing is outdated. Returns false to give up. */
    onchanged?: () => boolean;
  } = $props();
  const ROW = 20,
    OVERSCAN = 40,
    MAX_PAGES = 6;
  let meta = $state.raw<FileDiff | null>(null);
  let pages = $state.raw(new Map<number, DiffLine[]>());
  let allowLarge = $state(false);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let top = $state(0),
    viewport = $state(600);
  let body: HTMLDivElement | undefined = $state();
  let root: HTMLElement | undefined = $state();
  let disposed = false;
  const inflight = new Set<number>();
  const family = $derived(familyFor(file.path));
  const pageLines = $derived(meta?.pageLines || 1000);
  const total = $derived(meta?.totalLines ?? 0);
  const start = $derived(Math.max(0, Math.floor(top / ROW) - OVERSCAN));
  const end = $derived(
    Math.min(total, Math.ceil((top + viewport) / ROW) + OVERSCAN),
  );
  const rows = $derived.by(() => {
    const result: { index: number; line: DiffLine }[] = [];
    for (let index = start; index < end; index++) {
      const page = Math.floor(index / pageLines) * pageLines;
      const line = pages.get(page)?.[index - page];
      if (line) result.push({ index, line });
    }
    return result;
  });
  const firstVisible = $derived(Math.floor(top / ROW));
  // Hunk set by n/p (also when the diff fits and nothing can scroll); a manual
  // scroll returns to the position-derived hunk.
  let activeHunk = $state<number | null>(null);
  let expectedTop = -1;
  const hunk = $derived.by(() => {
    if (activeHunk !== null) return activeHunk;
    const starts = meta?.hunkStarts ?? [];
    let current = -1;
    for (let i = 0; i < starts.length && starts[i] <= firstVisible + 1; i++)
      current = i;
    return current;
  });

  async function fetchPage(offset: number, attempt = 0): Promise<void> {
    if (disposed || inflight.has(offset)) return;
    inflight.add(offset);
    try {
      const response = await loadDiff(
        source,
        sessionId,
        generation,
        file.index,
        offset,
        allowLarge,
      );
      if (disposed) return;
      const data = response.data;
      if (
        response.sessionId !== sessionId ||
        response.generation !== generation ||
        !sameTarget(source, data.target) ||
        data.file.index !== file.index
      )
        return;
      meta = data;
      const next = new Map(pages);
      next.set(data.offset, data.lines);
      // Keep memory bounded on long diffs: drop pages far from the viewport.
      while (next.size > MAX_PAGES) {
        const center = Math.floor(firstVisible / pageLines) * pageLines;
        const far = [...next.keys()].sort(
          (a, b) => Math.abs(b - center) - Math.abs(a - center),
        )[0];
        next.delete(far);
      }
      pages = next;
      error = null;
    } catch (cause) {
      if (disposed) return;
      const failure = api.readableError(cause);
      // A newer diff request superseded this one; retry while still visible.
      if (failure.code === 'Cancelled' && attempt < 3) {
        inflight.delete(offset);
        await new Promise((resolve) => setTimeout(resolve, 60));
        return fetchPage(offset, attempt + 1);
      }
      // Pages of an outdated listing would mix two versions of the file.
      if (failure.code === 'WorkingTreeChanged' && onchanged?.()) return;
      error = failure.message;
    } finally {
      inflight.delete(offset);
      if (!disposed) loading = false;
    }
  }

  onMount(() => {
    // Keyboard users land in the viewer: Esc, n/p and Alt+↑/↓ work at once.
    root?.focus({ preventScroll: true });
    void fetchPage(0);
    return () => {
      disposed = true;
    };
  });

  $effect(() => {
    if (!meta || meta.content !== 'text' || error) return;
    const first = Math.floor(start / pageLines) * pageLines;
    const last = Math.floor(Math.max(start, end - 1) / pageLines) * pageLines;
    for (let offset = first; offset <= last; offset += pageLines)
      if (!pages.has(offset) && !inflight.has(offset)) {
        void fetchPage(offset);
        return;
      }
  });

  $effect(() => {
    if (!body) return;
    const observer = new ResizeObserver(() => {
      viewport = body!.clientHeight;
    });
    observer.observe(body);
    return () => observer.disconnect();
  });

  function scrollToLine(index: number) {
    if (!body) return;
    body.scrollTop = Math.max(0, index * ROW - ROW);
    top = expectedTop = body.scrollTop;
  }
  function stepHunk(delta: number) {
    const starts = meta?.hunkStarts ?? [];
    if (!starts.length) return;
    const target =
      hunk < 0 && delta > 0
        ? 0
        : Math.max(0, Math.min(starts.length - 1, hunk + delta));
    activeHunk = target;
    scrollToLine(starts[target]);
  }
  function openLarge() {
    allowLarge = true;
    meta = null;
    pages = new Map();
    loading = true;
    void fetchPage(0);
  }
  function key(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      onclose();
    } else if (event.altKey && event.key === 'ArrowDown') {
      event.preventDefault();
      onstep(1);
    } else if (event.altKey && event.key === 'ArrowUp') {
      event.preventDefault();
      onstep(-1);
    } else if (
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey &&
      (event.key === 'n' || event.key === 'p')
    ) {
      event.preventDefault();
      stepHunk(event.key === 'n' ? 1 : -1);
    }
  }
  function size(bytes: number | null | undefined) {
    if (bytes === null || bytes === undefined) return '—';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  }
  const name = $derived(file.path.slice(file.path.lastIndexOf('/') + 1));
  const dir = $derived(file.path.slice(0, file.path.lastIndexOf('/') + 1));
  const sign = { added: '+', deleted: '−', context: ' ', hunk: '', note: '' };
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  bind:this={root}
  tabindex="-1"
  class="panel diff-viewer"
  aria-label={`Diff de ${file.path}`}
  onkeydown={key}
  style={`--diff-row:${ROW}px;--diff-columns:${meta?.maxColumns ?? 80}`}
>
  <header class="diff-toolbar">
    <button
      class="toolbar-button"
      onclick={onclose}
      title="Voltar ao histórico (Esc)"
      ><Icon name="history" size={14} /><span>Histórico</span></button
    >
    <div class="diff-title">
      <ChangeBadge kind={file.kind} />
      <h2 title={file.path}>
        <span class="file-dir">{dir}</span>{name}
      </h2>
      {#if file.binary}<span class="file-binary">binário</span
        >{:else if file.additions !== null}<span class="file-stats"
          ><span class="stat-add">+{file.additions}</span><span class="stat-del"
            >−{file.deletions}</span
          ></span
        >{/if}
    </div>
    <div class="diff-actions">
      <button
        class="icon-button"
        onclick={() => onstep(-1)}
        disabled={position <= 0}
        title="Arquivo anterior (Alt+↑)"
        aria-label="Arquivo anterior"><Icon name="up" size={15} /></button
      ><span class="diff-counter">{position + 1}/{count}</span><button
        class="icon-button"
        onclick={() => onstep(1)}
        disabled={position >= count - 1}
        title="Próximo arquivo (Alt+↓)"
        aria-label="Próximo arquivo"><Icon name="down" size={15} /></button
      >
    </div>
  </header>
  <div class="diff-meta">
    <span class="diff-range" title="Lado esquerdo → lado direito">
      {#if source.kind === 'commit'}{#if source.base.kind === 'emptyTree'}árvore
          vazia{:else}{source.base.parentCount > 1 ? '1º pai ' : 'pai '}<code
            >{source.base.oid.slice(0, 8)}</code
          >{/if}
        →
        <code>{source.oid.slice(0, 8)}</code
        >{:else if source.area === 'staged'}{#if source.headOid}HEAD <code
            >{source.headOid.slice(0, 8)}</code
          >{:else}árvore vazia (branch sem commits){/if} → index{:else if source.area === 'unstaged'}index
        → working tree{:else}vazio → working tree (não rastreado){/if}</span
    >
    {#if file.oldPath}<span
        >renomeado de <code>{file.oldPath}</code>{file.similarity !== null
          ? ` · ${file.similarity}%`
          : ''}</span
      >{/if}
    {#if file.oldMode !== file.newMode && file.oldMode !== '000000' && file.newMode !== '000000'}<span
        >modo {file.oldMode} → {file.newMode}</span
      >{/if}
    {#if meta?.content === 'text' && meta.hunkStarts.length}<span
        class="hunk-nav"
        ><button
          class="text-button"
          onclick={() => stepHunk(-1)}
          title="Hunk anterior (p)">↑</button
        >Hunk {Math.max(1, hunk + 1)} de {meta.hunkStarts.length}<button
          class="text-button"
          onclick={() => stepHunk(1)}
          title="Próximo hunk (n)">↓</button
        ></span
      >{/if}
    {#if meta?.lossy}<span class="diff-flag" title="Bytes inválidos em UTF-8"
        >UTF-8 inválido substituído</span
      >{/if}
  </div>
  {#if meta?.truncated}<p class="diff-banner" role="status">
      Exibindo as primeiras {meta.totalLines.toLocaleString('pt-BR')} linhas do diff.
      {#if !allowLarge}<button class="text-button" onclick={openLarge}
          >Carregar até 200.000 linhas</button
        >{/if}
    </p>{/if}
  {#if error}<div class="diff-state" role="alert">
      <Icon name="alert" size={22} />
      <p>{error}</p>
      <button
        class="text-button"
        onclick={() => {
          error = null;
          loading = true;
          void fetchPage(Math.floor(start / pageLines) * pageLines);
        }}>Tentar novamente</button
      >
    </div>{:else if loading && !meta}<div class="diff-state" role="status">
      <span class="spinning"><Icon name="refresh" size={18} /></span>
      <p>Carregando diff…</p>
    </div>{:else if meta && meta.content !== 'text'}<div class="diff-state">
      <Icon name="file" size={24} />
      {#if meta.content === 'binary'}<strong>Arquivo binário</strong>
        <p>Conteúdo não exibido como texto.</p>
      {:else if meta.content === 'noContent' && file.kind === 'unmerged'}<strong
          >Conflito não resolvido</strong
        >
        <p>
          Resolva no editor ou no terminal; a resolução pelo aplicativo vem numa
          fase posterior.
        </p>
      {:else if meta.content === 'noContent' && file.kind === 'untracked'}<strong
          >Sem conteúdo para exibir</strong
        >
        <p>Arquivo vazio, repositório aninhado ou arquivo especial.</p>
      {:else if meta.content === 'noContent'}<strong
          >Sem alteração de conteúdo</strong
        >
        <p>Apenas nome, modo ou arquivo vazio mudou.</p>
      {:else if meta.content === 'large'}<strong>Arquivo grande</strong>
        <p>Acima de 2 MiB, o diff só é calculado sob pedido.</p>
        <button class="text-button" onclick={openLarge}
          >Abrir mesmo assim</button
        >
      {:else}<strong>Arquivo grande demais</strong>
        <p>Acima de 32 MiB, o diff não é calculado nesta versão.</p>
      {/if}
      <p class="muted">
        Antes: {size(meta.oldSize)} · Depois: {size(meta.newSize)}
      </p>
    </div>{:else if meta}
    <!-- Focusable scroll region so arrows/PageUp/PageDown scroll the diff. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="diff-body"
      bind:this={body}
      tabindex="0"
      role="region"
      aria-label={`Linhas do diff, ${total} no total`}
      onscroll={() => {
        top = body!.scrollTop;
        if (Math.abs(top - expectedTop) > 1) activeHunk = null;
      }}
      data-total-lines={total}
      data-row-height={ROW}
    >
      <div class="diff-surface" style={`height:${total * ROW}px`}>
        {#each rows as { index, line } (index)}
          <div
            class="diff-row kind-{line.kind}"
            class:current={activeHunk !== null &&
              index === meta.hunkStarts[activeHunk]}
            style={`transform:translateY(${index * ROW}px)`}
            data-index={index}
          >
            {#if line.kind === 'hunk'}<span class="diff-hunk">{line.text}</span
              >{:else}<span class="ln" aria-hidden="true"
                >{line.oldLine ?? ''}</span
              ><span class="ln" aria-hidden="true">{line.newLine ?? ''}</span
              ><span class="sg" aria-hidden="true">{sign[line.kind]}</span><span
                class="tx"
                >{#if line.kind === 'note'}{line.text}{:else}{#each highlight(line.text, family) as segment}{#if segment.cls}<span
                        class="hl-{segment.cls}">{segment.text}</span
                      >{:else}{segment.text}{/if}{/each}{/if}{#if line.cr}<span
                    class="eol"
                    title="Fim de linha CRLF">␍</span
                  >{/if}{#if line.clipped}<span
                    class="eol"
                    title="Linha recortada para exibição">…</span
                  >{/if}</span
              >{/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}
  <footer class="panel-footer diff-footer" role="status">
    <span
      >{meta?.content === 'text'
        ? `${total.toLocaleString('pt-BR')} linhas · ${meta.hunkStarts.length} hunks`
        : ' '}</span
    ><span
      >Esc volta · n/p hunks · Alt+↑/↓ arquivos{meta
        ? ` · consulta ${meta.elapsedMs} ms`
        : ''}</span
    >
  </footer>
</section>
