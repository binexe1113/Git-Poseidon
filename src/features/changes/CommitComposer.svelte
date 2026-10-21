<script lang="ts">
  import { tick, untrack } from 'svelte';
  import * as api from '../../lib/ipc/client';
  import type {
    CommitPlan,
    CommitCreation,
    WorkingChanges,
  } from '../../lib/ipc/bindings';
  let {
    sessionId,
    generation,
    working,
    busy,
    onbusy,
    onresult,
  }: {
    sessionId: number;
    generation: number;
    working: WorkingChanges | null;
    busy: boolean;
    onbusy: (busy: boolean) => void;
    onresult: (result: CommitCreation) => Promise<void>;
  } = $props();
  let subject = $state('');
  let body = $state('');
  let expanded = $state(false);
  let plan = $state.raw<CommitPlan | null>(null);
  let reading = $state(false);
  let running = $state(false);
  let notice = $state<{ error: boolean; text: string } | null>(null);
  let planError = $state<string | null>(null);
  let modal = $state<'commit' | 'hooks' | null>(null);
  let dialog: HTMLDialogElement | undefined = $state();
  let cancel: HTMLButtonElement | undefined = $state();
  let reviewButton: HTMLButtonElement | undefined = $state();
  let epoch = 0;
  let retry = $state(0);
  $effect(() => {
    const scope = [sessionId, generation, working?.revision, busy, retry];
    if (scope[2] && !busy) untrack(() => void readPlan());
  });
  async function readPlan() {
    if (!working) return null;
    const e = ++epoch;
    reading = true;
    try {
      const response = await api.prepareCommit(
        sessionId,
        generation,
        working.revision,
      );
      if (
        e !== epoch ||
        response.sessionId !== sessionId ||
        response.generation !== generation
      )
        return null;
      plan = response.data;
      planError = null;
      return response.data;
    } catch (cause) {
      if (e === epoch) {
        plan = null;
        planError = api.readableError(cause).message;
      }
      return null;
    } finally {
      if (e === epoch) reading = false;
    }
  }
  async function review(kind: 'commit' | 'hooks') {
    if (busy || running || reading) return;
    onbusy(true);
    const current = await readPlan();
    if (!current || (kind === 'commit' && current.block)) {
      onbusy(false);
      return;
    }
    modal = kind;
    await tick();
    dialog?.showModal();
    cancel?.focus();
  }
  async function close() {
    dialog?.close();
    modal = null;
    onbusy(false);
    await tick();
    reviewButton?.focus();
  }
  async function hookTrust(trusted: boolean) {
    running = true;
    onbusy(true);
    try {
      await api.setCommitHookTrust(sessionId, generation, trusted);
      notice = {
        error: false,
        text: trusted
          ? 'Hooks autorizados. Revise o commit novamente.'
          : 'Autorização de hooks revogada.',
      };
    } catch (cause) {
      notice = { error: true, text: api.readableError(cause).message };
    } finally {
      running = false;
      await close();
      retry++;
    }
  }
  async function submit() {
    if (!plan || running || !subject.trim()) return;
    running = true;
    const message =
      subject.trim() +
      (body.trim() ? '\n\n' + body.replace(/\r\n/g, '\n').trim() : '') +
      '\n';
    dialog?.close();
    modal = null;
    try {
      const response = await api.createCommit(
        sessionId,
        generation,
        plan.token,
        message,
      );
      notice = {
        error: response.data.outcome !== 'created',
        text: response.data.message,
      };
      if (response.data.outcome === 'created') {
        subject = '';
        body = '';
        expanded = false;
      }
      await onresult(response.data);
    } catch (cause) {
      notice = {
        error: true,
        text:
          api.readableError(cause).message +
          ' Rascunho preservado; nenhuma repetição automática.',
      };
    } finally {
      running = false;
      onbusy(false);
      retry++;
      await tick();
      reviewButton?.focus();
    }
  }
</script>

<section
  class="commit-composer"
  aria-labelledby="compose-title"
  aria-busy={reading || running}
>
  <div class="compose-heading">
    <h3 id="compose-title">Criar commit</h3>
    <span>{plan?.stagedCount ?? working?.totalStaged ?? 0} preparados</span>
  </div>
  <p class="compose-hint">
    Revise os arquivos em Preparadas e abra o diff antes de confirmar.
  </p>
  <label for="commit-subject">Assunto</label>
  <input
    id="commit-subject"
    bind:value={subject}
    placeholder="Descreva a alteração"
    disabled={running}
    maxlength="500"
  />
  <button
    class="text-button compose-description"
    aria-expanded={expanded}
    onclick={() => (expanded = !expanded)}
    >Descrição {expanded ? '−' : '+'}</button
  >
  {#if expanded}<label for="commit-body">Descrição opcional</label><textarea
      id="commit-body"
      bind:value={body}
      rows="4"
      disabled={running}
      placeholder="Contexto e detalhes da alteração"
    ></textarea>{/if}
  {#if plan}<div class="compose-identity">
      <span>Autor: {plan.author ?? 'não configurado'}</span
      >{#if plan.committer !== plan.author}<span
          >Committer: {plan.committer ?? 'não configurado'}</span
        >{/if}<span class="compose-stats"
        >+{plan.additions} / −{plan.deletions} · {plan.headOid
          ? 'Commit normal'
          : 'Commit inicial'}</span
      >
    </div>
    {#if plan.hooks.length || plan.hooksTrusted}<div class="compose-hooks">
        <span
          >{plan.hooks.length} hooks ativos · {plan.hooksTrusted
            ? 'autorizados'
            : 'sem autorização'}</span
        >{#if plan.hooksTrusted}<button
            class="text-button"
            disabled={busy || running}
            onclick={() => void hookTrust(false)}>Revogar hooks</button
          >{:else}<button
            class="text-button"
            disabled={busy || running || reading || !working?.trusted}
            onclick={() => void review('hooks')}>Autorizar hooks…</button
          >{/if}
      </div>{/if}
    {#if plan.block}<p
        class="compose-block"
        class:compose-empty={plan.stagedCount === 0}
        role="status"
      >
        {plan.block}
      </p>{/if}
  {/if}
  {#if planError}<p class="compose-block" role="status">{planError}</p>{/if}
  {#if notice}<p
      class="compose-notice"
      class:error={notice.error}
      role={notice.error ? 'alert' : 'status'}
    >
      {notice.text}
    </p>{/if}
  <button
    class="primary-button compose-submit"
    bind:this={reviewButton}
    disabled={busy ||
      running ||
      reading ||
      !subject.trim() ||
      !plan ||
      !!plan.block ||
      (!!plan.hooks.length && !plan.hooksTrusted)}
    onclick={() => void review('commit')}
    >{running
      ? 'Criando commit…'
      : reading
        ? 'Verificando index…'
        : 'Revisar commit'}</button
  >
  {#if !plan && !reading}<button
      class="text-button"
      disabled={busy || running}
      onclick={() => retry++}>Recarregar preparação</button
    >{/if}
</section>

{#if modal}<dialog
    bind:this={dialog}
    class="index-dialog commit-dialog"
    aria-labelledby="commit-confirm-title"
    oncancel={(e) => {
      e.preventDefault();
      if (!running) void close();
    }}
  >
    <h2 id="commit-confirm-title">
      {modal === 'hooks'
        ? 'Autorizar hooks deste repositório?'
        : 'Criar este commit?'}
    </h2>
    <div class="index-dialog-body">
      {#if modal === 'hooks'}<p>
          Hooks são programas externos, executados com suas permissões. Podem
          alterar arquivos, o index e a mensagem, acessar a rede ou executar
          outras ferramentas, incluindo Husky. A autorização é separada da
          confiança em filtros e vale para os worktrees do mesmo diretório Git
          comum.
        </p>
        <p>
          Diretório efetivo: <code>{plan?.hooksPath}</code>. Hooks ativos:
          <code>{plan?.hooks.join(', ')}</code>. A autorização vincula-se ao
          conteúdo dos hooks atuais; alterações exigem nova autorização. Você
          pode revogá-la aqui ou revogar toda a confiança do repositório.
        </p>
        <p>
          A execução usa cópias dos hooks em um contexto Git temporário. Hooks
          que dependem da localização original do script ou da pasta Git devem
          ser usados no terminal. Não há sandbox: dependências e efeitos dos
          programas autorizados não são isolados.
        </p>
        <p>
          Se um hook alterar o index antes do commit, a operação será recusada
          para revisão. Seus efeitos não serão desfeitos automaticamente.
        </p>{:else}<p><strong>{subject}</strong></p>
        {#if body.trim()}<pre class="commit-preview-body">{body}</pre>{/if}
        <p>
          {plan?.stagedCount} arquivos preparados · +{plan?.additions} / −{plan?.deletions}
        </p>
        <p>Autor: {plan?.author}</p>
        <p>
          O commit usará o index revisado. Não haverá stage automático. {plan
            ?.hooks.length
            ? 'Os hooks autorizados serão executados e poderão alterar arquivos ou a mensagem.'
            : 'Nenhum hook de repositório foi detectado na configuração revisada.'}
        </p>{/if}
    </div>
    <div class="index-dialog-actions">
      <button
        class="toolbar-button"
        bind:this={cancel}
        disabled={running}
        onclick={() => void close()}>Cancelar</button
      ><button
        class="primary-button commit-confirm"
        disabled={running}
        onclick={() =>
          modal === 'hooks' ? void hookTrust(true) : void submit()}
        >{modal === 'hooks'
          ? 'Autorizar hooks e revisar'
          : 'Criar commit'}</button
      >
    </div>
  </dialog>{/if}
