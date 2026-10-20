<script lang="ts">
  import ChangeBadge from './ChangeBadge.svelte';
  import Icon from '../../lib/ui/Icon.svelte';
  import type { ChangedFile } from '../../lib/ipc/bindings';
  import type { FileAction } from './source';

  let {
    files,
    label,
    selectedIndex,
    onopen,
    action,
    partial,
    highlight,
  }: {
    files: ChangedFile[];
    label: string;
    selectedIndex: number | null;
    onopen: (index: number) => void;
    action?: FileAction;
    /** Paths that also have changes in the other index group. */
    partial?: Set<string>;
    /** Path that just moved into this list: briefly marked. */
    highlight?: string | null;
  } = $props();
  const STEP = 200;
  let shown = $state(STEP);
  let active = $state(0);
  let list: HTMLDivElement | undefined = $state();
  const visible = $derived(files.slice(0, shown));
  $effect(() => {
    const position = files.findIndex((f) => f.index === selectedIndex);
    if (position >= 0) {
      shown = Math.max(shown, Math.ceil((position + 1) / STEP) * STEP);
      active = position;
    } else active = Math.max(0, Math.min(active, files.length - 1));
  });
  function split(path: string) {
    const slash = path.lastIndexOf('/');
    return slash < 0
      ? { dir: '', name: path }
      : { dir: path.slice(0, slash + 1), name: path.slice(slash + 1) };
  }
  // Roving tabindex: one Tab stop for the whole list (plus the active row's action),
  // arrows move within it.
  function key(event: KeyboardEvent, position: number, file: ChangedFile) {
    if (
      action &&
      event.key === action.key &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      event.preventDefault();
      if (!action.disabled && action.busy === null) action.run(file.index);
      return;
    }
    const next =
      event.key === 'ArrowDown'
        ? position + 1
        : event.key === 'ArrowUp'
          ? position - 1
          : event.key === 'Home'
            ? 0
            : event.key === 'End'
              ? visible.length - 1
              : null;
    if (next === null) return;
    event.preventDefault();
    active = Math.max(0, Math.min(visible.length - 1, next));
    list
      ?.querySelectorAll<HTMLButtonElement>('.file-item')
      [active]?.focus({ preventScroll: false });
  }
</script>

<div class="file-items" role="list" aria-label={label} bind:this={list}>
  {#each visible as file, position (JSON.stringify([file.path, file.oldPath]))}
    {@const parts = split(file.path)}
    {@const isPartial = partial?.has(file.path) ?? false}
    <div
      role="listitem"
      class="file-row"
      class:moved={highlight === file.path}
      class:busy={action?.busy === file.index}
    >
      <button
        class="file-item"
        class:selected={selectedIndex === file.index}
        tabindex={position === active ? 0 : -1}
        aria-current={selectedIndex === file.index ? 'true' : undefined}
        title={(file.oldPath ? `${file.oldPath} → ${file.path}` : file.path) +
          (isPartial ? ' · também tem alterações no outro grupo' : '')}
        onfocus={() => (active = position)}
        onclick={() => {
          active = position;
          onopen(file.index);
        }}
        onkeydown={(event) => key(event, position, file)}
      >
        <ChangeBadge kind={file.kind} />
        <span class="file-name"
          ><span class="file-dir">{parts.dir}</span><strong>{parts.name}</strong
          ></span
        >
        {#if isPartial}<span class="file-partial">parcial</span>{/if}
        {#if file.binary}<span class="file-binary">bin</span
          >{:else if file.additions !== null}<span class="file-stats"
            ><span class="stat-add">+{file.additions}</span><span
              class="stat-del">−{file.deletions}</span
            ></span
          >{/if}
      </button>
      {#if action}<button
          class="row-action"
          tabindex={position === active ? 0 : -1}
          disabled={!!action.disabled || action.busy !== null}
          aria-label={`${action.label}: ${file.path}`}
          title={action.disabled ??
            `${action.label} (${action.key})${isPartial && action.icon === 'plus' ? ' — substitui a versão preparada' : ''}`}
          onfocus={() => (active = position)}
          onclick={() => action.run(file.index)}
          >{#if action.busy === file.index}<span class="spinning"
              ><Icon name="refresh" size={13} /></span
            >{:else}<Icon name={action.icon} size={13} />{/if}</button
        >{/if}
    </div>
  {/each}
</div>
{#if files.length > shown}<button
    class="text-button more-files"
    onclick={() => (shown += STEP)}
    >Mostrar mais {Math.min(STEP, files.length - shown)} de {files.length -
      shown}</button
  >{/if}
