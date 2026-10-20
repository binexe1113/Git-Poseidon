<script lang="ts">
  import { onMount } from 'svelte';
  let {
    onopen,
    oncancel,
  }: {
    onopen: (path: string) => void;
    oncancel: () => void;
  } = $props();
  let dialog: HTMLDialogElement;
  let input: HTMLInputElement;
  let path = $state('');
  onMount(() => {
    dialog.showModal();
    input.focus();
  });
</script>

<dialog
  bind:this={dialog}
  class="index-dialog"
  aria-labelledby="repository-path-title"
  oncancel={(event) => {
    event.preventDefault();
    oncancel();
  }}
>
  <form
    onsubmit={(event) => {
      event.preventDefault();
      if (path) onopen(path);
    }}
  >
    <h2 id="repository-path-title">Abrir repositório por caminho</h2>
    <div class="index-dialog-body">
      <p>
        Informe o caminho completo de um repositório existente. A abertura
        mantém a inspeção protegida; não concede confiança para escritas.
      </p>
      <label for="repository-path">Caminho do repositório</label>
      <input
        id="repository-path"
        bind:this={input}
        bind:value={path}
        required
        autocomplete="off"
        spellcheck="false"
      />
    </div>
    <div class="index-dialog-actions">
      <button type="button" class="toolbar-button" onclick={oncancel}
        >Cancelar</button
      >
      <button type="submit" class="primary-button" disabled={!path}
        >Abrir caminho</button
      >
    </div>
  </form>
</dialog>

<style>
  input {
    width: 100%;
    margin-top: 8px;
    padding: 10px;
    color: var(--text-strong);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 6px;
  }
</style>
