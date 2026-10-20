<script lang="ts">
  import Icon from '../../lib/ui/Icon.svelte';
  import type { CommitRef } from '../../lib/ipc/bindings';
  let { refs, limit = Infinity }: { refs: CommitRef[]; limit?: number } =
    $props();
  const kinds: Record<string, { icon: string; label: string }> = {
    branch: { icon: 'branch', label: 'Branch local' },
    remote: { icon: 'cloud', label: 'Branch remota' },
    tag: { icon: 'tag', label: 'Tag' },
    head: { icon: 'commit', label: 'HEAD destacado' },
  };
  // HEAD is shown inside the checked-out branch; a detached HEAD stays alone.
  const items = $derived.by(() => {
    const head = refs.some((r) => r.kind === 'head');
    const named = refs.filter((r) => r.kind !== 'head');
    const attached = named.some((r) => r.current);
    const list = named.map((r) => ({ ...r, head: head && r.current }));
    if (head && !attached)
      list.unshift({ name: 'HEAD', kind: 'head', current: true, head: true });
    return list.sort((a, b) => Number(b.head) - Number(a.head));
  });
  const hidden = $derived(items.slice(limit));
</script>

{#each items.slice(0, limit) as ref}<span
    class="ref-badge kind-{ref.kind}"
    class:current={ref.current}
    class:tag={ref.kind === 'tag'}
    title={`${kinds[ref.kind]?.label ?? ref.kind}: ${ref.name}${ref.head && ref.kind !== 'head' ? ' · HEAD' : ''}`}
    ><Icon name={kinds[ref.kind]?.icon ?? 'branch'} size={11} /><span
      class="ref-name">{ref.name}</span
    >{#if ref.head && ref.kind !== 'head'}<span class="ref-head">HEAD</span
      >{/if}</span
  >{/each}{#if hidden.length}<span
    class="ref-more"
    title={hidden.map((r) => r.name).join('\n')}>+{hidden.length}</span
  >{/if}
