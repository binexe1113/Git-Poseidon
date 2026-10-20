<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from '../../lib/ui/Icon.svelte';
  import CommitGraph from './CommitGraph.svelte';
  import RefBadges from './RefBadges.svelte';
  import { ROW_HEIGHT, graphWidth } from './geometry';
  import { readableError } from '../../lib/ipc/client';
  import type {
    Commit,
    CommitRef,
    GraphRow,
    HistoryPage,
  } from '../../lib/ipc/bindings';
  let {
    allCommits,
    graph,
    hasMoreCommits,
    selectedOid,
    selectedOffset,
    loading,
    onselect,
    fetchpage,
    onrefresh,
    oncontext,
  }: {
    allCommits: Commit[];
    graph: GraphRow[];
    hasMoreCommits: boolean;
    selectedOid: string | null;
    selectedOffset: number;
    loading: boolean;
    onselect: (commit: Commit, offset: number, refs: CommitRef[]) => void;
    fetchpage: (offset: number) => Promise<HistoryPage>;
    onrefresh: () => void;
    oncontext: (event: MouseEvent | KeyboardEvent, commit: Commit) => void;
  } = $props();
  const HEIGHT = ROW_HEIGHT,
    SIZE = 200,
    OVERSCAN = 20;
  // Immutable page payloads; scroll does not proxy or copy every commit.
  let pages = $state.raw(new Map<number, HistoryPage>());
  let highWater = $state(0),
    complete = $state(false),
    top = $state(0),
    viewport = $state(600);
  let pageLoading = $state(false),
    pageError = $state<string | null>(null);
  let query = $state(''),
    focused = $state<number | null>(null),
    list: HTMLDivElement;
  let disposed = false;
  const order: number[] = [];
  onMount(() => {
    const initial: HistoryPage = {
      offset: 0,
      commits: allCommits,
      graph,
      nextOffset: hasMoreCommits ? allCommits.length : null,
      complete: !hasMoreCommits,
      cacheBytes: 0,
      readElapsedMs: 0,
      layoutElapsedUs: 0,
    };
    pages = new Map([[0, initial]]);
    order.push(0);
    highWater = allCommits.length;
    complete = !hasMoreCommits;
    const observer = new ResizeObserver(() => {
      viewport = list.clientHeight;
    });
    observer.observe(list);
    return () => {
      disposed = true;
      observer.disconnect();
    };
  });
  const start = $derived(Math.max(0, Math.floor(top / HEIGHT) - OVERSCAN));
  const end = $derived(
    Math.min(highWater, Math.ceil((top + viewport) / HEIGHT) + OVERSCAN),
  );
  const extent = $derived(highWater + (complete ? 0 : SIZE));
  const width = $derived(
    Math.max(
      graphWidth(2),
      ...[...pages.values()].flatMap((p) =>
        p.graph.map((r) => graphWidth(r.width)),
      ),
    ),
  );
  // Scans at most the four cached pages, only when pages or selection change.
  const selectedIndex = $derived.by(() => {
    if (!selectedOid) return null;
    for (const page of pages.values()) {
      const local = page.commits.findIndex((c) => c.oid === selectedOid);
      if (local >= 0) return page.offset + local;
    }
    return null;
  });
  const selectedInView = $derived(
    selectedIndex !== null &&
      selectedIndex * HEIGHT >= top - 1 &&
      (selectedIndex + 1) * HEIGHT <= top + viewport + 1,
  );
  const visible = $derived.by(() => {
    const result: { commit: Commit; row: GraphRow }[] = [];
    const indices = Array.from(
      { length: Math.max(0, end - start) },
      (_, i) => start + i,
    );
    if (focused !== null && (focused < start || focused >= end))
      indices.push(focused);
    for (const index of indices) {
      const offset = Math.floor(index / SIZE) * SIZE,
        p = pages.get(offset),
        local = index - offset;
      if (p?.commits[local] && p.graph[local])
        result.push({ commit: p.commits[local], row: p.graph[local] });
    }
    return result;
  });
  async function ensurePage(offset: number) {
    if (disposed || loading || pageLoading || pages.has(offset)) return;
    pageLoading = true;
    try {
      const page = await fetchpage(offset);
      if (disposed) return;
      const next = new Map(pages);
      next.set(offset, page);
      const previous = order.indexOf(offset);
      if (previous >= 0) order.splice(previous, 1);
      order.push(offset);
      const focusPage =
        focused === null ? -1 : Math.floor(focused / SIZE) * SIZE;
      const firstVisiblePage = Math.floor(start / SIZE) * SIZE;
      const lastVisiblePage =
        Math.floor(Math.max(start, end - 1) / SIZE) * SIZE;
      while (next.size > 4) {
        const candidate = order.find(
          (key) =>
            key !== offset &&
            key !== focusPage &&
            (key < firstVisiblePage || key > lastVisiblePage),
        );
        if (candidate === undefined) break;
        next.delete(candidate);
        order.splice(order.indexOf(candidate), 1);
      }
      pages = next;
      highWater = Math.max(highWater, page.offset + page.commits.length);
      if (page.complete) complete = true;
    } catch (cause) {
      if (!disposed) pageError = readableError(cause).message;
    } finally {
      if (!disposed) pageLoading = false;
    }
  }
  $effect(() => {
    if (!pages.size || loading || pageLoading || pageError) return;
    const first =
      Math.floor(Math.min(start, Math.max(0, highWater - 1)) / SIZE) * SIZE;
    const last = Math.floor(Math.max(0, end - 1) / SIZE) * SIZE;
    for (let offset = first; offset <= last; offset += SIZE) {
      if (!pages.has(offset)) {
        void ensurePage(offset);
        return;
      }
    }
    if (!complete && top + viewport + OVERSCAN * HEIGHT >= highWater * HEIGHT)
      void ensurePage(highWater);
  });
  const dayFormatter = new Intl.DateTimeFormat('pt-BR', {
    day: '2-digit',
    month: '2-digit',
    year: 'numeric',
  });
  const fullFormatter = new Intl.DateTimeFormat('pt-BR', {
    dateStyle: 'medium',
    timeStyle: 'short',
  });
  function date(value: string, formatter = dayFormatter) {
    const d = new Date(value);
    return Number.isNaN(+d) ? value : formatter.format(d);
  }
  function matches(commit: Commit) {
    return `${commit.summary} ${commit.authorName} ${commit.oid}`
      .toLocaleLowerCase()
      .includes(query.toLocaleLowerCase());
  }
  function refNames(refs: CommitRef[]) {
    return refs.map((r) => (r.kind === 'tag' ? `tag ${r.name}` : r.name));
  }
  // Moves the viewport only as far as needed; a visible row never jumps.
  function scrollToIndex(index: number, center = false) {
    const rowTop = index * HEIGHT,
      rowBottom = rowTop + HEIGHT,
      height = list.clientHeight;
    if (center) list.scrollTop = Math.max(0, rowTop - (height - HEIGHT) / 2);
    else if (rowTop < list.scrollTop) list.scrollTop = rowTop;
    else if (rowBottom > list.scrollTop + height)
      list.scrollTop = rowBottom - height;
    top = list.scrollTop;
  }
  async function focusRow(oid: string) {
    await tick();
    document.getElementById(`commit-${oid}`)?.focus({ preventScroll: true });
  }
  export async function revealSelected() {
    if (!selectedOid) return;
    let index = selectedIndex;
    if (index === null) {
      await ensurePage(selectedOffset);
      const local =
        pages
          .get(selectedOffset)
          ?.commits.findIndex((c) => c.oid === selectedOid) ?? -1;
      if (local < 0) return;
      index = selectedOffset + local;
    }
    focused = index;
    scrollToIndex(index, true);
    await focusRow(selectedOid);
  }
  async function historyKey(event: KeyboardEvent, index: number) {
    const pageStep = Math.max(1, Math.floor(viewport / HEIGHT) - 1);
    let target =
      event.key === 'ArrowDown'
        ? index + 1
        : event.key === 'ArrowUp'
          ? index - 1
          : event.key === 'Home'
            ? 0
            : event.key === 'End'
              ? highWater - 1
              : event.key === 'PageDown'
                ? index + pageStep
                : event.key === 'PageUp'
                  ? index - pageStep
                  : null;
    if (target === null) return;
    event.preventDefault();
    target = Math.max(0, Math.min(highWater - 1, target));
    const offset = Math.floor(target / SIZE) * SIZE;
    if (!pages.has(offset)) await ensurePage(offset);
    const page = pages.get(offset);
    const commit = page?.commits[target - offset];
    if (!commit) return;
    focused = target;
    scrollToIndex(target);
    onselect(commit, offset, page.graph[target - offset]?.refs ?? []);
    await focusRow(commit.oid);
  }
</script>

<section
  class="panel history-panel graph-history"
  style={`--graph-width:${width}px;--row-height:${HEIGHT}px`}
  aria-labelledby="history-title"
>
  <div class="history-toolbar">
    <h2 id="history-title">
      <Icon name="history" size={16} />Histórico
      <span class="small-badge" title="Commits carregados"
        >{highWater}{complete ? '' : '+'}</span
      >
    </h2>
    <label class="search-box"
      ><Icon name="search" size={14} /><input
        type="search"
        aria-label="Destacar commits nas páginas em cache sem alterar a topologia"
        placeholder="Destacar mensagem, autor ou hash…"
        bind:value={query}
      /></label
    >
    <button
      class="toolbar-button"
      onclick={revealSelected}
      disabled={!selectedOid || selectedInView}
      title="Rolar até o commit selecionado"
      aria-label="Ir para o commit selecionado"
      ><Icon name="target" size={14} /><span>Selecionado</span></button
    >
  </div>
  <div class="history-columns" aria-hidden="true">
    <span>Grafo</span><span>Referências · mensagem</span><span
      class="column-author">Autor</span
    ><span class="column-date">Data</span><span class="column-oid">Commit</span>
  </div>
  <div
    class="history-list"
    bind:this={list}
    onscroll={() => {
      top = list.scrollTop;
    }}
    role="group"
    aria-label="Histórico paginado de commits"
    aria-busy={loading || pageLoading}
    data-cached-pages={pages.size}
    data-loaded={highWater}
    data-row-height={HEIGHT}
  >
    <div
      class="history-surface"
      style={`height:${extent * HEIGHT}px;min-width:${width + 280}px`}
    >
      {#each visible as { commit, row } (commit.oid)}
        <button
          id={`commit-${commit.oid}`}
          class="commit-row"
          class:selected={selectedOid === commit.oid}
          class:merge={commit.parents.length > 1}
          class:search-match={!!query && matches(commit)}
          class:search-muted={!!query && !matches(commit)}
          style={`transform:translateY(${row.index * HEIGHT}px)`}
          disabled={loading}
          aria-pressed={selectedOid === commit.oid}
          aria-label={`${row.index + 1}${complete ? ` de ${highWater}` : ''}: ${commit.summary}; ${commit.authorName}; ${date(commit.authoredAt, fullFormatter)}; ${commit.parents.length > 1 ? `merge de ${commit.parents.length} pais` : `${commit.parents.length} pai`}; ${commit.oid.slice(0, 8)}${row.refs.length ? `; referências: ${refNames(row.refs).join(', ')}` : ''}`}
          title={`${commit.summary}\n${commit.authorName} · ${date(commit.authoredAt, fullFormatter)}\n${commit.oid}\nPais: ${commit.parents.join(', ') || 'nenhum (raiz/fronteira shallow)'}${row.refs.length ? `\n${refNames(row.refs).join(' · ')}` : ''}`}
          onclick={() =>
            onselect(commit, Math.floor(row.index / SIZE) * SIZE, row.refs)}
          onfocus={() => {
            focused = row.index;
          }}
          oncontextmenu={(event) => oncontext(event, commit)}
          onkeydown={(event) => {
            if (
              event.key === 'ContextMenu' ||
              (event.shiftKey && event.key === 'F10')
            )
              oncontext(event, commit);
            else historyKey(event, row.index);
          }}
          data-index={row.index}
          data-lane={row.lane}
        >
          <CommitGraph {row} merge={commit.parents.length > 1} />
          <span class="commit-subject"
            >{#if row.refs.length}<span class="commit-labels"
                ><RefBadges refs={row.refs} limit={2} /></span
              >{/if}<strong>{commit.summary || '(sem mensagem)'}</strong></span
          >
          <span class="commit-author">{commit.authorName}</span><time
            class="commit-date"
            datetime={commit.authoredAt}>{date(commit.authoredAt)}</time
          ><code>{commit.oid.slice(0, 8)}</code>
        </button>
      {/each}
      {#if !highWater && complete}<div class="panel-empty">
          <Icon name="history" size={30} /><strong
            >A história ainda vai começar</strong
          >
          <p>Este repositório ainda não tem commits alcançáveis.</p>
        </div>{/if}
    </div>
  </div>
  {#if pageError}<div class="history-error" role="alert">
      <span>{pageError}</span><button class="text-button" onclick={onrefresh}
        >Atualizar repositório</button
      >
    </div>{/if}
  <div class="panel-footer" role="status">
    <span
      >{pageLoading
        ? 'Carregando página…'
        : complete
          ? `${highWater} commits · fim do histórico`
          : `${highWater} commits percorridos · continue rolando`}</span
    >{#if query}<span>Destaque apenas nas páginas em cache</span>{/if}
  </div>
</section>
