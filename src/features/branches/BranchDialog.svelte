<script lang="ts">
  import { onMount, tick } from 'svelte';
  import * as api from '../../lib/ipc/client';
  import type {
    BranchAction,
    BranchPlan,
    BranchChange,
  } from '../../lib/ipc/bindings';
  let {
    sessionId,
    generation,
    intent,
    onclose,
    onchanged,
    onbusy,
  }: {
    sessionId: number;
    generation: number;
    intent: BranchAction;
    onclose: () => void;
    onchanged: (result: BranchChange) => Promise<void>;
    onbusy: (busy: boolean) => void;
  } = $props();
  let dialog: HTMLDialogElement;
  let name = $state('');
  let plan = $state.raw<BranchPlan | null>(null);
  let busy = $state(false);
  let failure = $state('');
  let finished = $state(false);
  const title = {
    create: 'Criar branch',
    switch: 'Trocar de branch',
    rename: 'Renomear branch',
    delete: 'Excluir branch integrada',
  };
  onMount(() => {
    name = intent.kind === 'rename' ? intent.newName : intent.name;
    dialog.showModal();
    dialog.querySelector<HTMLElement>('input, button')?.focus();
  });
  function action(): BranchAction {
    if (intent.kind === 'create') return { ...intent, name };
    if (intent.kind === 'rename') return { ...intent, newName: name };
    return intent;
  }
  async function review() {
    busy = true;
    failure = '';
    plan = null;
    try {
      plan = (await api.prepareBranch(sessionId, generation, action())).data;
      await tick();
      dialog.querySelector<HTMLButtonElement>('[data-cancel]')?.focus();
    } catch (cause) {
      failure = api.readableError(cause).message;
    } finally {
      busy = false;
    }
  }
  async function trust(hooks: boolean, value = true) {
    busy = true;
    failure = '';
    try {
      if (hooks) await api.setBranchHookTrust(sessionId, generation, value);
      else await api.setRepositoryTrust(sessionId, generation, true);
      await review();
    } catch (cause) {
      failure = api.readableError(cause).message;
    } finally {
      busy = false;
    }
  }
  async function submit() {
    if (!plan || plan.block || busy || finished) return;
    busy = true;
    onbusy(true);
    failure = '';
    try {
      const result = (
        await api.executeBranch(sessionId, generation, plan.token)
      ).data;
      // Consume confirmation for every outcome; never offer an automatic retry.
      plan = null;
      finished = true;
      failure = result.message;
      await onchanged(result);
    } catch (cause) {
      plan = null;
      failure =
        api.readableError(cause).message +
        ' Revise novamente; nenhuma tentativa automática foi feita.';
    } finally {
      busy = false;
      onbusy(false);
    }
  }
</script>

<dialog
  bind:this={dialog}
  class="index-dialog branch-dialog"
  aria-labelledby="branch-dialog-title"
  oncancel={(event) => {
    event.preventDefault();
    if (!busy) onclose();
  }}
>
  <form
    onsubmit={(event) => {
      event.preventDefault();
      void review();
    }}
  >
    <h2 id="branch-dialog-title">{title[intent.kind]}</h2>
    <div class="index-dialog-body">
      {#if intent.kind === 'create' || intent.kind === 'rename'}
        <label for="branch-name"
          >{intent.kind === 'rename'
            ? 'Novo nome da branch'
            : 'Nome da branch'}</label
        >
        <input
          id="branch-name"
          bind:value={name}
          disabled={busy || finished}
          oninput={() => {
            plan = null;
          }}
          required
          maxlength="1024"
          autocomplete="off"
          spellcheck="false"
        />
      {/if}
      {#if intent.kind === 'create'}<p>
          Base: <code>{intent.startOid.slice(0, 12)}</code>. Criar não troca a
          branch atual.
        </p>{:else}<p>Branch: <strong>{intent.name}</strong></p>{/if}
      {#if intent.kind === 'switch'}<p>
          A troca pode alterar arquivos e executar filtros e hooks autorizados.
          Não haverá descarte, stash ou checkout forçado.
        </p>{/if}
      {#if intent.kind === 'delete'}<p>
          Somente branches integradas ao HEAD atual e aceitas por <code
            >git branch -d</code
          >. Sem exclusão forçada.
        </p>{/if}
      {#if plan}
        {#if plan.dirtyPaths.length}<p>
            Arquivos locais a preservar ({plan.dirtyPaths.length}):
          </p>
          <ul>
            {#each plan.dirtyPaths.slice(0, 8) as path}<li>
                <code>{path}</code>
              </li>{/each}
          </ul>{/if}
        {#if plan.filters.length}<p>
            Filtros configurados: {plan.filters.join(', ')}. Somente as
            definições autorizadas podem ser executadas. Git LFS deve ser usado
            no terminal.
          </p>{/if}
        {#if plan.hooks.length}<p>
            Hooks que podem executar: {plan.hooks.join(', ')}. São programas
            externos, podem alterar arquivos e produzir efeitos que não podem
            ser desfeitos automaticamente. A autorização vincula-se ao conteúdo
            atual. A execução usa cópias em um contexto Git temporário; hooks
            que dependem da localização original ou gerenciam outros metadados
            devem ser usados no terminal.
          </p>{/if}
        {#if plan.block}<p role="status">{plan.block}</p>{/if}
        {#if !plan.trusted}<button
            type="button"
            class="toolbar-button"
            disabled={busy}
            onclick={() => trust(false)}>Confiar neste repositório</button
          >{:else if plan.hooks.length}
          <button
            type="button"
            class="toolbar-button"
            disabled={busy}
            onclick={() => trust(true, !plan!.hooksTrusted)}
            >{plan.hooksTrusted
              ? 'Revogar hooks de branches'
              : 'Autorizar hooks de branches'}</button
          >
        {/if}
      {/if}
      {#if busy}<p role="status">Verificando estado do Git…</p>{/if}
      {#if failure}<p role="status" class="branch-result">{failure}</p>{/if}
    </div>
    <div class="index-dialog-actions">
      <button
        data-cancel
        type="button"
        class="toolbar-button"
        disabled={busy}
        onclick={onclose}>{finished ? 'Fechar' : 'Cancelar'}</button
      >
      {#if !finished}
        {#if plan && !plan.block}<button
            type="button"
            class="primary-button"
            disabled={busy}
            onclick={submit}
            >Confirmar {intent.kind === 'switch'
              ? 'troca'
              : intent.kind === 'delete'
                ? 'exclusão'
                : intent.kind === 'rename'
                  ? 'renomeação'
                  : 'criação'}</button
          >
        {:else}<button
            type="submit"
            class="primary-button"
            disabled={busy || !name}>Revisar operação</button
          >{/if}
      {/if}
    </div>
  </form>
</dialog>

<style>
  input {
    width: 100%;
    margin: 8px 0;
    padding: 9px;
    color: var(--text-strong);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 4px;
  }
  .branch-dialog {
    max-height: 90vh;
    overflow: auto;
  }
  ul {
    max-height: 130px;
    overflow: auto;
    overflow-wrap: anywhere;
  }
  .branch-result {
    white-space: pre-wrap;
  }
</style>
