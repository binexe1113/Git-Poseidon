<script lang="ts">
  import { onMount } from 'svelte';
  let {
    label = 'Branches locais',
    x,
    y,
    items,
    onclose,
  }: {
    label?: string;
    x: number;
    y: number;
    items: { label: string; disabled?: boolean; pick: () => void }[];
    onclose: () => void;
  } = $props();
  let dialog: HTMLDialogElement;
  onMount(() => {
    dialog.showModal();
    const r = dialog.getBoundingClientRect();
    dialog.style.left =
      Math.max(8, Math.min(x, innerWidth - r.width - 8)) + 'px';
    dialog.style.top =
      Math.max(8, Math.min(y, innerHeight - r.height - 8)) + 'px';
    dialog.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
  });
  function key(event: KeyboardEvent) {
    const buttons = [
      ...dialog.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'),
    ];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      buttons[
        event.key === 'Home'
          ? 0
          : event.key === 'End'
            ? buttons.length - 1
            : (i + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) %
              buttons.length
      ]?.focus();
    }
  }
</script>

<dialog
  bind:this={dialog}
  class="branch-menu"
  aria-label={`Operações: ${label}`}
  oncancel={(event) => {
    event.preventDefault();
    onclose();
  }}
  onkeydown={key}
  onclick={(event) => {
    if (event.target === dialog) onclose();
  }}
>
  <div role="menu" aria-label={label}>
    {#each items as item}<button
        type="button"
        role="menuitem"
        disabled={item.disabled}
        onclick={item.pick}>{item.label}</button
      >{/each}
  </div>
</dialog>

<style>
  .branch-menu {
    position: fixed;
    margin: 0;
    padding: 5px;
    max-width: min(420px, 95vw);
    max-height: 75vh;
    overflow: auto;
    background: var(--panel);
    color: var(--text-strong);
    border: 1px solid var(--line);
    box-shadow: 0 8px 30px #0008;
    border-radius: 5px;
  }
  .branch-menu::backdrop {
    background: transparent;
  }
  button {
    display: block;
    width: 100%;
    border: 0;
    background: transparent;
    text-align: left;
    padding: 9px;
    color: inherit;
    overflow-wrap: anywhere;
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: #304665;
  }
</style>
