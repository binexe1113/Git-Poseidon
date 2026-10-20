<script lang="ts">
  import FileList from './FileList.svelte';
  import type { CommitFiles } from '../../lib/ipc/bindings';
  let {
    files,
    loading,
    error,
    selectedIndex,
    onopen,
    onretry,
  }: {
    files: CommitFiles | null;
    loading: boolean;
    error: string | null;
    selectedIndex: number | null;
    onopen: (index: number) => void;
    onretry: () => void;
  } = $props();
</script>

<section
  class="commit-files"
  aria-labelledby="commit-files-title"
  data-oid={files?.oid}
>
  <h4 class="message-heading files-heading" id="commit-files-title">
    <span>Arquivos alterados</span>
    {#if files}<span class="files-summary"
        >{files.totalFiles}
        <span class="stat-add">+{files.additions}</span>
        <span class="stat-del">−{files.deletions}</span></span
      >{/if}
  </h4>
  {#if files}
    <p class="diff-base">
      {#if files.base.kind === 'emptyTree'}
        Comparado com a árvore vazia (commit inicial ou fronteira shallow).
      {:else if files.base.parentCount > 1}
        <strong>Merge:</strong> comparado ao 1º pai
        <code title={files.base.oid}>{files.base.oid.slice(0, 8)}</code>
        (de {files.base.parentCount}).
      {:else}
        Comparado ao pai
        <code title={files.base.oid}>{files.base.oid.slice(0, 8)}</code>.
      {/if}
    </p>
  {/if}
  {#if loading && !files}<p class="muted" role="status">
      Listando arquivos…
    </p>{:else if error}<p class="detail-error" role="alert">{error}</p>
    <button class="text-button" onclick={onretry}>Tentar novamente</button
    >{:else if files && !files.files.length}<p class="muted">
      Nenhum arquivo difere da base (merge sem alterações em relação ao 1º pai
      ou commit vazio).
    </p>{:else if files}
    <FileList
      files={files.files}
      label="Arquivos deste commit"
      {selectedIndex}
      {onopen}
    />
    {#if files.hasMoreFiles}<p class="limit-note">
        Listando {files.files.length} de {files.totalFiles} arquivos. Os totais +/−
        abrangem todos.
      </p>{/if}
  {/if}
  {#if files && !loading}<p class="sr-only" role="status">
      {files.totalFiles} arquivos alterados
    </p>{/if}
</section>
