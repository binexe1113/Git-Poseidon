<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../../lib/ui/Icon.svelte';
  let {
    kind,
    path,
    drivers,
    onconfirm,
    oncancel,
  }: {
    /** `trust`: first write in an untrusted repository; `partial`: replace a staged version. */
    kind: 'trust' | 'partial';
    path: string;
    drivers: string[];
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();
  let dialog: HTMLDialogElement | undefined = $state();
  let cancel: HTMLButtonElement | undefined = $state();
  onMount(() => {
    dialog?.showModal();
    // The safe choice has the initial focus.
    cancel?.focus();
  });
</script>

<dialog
  bind:this={dialog}
  class="index-dialog"
  aria-labelledby="index-dialog-title"
  aria-describedby="index-dialog-body"
  oncancel={(event) => {
    event.preventDefault();
    oncancel();
  }}
>
  {#if kind === 'trust'}
    <h2 id="index-dialog-title">
      <Icon name="shield" size={18} />Confiar neste repositório?
    </h2>
    <div id="index-dialog-body" class="index-dialog-body">
      <p>
        Preparar ou retirar <strong>{path}</strong> grava no index deste
        repositório. Ao preparar, o Git aplica os filtros configurados — como os
        do
        <strong>Git LFS</strong> — e eles
        <strong>podem executar programas externos</strong> com as suas permissões.
      </p>
      {#if drivers.length}<p>
          Filtros configurados agora: <code>{drivers.join(', ')}</code>.
        </p>{:else}<p>
          Nenhum filtro está configurado agora, mas a configuração e os arquivos
          <code>.gitattributes</code> podem passar a ativar algum depois.
        </p>{/if}
      <p>
        Confiar <strong>não é um sandbox</strong>: o Poseidon não isola esses
        programas. A confiança vale só para esta cópia do repositório (a pasta
        <code>.git</code> atual), fica gravada neste computador e pode ser revogada
        no painel Alterações locais. Worktrees com o mesmo diretório Git comum compartilham
        essa confiança. Após confiar, confira a lista atualizada e acione o arquivo
        novamente. Sem confiança, a inspeção continua disponível e nenhum filtro é
        executado.
      </p>
    </div>
    <div class="index-dialog-actions">
      <button class="toolbar-button" bind:this={cancel} onclick={oncancel}
        >Cancelar</button
      ><button class="primary-button" onclick={onconfirm}
        >Confiar e atualizar lista</button
      >
    </div>
  {:else}
    <h2 id="index-dialog-title">
      <Icon name="alert" size={18} />Substituir a versão preparada?
    </h2>
    <div id="index-dialog-body" class="index-dialog-body">
      <p>
        <strong>{path}</strong> tem alterações preparadas e outras não preparadas.
        Preparar o arquivo inteiro coloca no index a versão atual do working tree:
        a versão preparada hoje deixa de estar no index.
      </p>
      <p>O conteúdo do working tree não muda.</p>
    </div>
    <div class="index-dialog-actions">
      <button class="toolbar-button" bind:this={cancel} onclick={oncancel}
        >Cancelar</button
      ><button class="primary-button" onclick={onconfirm}
        >Preparar arquivo inteiro</button
      >
    </div>
  {/if}
</dialog>
